// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The redial rule against real sockets. The OLD server's identity is a pid
//! that WAS a process and is not one any more (a reaped child's), so an
//! in-process fake server — this test process's pid — plays the successor
//! exactly as a new aterm process would: a live server whose pid differs from
//! the one the exchange started with. The same rule across real aterm
//! processes is driven in `crates/aterm/tests/ctl_redial_live_headless.rs`.

use super::*;
use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;

/// A pid that names no process: a reaped child's.
#[cfg(unix)]
fn gone_pid() -> u32 {
    let mut child = std::process::Command::new("/usr/bin/true")
        .spawn()
        .expect("spawn /usr/bin/true");
    let pid = child.id();
    child.wait().expect("reap");
    pid
}

/// A scratch dir removed on drop (this crate is std-only: no tempfile crate).
#[cfg(unix)]
struct Scratch(PathBuf);

#[cfg(unix)]
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A short socket path in a fresh scratch dir under `/tmp` (sun_path is ~104
/// bytes, and the platform temp dir can be longer than that alone).
#[cfg(unix)]
fn scratch_sock(tag: &str) -> (Scratch, PathBuf) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let dir = PathBuf::from(format!("/tmp/{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).expect("scratch dir");
    let sock = dir.join("s.sock");
    (Scratch(dir), sock)
}

/// A scripted fake server on `listener`, in its own thread. It answers every
/// `version` probe `OK` (as a live aterm does — the redial's liveness test),
/// ignores a connection that hangs up without a request, answers the FIRST
/// other request (recorded in `seen`) with `reply` and closes that connection,
/// and stops on a `stop` request ([`stop`]). With `retire` set it stops right
/// after its reply and removes its socket file — the old process exiting.
#[cfg(unix)]
fn fake_server(
    listener: aterm_uds::CtlListener,
    path: PathBuf,
    reply: &'static str,
    retire: bool,
    seen: std::sync::mpsc::Sender<String>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || serve(&listener, &path, reply, retire, &seen))
}

/// [`fake_server`]'s body, for a caller already on its own thread.
#[cfg(unix)]
fn serve(
    listener: &aterm_uds::CtlListener,
    path: &std::path::Path,
    reply: &str,
    retire: bool,
    seen: &std::sync::mpsc::Sender<String>,
) {
    let mut served = false;
    loop {
        let Ok((conn, _)) = listener.accept() else {
            return;
        };
        let mut reader = std::io::BufReader::new(&conn);
        let mut request = String::new();
        if reader.read_line(&mut request).unwrap_or(0) == 0 {
            continue;
        }
        if request.starts_with("version") {
            let _ = (&conn).write_all(b"OK aterm test\n");
            continue;
        }
        if request.starts_with("stop") {
            return;
        }
        if served {
            continue;
        }
        let _ = seen.send(request);
        let _ = (&conn).write_all(reply.as_bytes());
        served = true;
        if retire {
            let _ = std::fs::remove_file(path);
            return;
        }
    }
}

/// End a [`fake_server`] that is still answering.
#[cfg(unix)]
fn stop(sock: &std::path::Path) {
    if let Ok(conn) = CtlStream::connect(sock) {
        let _ = (&conn).write_all(b"stop\n");
    }
}

/// Complete the subscribe handshake the way `exchange` does: send the request,
/// read the `OK subscribe` status line through the reader the relay goes on
/// using (which may already hold the frames behind it).
#[cfg(unix)]
fn handshake(stream: &CtlStream, reader: &mut BufReader<&CtlStream>, request: &str) {
    let mut writer = stream;
    writer.write_all(request.as_bytes()).expect("send");
    let mut status = String::new();
    reader.read_line(&mut status).expect("status");
    assert!(status.starts_with("OK subscribe"), "{status}");
}

#[test]
fn the_tracker_ends_a_session_only_on_every_targets_exited_frame() {
    let mut t = FrameTracker::new("subscribe @s-a,@s-b screen,events\n");
    t.feed(b"sub 1 s-a\nsub 2 s-b\nEVENT 1 exited\n");
    assert!(!t.session_over(), "one of two targets exited");
    t.feed(b"EVENT 2 exi");
    assert!(
        !t.session_over(),
        "a header split across reads is not a frame yet"
    );
    t.feed(b"ted\n");
    assert!(t.session_over());

    // A BODY that reads like the marker is skipped by its declared length.
    let mut t = FrameTracker::new("subscribe @s-a screen,bytes,events\n");
    t.feed(b"sub 1 s-a\nDELTA 1 seq=4 screen 2\nEVENT 1 exited\nrow two\n");
    assert!(!t.session_over(), "screen rows are rows, not frames");
    t.feed(b"BYTES 1 15\nEVENT 1 exit");
    t.feed(b"ed\n\n");
    assert!(!t.session_over(), "raw PTY bytes are a body, not a frame");
    t.feed(b"DELTA 1 seq=5 cells 3\nabc\nEVENT 1 exited\n");
    assert!(t.session_over(), "the real frame after the bodies");

    // `@*` follows the live roster: its sessions exiting never ends it.
    let mut t = FrameTracker::new("subscribe @* events\n");
    t.feed(b"sub 1 s-a\nEVENT 1 exited\n");
    assert!(!t.session_over());

    // A new leg forgets the old locals.
    let mut t = FrameTracker::new("subscribe @s-a events\n");
    t.feed(b"sub 1 s-a\nEVENT 1 exited\n");
    t.new_leg();
    assert!(!t.session_over());
    // Unframeable input can only ever mean "cannot say".
    let mut t = FrameTracker::new("subscribe @s-a events\n");
    t.feed(b"sub 1 s-a\nEVENT 1 exited\nDELTA 1 seq=1 screen x\n");
    assert!(!t.session_over());
}

#[test]
fn a_resumed_subscribe_drops_its_old_process_anchors() {
    assert_eq!(
        resume_request(
            "@s-a subscribe screen,events since=41 since-turn=7 since-block=3 every-frame\n",
            &[]
        )
        .as_deref(),
        Some("@s-a subscribe screen,events every-frame\n")
    );
    assert_eq!(
        resume_request("subscribe @* sessions\n", &[]).as_deref(),
        Some("subscribe @* sessions\n")
    );
}

/// A LOCAL NUMBER IS AN OLD-PROCESS ANCHOR TOO (review of 2026-09-25): a
/// successor numbers the sessions it adopts afresh, in layout order, so a
/// target sent as `@3` again streamed ANOTHER tab. Each local target is
/// rewritten to the session id the stream's own `sub <n> <sid>` line named;
/// one the stream never tied to a sid cannot be followed. NEGATIVE CONTROL:
/// sid, self and roster targets pass as they are.
#[test]
fn a_resumed_subscribe_names_a_local_target_by_its_sid() {
    let tied = [
        ("3".to_string(), "s-aaaa".to_string()),
        ("5".to_string(), "s-bbbb".to_string()),
    ];
    assert_eq!(
        resume_request("subscribe @3 screen,events since=9\n", &tied).as_deref(),
        Some("subscribe @s-aaaa screen,events\n")
    );
    assert_eq!(
        resume_request("subscribe @3,@s-cccc,@.,@5 events\n", &tied).as_deref(),
        Some("subscribe @s-aaaa,@s-cccc,@.,@s-bbbb events\n")
    );
    assert_eq!(
        resume_request("subscribe @4 events\n", &tied),
        None,
        "a local number the stream never tied to a session"
    );
    let mut t = FrameTracker::new("subscribe @3 screen,events\n");
    t.feed(b"sub 3 s-aaaa\nDELTA 3 seq=1 screen 1\nsub 9 s-row\n");
    assert_eq!(
        t.sids(),
        [("3".to_string(), "s-aaaa".to_string())],
        "a body that reads like a `sub` line is no target"
    );
}

#[test]
fn only_anchorless_blocking_reads_are_asked_again() {
    let parts = |s: &str| -> Vec<String> { s.split_whitespace().map(String::from).collect() };
    for yes in [
        "text",
        "@s-a text",
        "wait",
        "ready",
        "inbox",
        "await idle 500",
        "await match prompt",
        "await seq",
        "await seq timeout=0",
        "await inbox",
    ] {
        assert!(retries_across_replacement(&parts(yes)), "{yes}");
        assert!(is_blocking_read(&parts(yes)), "{yes}");
    }
    for no in [
        "await seq 40",
        "await inbox since=12",
        "inbox get 4",
        "@s-a await seq 3 timeout=500",
        // A local number names a session of the OLD process (review of
        // 2026-09-25): the successor's `@3` can be another tab.
        "@3 text",
        "@3 await match DONE",
        "@12 wait",
    ] {
        assert!(!retries_across_replacement(&parts(no)), "{no}");
        assert!(is_blocking_read(&parts(no)), "{no}");
    }
    for never in [
        "send hi",
        "key enter",
        "inbox seen 4",
        "post to=@s-b hi",
        "subscribe @s-a events",
    ] {
        assert!(!retries_across_replacement(&parts(never)), "{never}");
    }
    assert!(!is_blocking_read(&parts("send hi")));
}

/// A listener that accepts but never answers — a process mid-exit, whose
/// listening socket still takes connections into its backlog — is NOT a server
/// that still serves (the measured SIGKILL race). One that answers `version` is.
#[cfg(unix)]
#[test]
fn only_a_server_that_answers_counts_as_serving() {
    let (_dir, sock) = scratch_sock("rd-probe");
    let path = sock.to_str().unwrap().to_string();
    let mute = aterm_uds::CtlListener::bind(&sock).unwrap();
    assert_eq!(serving_pid(&path), None, "accepting is not answering");
    drop(mute);
    let _ = std::fs::remove_file(&sock);
    let (tx, _rx) = std::sync::mpsc::channel();
    let server = fake_server(
        aterm_uds::CtlListener::bind(&sock).unwrap(),
        sock.clone(),
        "",
        false,
        tx,
    );
    assert_eq!(serving_pid(&path), Some(std::process::id()));
    stop(&sock);
    server.join().unwrap();
}

/// The four verdicts, each against a real socket.
#[cfg(unix)]
#[test]
fn a_hangup_is_classified_by_who_serves_the_path_now() {
    let (_dir, sock) = scratch_sock("rd-class");
    let path = sock.to_str().unwrap().to_string();
    let own = std::process::id();
    let gone = gone_pid();
    let follow = Redial::new(Follow::Path(path.clone())).with_bound(Duration::from_millis(300));

    assert_eq!(follow.after_hangup(&path, None, None), Hangup::Unknown);
    // Nothing answers: the server is gone and nobody replaced it.
    let started = Instant::now();
    assert!(matches!(
        follow.after_hangup(&path, Some(gone), None),
        Hangup::Gone {
            by_deadline: false,
            ..
        }
    ));
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "it waited the bound"
    );

    let (tx, _rx) = std::sync::mpsc::channel();
    let server = fake_server(
        aterm_uds::CtlListener::bind(&sock).unwrap(),
        sock.clone(),
        "",
        false,
        tx,
    );
    // The SAME process still answers: it ended the exchange on purpose.
    assert_eq!(
        follow.after_hangup(&path, Some(own), None),
        Hangup::StillServing
    );
    // A DIFFERENT live process answers on the path: the successor.
    assert_eq!(
        follow.after_hangup(&path, Some(gone), None),
        Hangup::Successor {
            path: path.clone(),
            pid: own
        }
    );
    stop(&sock);
    server.join().unwrap();
    // A `--pid` pin follows nothing, and says so at once.
    let started = Instant::now();
    assert_eq!(
        Redial::none().after_hangup(&path, Some(gone), None),
        Hangup::Gone {
            waited: Duration::ZERO,
            by_deadline: false
        }
    );
    assert!(started.elapsed() < Duration::from_secs(1));
}

/// A successor that binds AFTER the hang-up — the handoff's successor comes up
/// while the client waits — is found inside the bound.
#[cfg(unix)]
#[test]
fn a_successor_that_arrives_late_is_waited_for() {
    let (_dir, sock) = scratch_sock("rd-late");
    let path = sock.to_str().unwrap().to_string();
    let bind_at = sock.clone();
    let late = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        let listener = aterm_uds::CtlListener::bind(&bind_at).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        serve(&listener, &bind_at, "", false, &tx);
    });
    let verdict = Redial::new(Follow::Path(path.clone()))
        .with_bound(Duration::from_secs(20))
        .after_hangup(&path, Some(gone_pid()), None);
    assert_eq!(
        verdict,
        Hangup::Successor {
            path,
            pid: std::process::id()
        }
    );
    stop(&sock);
    late.join().unwrap();
}

/// THE DEFECT, replayed: a `subscribe` whose server is replaced mid-stream.
/// The old leg's frames, then — after the successor answers on the same
/// (fixed) path — the successor's frames, all on one stdout; the resumed
/// request carries no `since=`; the relay ends 0 only when the stream says its
/// session exited. NEGATIVE CONTROL: the relay as it was (copy to EOF, exit 0)
/// run on the same first leg stops at the hang-up — "session over".
#[cfg(unix)]
#[test]
fn a_subscription_follows_its_server_across_a_replacement() {
    let (_dir, sock) = scratch_sock("rd-relay");
    let path = sock.to_str().unwrap().to_string();
    let request = "subscribe @s-a cursor,events since=9\n";
    let (seen_tx, seen) = std::sync::mpsc::channel();

    let old = fake_server(
        aterm_uds::CtlListener::bind(&sock).unwrap(),
        sock.clone(),
        "OK subscribe 1\nsub 1 s-a\nDELTA 1 seq=9 cursor 0 0 1 block\n",
        true,
        seen_tx.clone(),
    );
    let stream = CtlStream::connect(&sock).expect("connect");
    let mut reader = BufReader::new(&stream);
    handshake(&stream, &mut reader, request);
    old.join().unwrap();
    // The successor: binds the same path once the old one is gone.
    let successor_path = sock.clone();
    let successor = std::thread::spawn(move || {
        let listener = aterm_uds::CtlListener::bind(&successor_path).unwrap();
        serve(
            &listener,
            &successor_path,
            "OK subscribe 1\nsub 7 s-a\nDELTA 7 seq=1 cursor 5 5 1 block\nEVENT 7 exited\n",
            false,
            &seen_tx,
        );
    });

    let mut out = Vec::new();
    let code = relay_subscription(
        FirstLeg {
            stream: &stream,
            reader: &mut reader,
            dialed: &path,
            server: Some(gone_pid()),
        },
        request,
        None,
        &Redial::new(Follow::Path(path.clone())).with_bound(Duration::from_secs(20)),
        &mut out,
    )
    .expect("relay");
    stop(&sock);
    successor.join().unwrap();
    let text = String::from_utf8(out).unwrap();
    assert_eq!(code, ExitCode::SUCCESS, "ended by the successor's `exited`");
    let old_at = text
        .find("DELTA 1 seq=9 cursor 0 0")
        .expect("old leg relayed");
    let new_at = text
        .find("DELTA 7 seq=1 cursor 5 5")
        .unwrap_or_else(|| panic!("successor leg relayed: {text}"));
    assert!(old_at < new_at, "{text}");
    let requests: Vec<String> = seen.try_iter().collect();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(requests[0].contains("since=9"));
    assert_eq!(requests[1], "subscribe @s-a cursor,events\n");

    // Control: the relay as it was, on the same shape of first leg.
    let (_dir2, sock2) = scratch_sock("rd-before");
    let (tx2, _rx2) = std::sync::mpsc::channel();
    let old2 = fake_server(
        aterm_uds::CtlListener::bind(&sock2).unwrap(),
        sock2.clone(),
        "OK subscribe 1\nsub 1 s-a\nDELTA 1 seq=9 cursor 0 0 1 block\n",
        true,
        tx2,
    );
    let stream2 = CtlStream::connect(&sock2).expect("connect");
    let mut reader2 = BufReader::new(&stream2);
    handshake(&stream2, &mut reader2, request);
    old2.join().unwrap();
    let mut before = Vec::new();
    std::io::copy(&mut reader2, &mut before).unwrap();
    assert!(
        !String::from_utf8_lossy(&before).contains("cursor 5 5"),
        "control: the old relay ends at the hang-up (and returned SUCCESS there)"
    );
}

/// THE LOCAL-NUMBER DEFECT, replayed (review of 2026-09-25): the old server
/// acknowledged `subscribe @3 …` with `sub 3 s-aaaa`; the successor numbers its
/// sessions afresh, and the relay sent it `subscribe @3 …` verbatim — streaming
/// whatever tab is `3` there. The successor is now asked for `@s-aaaa`. And a
/// local target the stream never tied to a sid (no `sub` line read) is not
/// followed at all: exit 75, the successor asked nothing.
#[cfg(unix)]
#[test]
fn a_subscription_by_local_number_follows_its_session_not_the_number() {
    for (case, first_leg, expect) in [
        (
            "tied",
            "OK subscribe 1\nsub 3 s-aaaa\nDELTA 3 seq=9 cursor 0 0 1 block\n",
            Some("subscribe @s-aaaa cursor,events\n"),
        ),
        ("untied", "OK subscribe 1\n", None),
    ] {
        let (_dir, sock) = scratch_sock(&format!("rd-local-{case}"));
        let path = sock.to_str().unwrap().to_string();
        let request = "subscribe @3 cursor,events\n";
        let (seen_tx, seen) = std::sync::mpsc::channel();
        let old = fake_server(
            aterm_uds::CtlListener::bind(&sock).unwrap(),
            sock.clone(),
            first_leg,
            true,
            seen_tx.clone(),
        );
        let stream = CtlStream::connect(&sock).expect("connect");
        let mut reader = BufReader::new(&stream);
        handshake(&stream, &mut reader, request);
        old.join().unwrap();
        let successor_path = sock.clone();
        let successor = std::thread::spawn(move || {
            let listener = aterm_uds::CtlListener::bind(&successor_path).unwrap();
            serve(
                &listener,
                &successor_path,
                "OK subscribe 1\nsub 1 s-aaaa\nEVENT 1 exited\n",
                false,
                &seen_tx,
            );
        });
        let mut out = Vec::new();
        let code = relay_subscription(
            FirstLeg {
                stream: &stream,
                reader: &mut reader,
                dialed: &path,
                server: Some(gone_pid()),
            },
            request,
            None,
            &Redial::new(Follow::Path(path.clone())).with_bound(Duration::from_secs(20)),
            &mut out,
        )
        .expect("relay");
        stop(&sock);
        successor.join().unwrap();
        let requests: Vec<String> = seen.try_iter().collect();
        match expect {
            Some(resumed) => {
                assert_eq!(code, ExitCode::SUCCESS, "{case}");
                assert_eq!(requests.len(), 2, "{case}: {requests:?}");
                assert_eq!(requests[1], resumed, "{case}");
            }
            None => {
                assert_eq!(code, ExitCode::from(EXIT_REPLACED), "{case}");
                assert_eq!(requests.len(), 1, "{case}: the successor asked nothing");
            }
        }
    }
}

/// No successor: the relay says so and exits 75 — never 0.
#[cfg(unix)]
#[test]
fn a_subscription_whose_server_is_not_replaced_exits_75() {
    let (_dir, sock) = scratch_sock("rd-gone");
    let path = sock.to_str().unwrap().to_string();
    let request = "subscribe @s-a screen\n";
    let (tx, _rx) = std::sync::mpsc::channel();
    let old = fake_server(
        aterm_uds::CtlListener::bind(&sock).unwrap(),
        sock.clone(),
        "OK subscribe 1\nsub 1 s-a\nDELTA 1 seq=2 screen 1\nhello\n",
        true,
        tx,
    );
    let stream = CtlStream::connect(&sock).expect("connect");
    let mut reader = BufReader::new(&stream);
    handshake(&stream, &mut reader, request);
    old.join().unwrap();
    let mut out = Vec::new();
    let code = relay_subscription(
        FirstLeg {
            stream: &stream,
            reader: &mut reader,
            dialed: &path,
            server: Some(gone_pid()),
        },
        request,
        None,
        &Redial::new(Follow::Path(path.clone())).with_bound(Duration::from_millis(400)),
        &mut out,
    )
    .unwrap();
    assert_eq!(code, ExitCode::from(EXIT_REPLACED));
    assert!(String::from_utf8(out).unwrap().contains("hello"));
}

/// A server that STILL ANSWERS ended the stream on purpose (a fixed-target
/// subscription whose sessions closed): exit 0 at once, no 30 s wait.
#[cfg(unix)]
#[test]
fn a_deliberate_end_by_a_live_server_is_still_exit_0_and_immediate() {
    let (_dir, sock) = scratch_sock("rd-live");
    let path = sock.to_str().unwrap().to_string();
    let request = "subscribe @s-a screen\n";
    let (tx, _rx) = std::sync::mpsc::channel();
    let server = fake_server(
        aterm_uds::CtlListener::bind(&sock).unwrap(),
        sock.clone(),
        "OK subscribe 1\nsub 1 s-a\n",
        false,
        tx,
    );
    let stream = CtlStream::connect(&sock).expect("connect");
    let mut reader = BufReader::new(&stream);
    handshake(&stream, &mut reader, request);
    let started = Instant::now();
    let mut out = Vec::new();
    let code = relay_subscription(
        FirstLeg {
            stream: &stream,
            reader: &mut reader,
            dialed: &path,
            server: Some(std::process::id()),
        },
        request,
        None,
        &Redial::new(Follow::Path(path.clone())).with_bound(Duration::from_secs(20)),
        &mut out,
    )
    .unwrap();
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(started.elapsed() < Duration::from_secs(5));
    stop(&sock);
    server.join().unwrap();
}

/// A blocking read cut before its reply: asked once more on the successor when
/// it carries no old-process anchor; 75 with a note when it does, or when
/// nothing replaced the server; the original error when the same server still
/// answers (it hung up on purpose).
#[cfg(unix)]
#[test]
fn a_blocking_read_is_asked_again_only_where_that_means_the_same_question() {
    let (_dir, sock) = scratch_sock("rd-block");
    let path = sock.to_str().unwrap().to_string();
    let parts = |s: &str| -> Vec<String> { s.split_whitespace().map(String::from).collect() };
    let redial = Redial::new(Follow::Path(path.clone())).with_bound(Duration::from_millis(300));
    let gone = gone_pid();

    // Nothing answers: 75, and `again` never runs.
    let code = super::super::replaced_before_reply(
        &path,
        Some(gone),
        &parts("await idle 500"),
        &redial,
        None,
        |_| panic!("no successor to ask"),
        None,
    )
    .unwrap();
    assert_eq!(code, ExitCode::from(EXIT_REPLACED));

    let (tx, _rx) = std::sync::mpsc::channel();
    let server = fake_server(
        aterm_uds::CtlListener::bind(&sock).unwrap(),
        sock.clone(),
        "",
        false,
        tx,
    );
    // A successor answers: an anchorless wait is asked there, once.
    let mut asked = None;
    let code = super::super::replaced_before_reply(
        &path,
        Some(gone),
        &parts("@s-a await idle 500"),
        &redial,
        None,
        |next| {
            asked = Some(next.to_string());
            Ok(ExitCode::from(124))
        },
        None,
    )
    .unwrap();
    assert_eq!(asked.as_deref(), Some(path.as_str()));
    assert_eq!(
        code,
        ExitCode::from(124),
        "the successor's answer is the answer"
    );
    // ...but a wait anchored in the old process is not.
    let code = super::super::replaced_before_reply(
        &path,
        Some(gone),
        &parts("await seq 40"),
        &redial,
        None,
        |_| panic!("a seq anchor means nothing to the successor"),
        None,
    )
    .unwrap();
    assert_eq!(code, ExitCode::from(EXIT_REPLACED));
    // Nor is an inbox row id: each process numbers its own rows, so the same id
    // on the successor can name another message (review of 2026-09-24).
    let code = super::super::replaced_before_reply(
        &path,
        Some(gone),
        &parts("@s-a inbox get 4"),
        &redial,
        None,
        |_| panic!("an inbox id means nothing to the successor"),
        None,
    )
    .unwrap();
    assert_eq!(code, ExitCode::from(EXIT_REPLACED));
    // The same server still answering: the original error, as before.
    let err = super::super::replaced_before_reply(
        &path,
        Some(std::process::id()),
        &parts("await idle 500"),
        &redial,
        None,
        |_| panic!("not replaced"),
        None,
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    stop(&sock);
    server.join().unwrap();
}

/// AN EXPLICIT `--timeout` BOUNDS THE WAIT FOR A SUCCESSOR (the review of
/// 2026-09-24 measured `--timeout 3 subscribe` and `--timeout 3 await match`
/// each exiting 75 after 31 s on a SIGKILLed instance: the 30 s successor wait
/// on top of the caller's 3). Against a successor bound far longer than the
/// deadline, the classification, a `subscribe` relay under a short watch and a
/// blocking read with little left each give up at about the deadline — exit 75,
/// saying it was the deadline. NEGATIVE CONTROL: the same hang-up with no
/// deadline waits the whole bound, which is what all three did before.
///
/// Sized so load cannot decide it (the load-sensitive test audit of
/// 2026-09-27). The legs run against the real 30 s [`SUCCESSOR_BOUND`], and
/// each must end inside `deadline..deadline + slack`: the floor is safe because
/// every leg's clock starts before its deadline is fixed, and it shows each
/// really waited for a successor; the ceiling leaves 10 s for a loaded gate and
/// still sits 18 s under the bound that ignoring the deadline would wait. The
/// 2 s deadline is also the relay's headroom to read its already-buffered
/// frames and EOF before its watch expires. (It was 500 ms + 1.5 s of slack
/// against a 3 s bound.) The control runs on a 1 s bound of its own, so the
/// test does not pay the 30 s.
#[cfg(unix)]
#[test]
fn an_explicit_timeout_bounds_the_wait_for_a_successor() {
    let (_dir, sock) = scratch_sock("rd-deadline");
    let path = sock.to_str().unwrap().to_string();
    let gone = gone_pid();
    let deadline = Duration::from_secs(2);
    let slack = Duration::from_secs(10);
    let redial = Redial::new(Follow::Path(path.clone()));
    let parts = |s: &str| -> Vec<String> { s.split_whitespace().map(String::from).collect() };
    let within_the_deadline = |started: Instant| {
        let took = started.elapsed();
        assert!(
            took >= deadline && took < deadline + slack,
            "took {took:?} against a {deadline:?} deadline and a {SUCCESSOR_BOUND:?} bound"
        );
    };

    let started = Instant::now();
    let verdict = redial.after_hangup(&path, Some(gone), Some(started + deadline));
    assert!(
        matches!(
            verdict,
            Hangup::Gone {
                by_deadline: true,
                ..
            }
        ),
        "{verdict:?}"
    );
    within_the_deadline(started);

    // The relay: the first leg's frame, the hang-up, then no successor within
    // the watch.
    let request = "subscribe @s-a screen\n";
    let (tx, _rx) = std::sync::mpsc::channel();
    let old = fake_server(
        aterm_uds::CtlListener::bind(&sock).unwrap(),
        sock.clone(),
        "OK subscribe 1\nsub 1 s-a\nDELTA 1 seq=2 screen 1\nhello\n",
        true,
        tx,
    );
    let stream = CtlStream::connect(&sock).expect("connect");
    let mut reader = BufReader::new(&stream);
    handshake(&stream, &mut reader, request);
    old.join().unwrap();
    let started = Instant::now();
    let mut out = Vec::new();
    let code = relay_subscription(
        FirstLeg {
            stream: &stream,
            reader: &mut reader,
            dialed: &path,
            server: Some(gone),
        },
        request,
        Some(deadline),
        &redial,
        &mut out,
    )
    .unwrap();
    assert_eq!(code, ExitCode::from(EXIT_REPLACED));
    within_the_deadline(started);
    assert!(String::from_utf8(out).unwrap().contains("hello"));

    // A blocking read with the same time left.
    let started = Instant::now();
    let code = super::super::replaced_before_reply(
        &path,
        Some(gone),
        &parts("@s-a await match prompt"),
        &redial,
        Some(started + deadline),
        |_| panic!("no successor to ask"),
        None,
    )
    .unwrap();
    assert_eq!(code, ExitCode::from(EXIT_REPLACED));
    within_the_deadline(started);

    // Control: no deadline, the whole bound.
    let bound = Duration::from_secs(1);
    let started = Instant::now();
    assert!(matches!(
        Redial::new(Follow::Path(path.clone()))
            .with_bound(bound)
            .after_hangup(&path, Some(gone), None),
        Hangup::Gone {
            by_deadline: false,
            ..
        }
    ));
    assert!(started.elapsed() >= bound, "{:?}", started.elapsed());
    assert!(gone_note(Some(gone), deadline, true).contains("its --timeout left"));
}

/// A scratch RENDEZVOUS dir — what `aterm_uds::control_socket_dir` names —
/// with its `graph/` subdir, short enough for socket paths inside it.
#[cfg(unix)]
fn scratch_rendezvous(tag: &str) -> (Scratch, PathBuf) {
    let (scratch, sock) = scratch_sock(tag);
    let dir = sock.parent().expect("scratch dir").to_path_buf();
    std::fs::create_dir(dir.join("graph")).expect("graph dir");
    (scratch, dir)
}

/// Publish `sid`'s graph entry in `dir` naming `sock`, as an instance does for
/// every session it hosts (the format `control_socket::graph_entry_sock` reads).
#[cfg(unix)]
fn publish_graph_entry(dir: &std::path::Path, sid: &str, sock: &std::path::Path) {
    std::fs::write(
        dir.join("graph").join(sid),
        format!("sock {}\n", sock.display()),
    )
    .expect("graph entry");
}

/// THE REAL SELF-UPDATE PATH (`Follow::Flagless`): the successor does not
/// rebind the socket the call dialled — it binds its OWN `aterm-<pid>.sock` —
/// and is reached through the calling session's graph entry, which the
/// successor republishes, or, outside a session, through the `latest` alias it
/// repoints. CONTROL: a fixed-path follow of the dialled socket never finds a
/// successor on another socket — the gap the flagless follow closes.
#[cfg(unix)]
#[test]
fn a_flagless_call_follows_its_session_to_the_successors_own_socket() {
    let (_scratch, dir) = scratch_rendezvous("rd-flag");
    let gone = gone_pid();
    let own = std::process::id();
    let old_sock = dir.join(format!("aterm-{gone}.sock"));
    let old = old_sock.to_str().unwrap();
    let new_sock = dir.join(format!("aterm-{own}.sock"));
    let sid = "s-0123abcd";
    let (tx, _rx) = std::sync::mpsc::channel();
    let successor = fake_server(
        aterm_uds::CtlListener::bind(&new_sock).unwrap(),
        new_sock.clone(),
        "",
        false,
        tx,
    );
    publish_graph_entry(&dir, sid, &new_sock);

    let in_session = Redial::new(Follow::Flagless {
        dir: dir.clone(),
        self_sid: Some(sid.to_string()),
    })
    .with_bound(Duration::from_secs(5));
    assert_eq!(
        in_session.after_hangup(old, Some(gone), None),
        Hangup::Successor {
            path: new_sock.to_str().unwrap().to_string(),
            pid: own
        }
    );

    let alias = dir.join(super::super::SOCK_FILE);
    std::os::unix::fs::symlink(&new_sock, &alias).unwrap();
    let outside = Redial::new(Follow::Flagless {
        dir: dir.clone(),
        self_sid: None,
    })
    .with_bound(Duration::from_secs(5));
    assert_eq!(
        outside.after_hangup(old, Some(gone), None),
        Hangup::Successor {
            path: alias.to_str().unwrap().to_string(),
            pid: own
        }
    );

    let pinned = Redial::new(Follow::Path(old.to_string())).with_bound(Duration::from_millis(300));
    assert!(matches!(
        pinned.after_hangup(old, Some(gone), None),
        Hangup::Gone { .. }
    ));
    stop(&new_sock);
    successor.join().unwrap();
}

/// …and never a STRANGER: a live instance in the same dir that the calling
/// session's graph entry does not name and the `latest` alias does not point at
/// is not a successor — the dead-alias fallback a fresh flagless call takes is
/// not a redial target. CONTROL: once the entry names that instance (its
/// successor republished it), it is.
#[cfg(unix)]
#[test]
fn a_flagless_call_never_takes_an_unrelated_instance_for_a_successor() {
    let (_scratch, dir) = scratch_rendezvous("rd-stranger");
    let gone = gone_pid();
    let own = std::process::id();
    let old_sock = dir.join(format!("aterm-{gone}.sock"));
    let old = old_sock.to_str().unwrap();
    let stranger_sock = dir.join(format!("aterm-{own}.sock"));
    let sid = "s-4567cdef";
    let (tx, _rx) = std::sync::mpsc::channel();
    let stranger = fake_server(
        aterm_uds::CtlListener::bind(&stranger_sock).unwrap(),
        stranger_sock.clone(),
        "",
        false,
        tx,
    );
    // The entry still names the dead instance, and there is no alias.
    publish_graph_entry(&dir, sid, &old_sock);
    let follow = Redial::new(Follow::Flagless {
        dir: dir.clone(),
        self_sid: Some(sid.to_string()),
    })
    .with_bound(Duration::from_millis(400));
    assert!(matches!(
        follow.after_hangup(old, Some(gone), None),
        Hangup::Gone { .. }
    ));
    // THE ALIAS NAMES THE STRANGER (review of 2026-09-25: two windows, the
    // alias on the newer one, the older self-updating and its successor not
    // yet republished): still no successor for a call made inside a session —
    // neither through the alias nor through a dialled path that was the alias.
    let alias = dir.join(super::super::SOCK_FILE);
    std::os::unix::fs::symlink(&stranger_sock, &alias).unwrap();
    assert!(matches!(
        follow.after_hangup(old, Some(gone), None),
        Hangup::Gone { .. }
    ));
    assert!(matches!(
        follow.after_hangup(alias.to_str().unwrap(), Some(gone), None),
        Hangup::Gone { .. }
    ));
    // Outside any session the alias IS what the call followed: taken.
    let outside = Redial::new(Follow::Flagless {
        dir: dir.clone(),
        self_sid: None,
    })
    .with_bound(Duration::from_millis(400));
    assert!(matches!(
        outside.after_hangup(old, Some(gone), None),
        Hangup::Successor { pid, .. } if pid == own
    ));
    std::fs::remove_file(&alias).unwrap();

    publish_graph_entry(&dir, sid, &stranger_sock);
    assert_eq!(
        follow.after_hangup(old, Some(gone), None),
        Hangup::Successor {
            path: stranger_sock.to_str().unwrap().to_string(),
            pid: own
        }
    );
    stop(&stranger_sock);
    stranger.join().unwrap();
}

/// The relay across the real update path: the old instance's socket goes dark
/// for good, the successor comes up on its own socket and republishes the
/// session's graph entry, and the stream resumes from it on the same stdout.
#[cfg(unix)]
#[test]
fn a_flagless_subscription_resumes_on_the_successors_new_socket() {
    let (_scratch, dir) = scratch_rendezvous("rd-flag-relay");
    let gone = gone_pid();
    let old_sock = dir.join(format!("aterm-{gone}.sock"));
    let new_sock = dir.join(format!("aterm-{}.sock", std::process::id()));
    let sid = "s-89abcdef";
    let request = "subscribe @s-a cursor,events\n";
    let (seen_tx, seen) = std::sync::mpsc::channel();
    let old = fake_server(
        aterm_uds::CtlListener::bind(&old_sock).unwrap(),
        old_sock.clone(),
        "OK subscribe 1\nsub 1 s-a\nDELTA 1 seq=3 cursor 0 0 1 block\n",
        true,
        seen_tx.clone(),
    );
    publish_graph_entry(&dir, sid, &old_sock);
    let stream = CtlStream::connect(&old_sock).expect("connect");
    let mut reader = BufReader::new(&stream);
    handshake(&stream, &mut reader, request);
    old.join().unwrap();

    let (bind_at, entry_dir) = (new_sock.clone(), dir.clone());
    let successor = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        let listener = aterm_uds::CtlListener::bind(&bind_at).unwrap();
        publish_graph_entry(&entry_dir, sid, &bind_at);
        serve(
            &listener,
            &bind_at,
            "OK subscribe 1\nsub 4 s-a\nDELTA 4 seq=1 cursor 9 9 1 block\nEVENT 4 exited\n",
            false,
            &seen_tx,
        );
    });
    let mut out = Vec::new();
    let code = relay_subscription(
        FirstLeg {
            stream: &stream,
            reader: &mut reader,
            dialed: old_sock.to_str().unwrap(),
            server: Some(gone),
        },
        request,
        None,
        &Redial::new(Follow::Flagless {
            dir: dir.clone(),
            self_sid: Some(sid.to_string()),
        })
        .with_bound(Duration::from_secs(20)),
        &mut out,
    )
    .expect("relay");
    stop(&new_sock);
    successor.join().unwrap();
    let text = String::from_utf8(out).unwrap();
    assert_eq!(code, ExitCode::SUCCESS, "ended by the successor's `exited`");
    let old_at = text.find("cursor 0 0").expect("old leg relayed");
    let new_at = text
        .find("cursor 9 9")
        .unwrap_or_else(|| panic!("successor leg relayed: {text}"));
    assert!(old_at < new_at, "{text}");
    assert_eq!(seen.try_iter().count(), 2, "one request per leg");
}

/// Where a redial looks is what the call resolved through: a `--pid` pin
/// follows nothing, `--sock` follows its path, and the flagless default the
/// calling session's graph entry and the alias in the rendezvous dir —
/// `Nothing` when there is no such dir. No environment variable selects a
/// socket (2026-09-24), so none can steer a redial.
#[test]
fn a_redial_follows_what_the_call_resolved_through() {
    use super::super::redial_follow_in as follow;
    let dir = PathBuf::from("/r");
    let sid = Some("s-ab".to_string());
    assert_eq!(
        follow(Some(dir.clone()), Some("/x.sock"), Some(42), sid.clone()),
        Follow::Nothing,
        "--pid"
    );
    assert_eq!(
        follow(Some(dir.clone()), Some("/x.sock"), None, sid.clone()),
        Follow::Path("/x.sock".to_string()),
        "--sock"
    );
    assert_eq!(
        follow(Some(dir.clone()), None, None, sid.clone()),
        Follow::Flagless {
            dir: dir.clone(),
            self_sid: sid.clone()
        }
    );
    assert_eq!(
        follow(Some(dir.clone()), None, None, None),
        Follow::Flagless {
            dir,
            self_sid: None
        },
        "outside a session the flagless rule follows the alias alone"
    );
    assert_eq!(
        follow(None, None, None, sid),
        Follow::Nothing,
        "no rendezvous dir"
    );
}
