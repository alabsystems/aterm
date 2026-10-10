// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The redial rule against real sockets. The old server's pid is a reaped
//! child's, so an in-process fake server (this test process's pid) plays the
//! successor: a live server whose pid differs from the one the exchange started
//! with. Real successor processes, fresh or already running, are exercised by
//! `ChildSuccessor` here.

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

/// A short socket path in a fresh scratch dir under `/tmp` (sun_path is ~104
/// bytes, and the platform temp dir can be longer), removed on drop.
#[cfg(unix)]
fn scratch_sock(tag: &str) -> (aterm_tempfile::TempDir, PathBuf) {
    let dir = aterm_tempfile::Builder::new()
        .prefix(&format!("{tag}-"))
        .tempdir_in("/tmp")
        .expect("scratch dir");
    let sock = dir.path().join("s.sock");
    (dir, sock)
}

/// A scripted fake server on `listener`, in its own thread. It answers every
/// `version` probe `OK` (the redial's liveness test), ignores a connection that
/// hangs up without a request, answers the first other request (recorded in
/// `seen`) with `reply` and closes it, and stops on a `stop` request
/// ([`stop`]). With `retire` set it stops after its reply and removes its
/// socket file, as the old process exiting.
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

/// Complete the subscribe handshake as `exchange` does: send the request and
/// read the `OK subscribe` status line through the reader the relay keeps
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

    // A body that reads like the marker is skipped by its declared length.
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
    t.start_leg(LegSession::Same);
    assert!(!t.session_over());
    // Unframeable input can only ever mean "cannot say".
    let mut t = FrameTracker::new("subscribe @s-a events\n");
    t.feed(b"sub 1 s-a\nEVENT 1 exited\nDELTA 1 seq=1 screen x\n");
    assert!(!t.session_over());
}

#[test]
fn a_resumed_subscribe_drops_its_old_process_anchors() {
    let none = EventAnchors::default();
    assert_eq!(
        resume_request(
            "@s-a subscribe screen,events since=41 since-turn=7 since-block=3 every-frame\n",
            &[],
            &none,
        )
        .as_deref(),
        Some("@s-a subscribe screen,events every-frame since-block=3 since-turn=7\n"),
        "`since=` goes; the carried event anchors stay"
    );
    assert_eq!(
        resume_request("subscribe @* sessions\n", &[], &none).as_deref(),
        Some("subscribe @* sessions\n")
    );
}

/// A resumed `events` stream asks the successor for what comes after the
/// newest `block-complete` and `turn` it relayed (ids the handoff carries).
/// Negative controls: a multi-target stream carries none, and a stream that
/// relayed nothing keeps the anchor it was opened with.
#[test]
fn a_resumed_events_stream_keeps_its_carried_anchors() {
    let mut t = FrameTracker::new("subscribe @s-a events\n");
    t.feed(
        b"sub 1 s-a\nEVENT 1 block-complete 12 exit=0\n\
          EVENT 1 turn 7 submitted=1 status=settled dur_ms=5\n",
    );
    t.start_leg(LegSession::Same);
    assert_eq!(
        resume_request("subscribe @s-a events\n", &t.sids(), &t.anchors()).as_deref(),
        Some("subscribe @s-a events since-block=12 since-turn=7\n"),
        "the anchors outlive the leg that relayed them"
    );
    assert_eq!(
        resume_request(
            "subscribe @s-a events since-block=20\n",
            &[],
            &EventAnchors {
                block: Some(12),
                turn: None
            }
        )
        .as_deref(),
        Some("subscribe @s-a events since-block=20\n"),
        "never below the anchor the stream was opened with"
    );
    assert_eq!(
        resume_request(
            "subscribe @s-a,@s-b events\n",
            &[],
            &EventAnchors {
                block: Some(12),
                turn: Some(7)
            }
        )
        .as_deref(),
        Some("subscribe @s-a,@s-b events\n"),
        "a multi-target stream carries no anchor"
    );
}

/// A SELF target (`@.`, `@`, or none) is resolved again on the successor —
/// for a client outside every session, the active tab — so the carried
/// anchors ride only once it is pinned to the sid the stream's `sub` line
/// named. With no such sid they are left off (the successor seeds at its live
/// high); sent, another tab's finished blocks would come back as fresh events.
///
/// THE FALLBACK FOR A PINNED RESUME THE SUCCESSOR REFUSES: a replacement that
/// is not an update names its sessions afresh, so the pinned sid meets `ERR no
/// such session`. The request asked once more is the self target AS WRITTEN
/// and NO anchors. Only where there is a pin to give up: a sid target, an
/// unpinnable self target (no sid tied) and a multi-target stream have no
/// other request to ask.
#[test]
fn a_refused_pin_falls_back_to_self_as_written_without_anchors() {
    let seen = EventAnchors {
        block: Some(12),
        turn: Some(7),
    };
    let tied = [("1".to_string(), "s-aaaa".to_string())];
    for (request, pinned, fallback) in [
        (
            "subscribe @. events\n",
            "subscribe @s-aaaa events since-block=12 since-turn=7\n",
            "subscribe @. events\n",
        ),
        (
            "subscribe events\n",
            "subscribe @s-aaaa events since-block=12 since-turn=7\n",
            "subscribe events\n",
        ),
        (
            "@ subscribe screen,events since=4\n",
            "@s-aaaa subscribe screen,events since-block=12 since-turn=7\n",
            "@ subscribe screen,events\n",
        ),
    ] {
        assert_eq!(
            resume_request(request, &tied, &seen).as_deref(),
            Some(pinned),
            "{request}"
        );
        assert_eq!(
            unpinned_resume_request(request, &tied, &seen).as_deref(),
            Some(fallback),
            "{request}"
        );
    }
    // Nothing to pin it to: the selector stays and the anchors do not ride.
    // NEGATIVE CONTROL: a sid target carries its anchors as before.
    for (request, resumed) in [
        (
            "subscribe @. events since-block=3\n",
            "subscribe @. events\n",
        ),
        ("subscribe events\n", "subscribe events\n"),
        (
            "subscribe @s-bbbb events\n",
            "subscribe @s-bbbb events since-block=12 since-turn=7\n",
        ),
    ] {
        assert_eq!(
            resume_request(request, &[], &seen).as_deref(),
            Some(resumed),
            "{request}"
        );
    }
    // NEGATIVE CONTROLS: nothing else to ask.
    for (request, sids) in [
        // A sid target keeps its anchors and has no self to fall back to.
        ("subscribe @s-bbbb events\n", &tied[..]),
        // Nothing tied: the selector already stays, with no anchors.
        ("subscribe @. events\n", &[][..]),
        // Several targets: the anchors never ride.
        ("subscribe @.,@s-cccc events\n", &tied[..]),
        ("@. subscribe @s-cccc events\n", &tied[..]),
        // A local number is tied to its sid, never re-resolved.
        ("subscribe @1 events\n", &tied[..]),
        // The live roster.
        ("subscribe @* sessions\n", &tied[..]),
    ] {
        assert_eq!(
            unpinned_resume_request(request, sids, &seen),
            None,
            "{request}"
        );
    }
}

/// The fallback outlives an update: after an update's successor took the
/// pinned request, the next leg names the sid (`@s-a`), yet the fallback is
/// still the original request's self as written. The negative control derives
/// it from the last leg instead. A stream that named its session by sid has no
/// fallback on any leg.
#[test]
fn a_fallback_to_self_survives_a_leg_that_was_pinned() {
    let seen = EventAnchors {
        block: Some(5),
        turn: None,
    };
    let tied = [("1".to_string(), "s-a".to_string())];
    let original = "subscribe @. cursor,events\n";
    // What the relay asks next after an update's successor took the pin.
    let after_update = "subscribe @s-a cursor,events since-block=5\n";
    assert_eq!(
        resume_lines(original, after_update, &tied, &seen),
        Some((after_update.to_string(), Some(original.to_string())))
    );
    assert_eq!(
        resume_lines(after_update, after_update, &tied, &seen),
        Some((after_update.to_string(), None)),
        "NEGATIVE CONTROL: from the last leg's request, no fallback"
    );
    // The first leg: the same two lines as the pin and its fallback.
    assert_eq!(
        resume_lines(original, original, &tied, &seen),
        Some((after_update.to_string(), Some(original.to_string())))
    );
    let by_sid = "subscribe @s-a cursor,events\n";
    assert_eq!(
        resume_lines(by_sid, by_sid, &tied, &seen),
        Some((after_update.to_string(), None))
    );
}

/// A successor's start for the relay tests below: after any hang-up — a fresh
/// instance. (Without a stand-in, the in-process fake successor is THIS test
/// process, which started long before the hang-up it serves.)
fn started_later(_: u32) -> Option<Stamp> {
    Some(Stamp {
        boot: Some(BootClock(u64::MAX)),
        wall: SystemTime::now().checked_add(Duration::from_secs(3600)),
    })
}

/// A successor whose start cannot be read.
fn start_unknown(_: u32) -> Option<Stamp> {
    None
}

/// A PINNED SELF RESUME AND ITS FALLBACK'S LIMIT. A successor that lacks the
/// pinned `s-old` answers `ERR no such session`. A FRESH one (started after
/// the hang-up: a relaunch after a crash or a quit) is asked once more,
/// unpinned and with no anchors, and the stream goes on with its own session
/// (`fresh-self`, `fresh-bare`). One ALREADY RUNNING at the hang-up — the
/// alias naming another window, or an update's successor whose copy of the
/// tab closed — is played by this test process itself, whose real start
/// predates the hang-up: it is NOT asked again, and the stream ends 75 instead
/// of moving to its active tab (`stranger-self`).
/// CONTROLS: an unreadable start fails closed (`unknown-self`); a successor
/// that CARRIED the session is asked for it pinned, once (`carried-self`, the
/// self-update); a stream that named `@s-old` is never re-targeted
/// (`fresh-named`); and a refusal other than `ERR no such session` — a busy
/// or saturated socket, a halt, a denial, from a successor that may well hold
/// the session — is no reason to move, even from a fresh one (`fresh-refused`).
#[cfg(unix)]
#[test]
fn a_self_stream_moves_to_a_successors_own_session_only_when_it_started_after_the_hangup() {
    const PINNED: &str = "subscribe @s-old cursor,events since-block=12\n";
    const NO_SUCH: &str = "ERR no such session\n";
    type Started = Option<fn(u32) -> Option<Stamp>>;
    let later: Started = Some(started_later);
    for (case, request, carried, started, refusal, asked, followed) in [
        (
            "fresh-self",
            "subscribe @. cursor,events\n",
            false,
            later,
            NO_SUCH,
            &[PINNED, "subscribe @. cursor,events\n"][..],
            Some("sub 4 s-new"),
        ),
        (
            "fresh-bare",
            "subscribe cursor,events\n",
            false,
            later,
            NO_SUCH,
            &[PINNED, "subscribe cursor,events\n"][..],
            Some("sub 4 s-new"),
        ),
        (
            "stranger-self",
            "subscribe @. cursor,events\n",
            false,
            None,
            NO_SUCH,
            &[PINNED][..],
            None,
        ),
        (
            "unknown-self",
            "subscribe @. cursor,events\n",
            false,
            Some(start_unknown as fn(u32) -> Option<Stamp>),
            NO_SUCH,
            &[PINNED][..],
            None,
        ),
        (
            "carried-self",
            "subscribe @. cursor,events\n",
            true,
            None,
            NO_SUCH,
            &[PINNED][..],
            Some("sub 4 s-old"),
        ),
        (
            "fresh-named",
            "subscribe @s-old cursor,events\n",
            false,
            later,
            NO_SUCH,
            &[PINNED][..],
            None,
        ),
        (
            "fresh-refused",
            "subscribe @. cursor,events\n",
            false,
            later,
            "ERR subscription lanes full\n",
            &[PINNED][..],
            None,
        ),
    ] {
        let (_dir, sock) = scratch_sock(&format!("rd-{case}"));
        let path = sock.to_str().unwrap().to_string();
        let (seen_tx, seen) = std::sync::mpsc::channel();
        let old = fake_server(
            aterm_uds::CtlListener::bind(&sock).unwrap(),
            sock.clone(),
            "OK subscribe 1\nsub 0 s-old\nEVENT 0 block-complete 12 exit=0\n",
            true,
            seen_tx.clone(),
        );
        let stream = CtlStream::connect(&sock).expect("connect");
        let mut reader = BufReader::new(&stream);
        handshake(&stream, &mut reader, request);
        old.join().unwrap();
        // The successor's own session is `s-new`; it holds `s-old` only when
        // it carried it.
        let successor_path = sock.clone();
        let successor = std::thread::spawn(move || {
            let listener = aterm_uds::CtlListener::bind(&successor_path).unwrap();
            loop {
                let Ok((conn, _)) = listener.accept() else {
                    return;
                };
                let mut reader = std::io::BufReader::new(&conn);
                let mut request = String::new();
                if reader.read_line(&mut request).unwrap_or(0) == 0 {
                    continue;
                }
                let reply: &[u8] = if request.starts_with("version") {
                    b"OK aterm test\n"
                } else if request.starts_with("stop") {
                    return;
                } else if !request.contains("@s-old") {
                    let _ = seen_tx.send(request);
                    b"OK subscribe 1\nsub 4 s-new\nDELTA 4 seq=1 cursor 5 5 1 block\nEVENT 4 exited\n"
                } else if carried {
                    let _ = seen_tx.send(request);
                    b"OK subscribe 1\nsub 4 s-old\nDELTA 4 seq=1 cursor 5 5 1 block\nEVENT 4 exited\n"
                } else {
                    let _ = seen_tx.send(request);
                    refusal.as_bytes()
                };
                let _ = (&conn).write_all(reply);
            }
        });
        let redial = Redial::new(Follow::Path(path.clone())).with_bound(REACHED);
        let redial = match started {
            Some(started) => redial.with_started(started),
            None => redial,
        };
        let mut out = Vec::new();
        let code = relay_subscription(
            FirstLeg {
                stream: &stream,
                reader: &mut reader,
                dialed: &path,
                server: Some(gone_pid()),
                listener: None,
            },
            request,
            None,
            &redial,
            &mut out,
        )
        .expect("relay");
        stop(&sock);
        successor.join().unwrap();
        let requests: Vec<String> = seen.try_iter().collect();
        assert_eq!(
            requests.first().map(String::as_str),
            Some(request),
            "{case}"
        );
        assert_eq!(
            &requests[1..],
            asked,
            "{case}: what the successor was asked"
        );
        let text = String::from_utf8(out).unwrap();
        match followed {
            Some(sub) => {
                assert_eq!(code, ExitCode::SUCCESS, "{case}: the successor's `exited`");
                assert!(text.contains(sub), "{case}: {text}");
            }
            None => {
                assert_eq!(code, ExitCode::from(EXIT_REPLACED), "{case}");
                assert!(!text.contains("sub 4"), "{case}: {text}");
            }
        }
    }
}

/// The decision at a refused resume, pure: the successor is asked again, with
/// self as written, only when it answered `ERR no such session` to the pinned
/// form and started after the hang-up; that leg relays another session
/// ([`LegSession::Other`], which [`FrameTracker::start_leg`] takes from here).
/// Negative controls: a successor already running, one whose start cannot be
/// read, one on the `latest` alias, another refusal, no answer, a stream with
/// no self fallback, and a fallback equal to the pinned request all end the
/// relay; a refused fallback is never followed by a third ask.
#[test]
fn a_refused_pin_is_asked_again_only_of_a_fresh_successor() {
    let pinned = Ask {
        request: "subscribe @s-old events since-block=12\n".to_string(),
        session: LegSession::Same,
    };
    let fallback = || Some("subscribe @. events\n".to_string());
    assert_eq!(
        after_refusal(&pinned, NO_SUCH_SESSION, fallback(), Arrival::Fresh),
        Ok(Ask {
            request: "subscribe @. events\n".to_string(),
            session: LegSession::Other,
        })
    );
    for (status, fallback, arrival, why) in [
        (
            NO_SUCH_SESSION,
            fallback(),
            Arrival::Predates,
            Unresumed::Predates,
        ),
        (
            NO_SUCH_SESSION,
            fallback(),
            Arrival::Unknown,
            Unresumed::Unknown,
        ),
        (
            NO_SUCH_SESSION,
            fallback(),
            Arrival::Shared,
            Unresumed::Shared,
        ),
        (
            "ERR subscription lanes full",
            fallback(),
            Arrival::Fresh,
            Unresumed::Refused,
        ),
        // Busy to the end of the wait: said as such.
        (
            "ERR control server busy; retry",
            fallback(),
            Arrival::Fresh,
            Unresumed::Busy,
        ),
        ("", fallback(), Arrival::Fresh, Unresumed::Refused),
        (NO_SUCH_SESSION, None, Arrival::Fresh, Unresumed::Refused),
        (
            NO_SUCH_SESSION,
            Some(pinned.request.clone()),
            Arrival::Fresh,
            Unresumed::Refused,
        ),
    ] {
        assert_eq!(
            after_refusal(&pinned, status, fallback.clone(), arrival),
            Err(why),
            "{status:?} {fallback:?} {arrival:?}"
        );
    }
    // A fallback distinct from the request just refused, so only the
    // fallen-back leg's own session ends it (not the equality filter).
    let fell_back = Ask {
        request: "subscribe @. events\n".to_string(),
        session: LegSession::Other,
    };
    assert_eq!(
        after_refusal(
            &fell_back,
            NO_SUCH_SESSION,
            Some("subscribe @ events\n".to_string()),
            Arrival::Fresh
        ),
        Err(Unresumed::Refused),
        "the fallback refused in turn ends it"
    );
}

/// A fallen-back stream follows another session, so the lost session's
/// anchors must not ride with it: the next resume, pinned to the new sid,
/// carries only what the new session relayed. Driven through the [`Ask`]
/// [`after_refusal`] returns and [`FrameTracker::start_leg`]. Marking the leg
/// [`LegSession::Same`] instead would pin `s-old`'s block 12 and turn 7 to
/// `s-new`. Negative control: a leg on the same session keeps them.
#[test]
fn a_fallen_back_stream_carries_only_its_new_sessions_anchors() {
    let original = "subscribe @. events\n";
    let old_leg = b"sub 0 s-old\nEVENT 0 block-complete 12 exit=0\nEVENT 0 turn 7 status=settled\n";
    let new_leg = b"sub 4 s-new\nEVENT 4 block-complete 2 exit=0\n";
    let mut t = FrameTracker::new(original);
    t.feed(old_leg);
    let pinned = resume_request(original, &t.sids(), &t.anchors()).expect("pinned");
    assert_eq!(
        pinned,
        "subscribe @s-old events since-block=12 since-turn=7\n"
    );
    let asked = Ask {
        request: pinned,
        session: LegSession::Same,
    };
    let fallback = unpinned_resume_request(original, &t.sids(), &t.anchors());
    let ask = after_refusal(&asked, NO_SUCH_SESSION, fallback, Arrival::Fresh)
        .expect("a fresh successor is asked again");
    t.start_leg(ask.session);
    t.feed(new_leg);
    assert_eq!(
        resume_request(&ask.request, &t.sids(), &t.anchors()).as_deref(),
        Some("subscribe @s-new events since-block=2\n")
    );
    let mut same = FrameTracker::new(original);
    same.feed(old_leg);
    same.start_leg(LegSession::Same);
    same.feed(new_leg);
    assert_eq!(
        resume_request(&ask.request, &same.sids(), &same.anchors()).as_deref(),
        Some("subscribe @s-new events since-block=12 since-turn=7\n"),
        "control: a same-session leg keeps the old anchors"
    );
}

/// A child successor's argv: this test binary, running only
/// [`serve_as_a_child_successor`].
#[cfg(unix)]
const CHILD: [&str; 2] = ["redial::tests::serve_as_a_child_successor", "--exact"];

/// What tells [`serve_as_a_child_successor`] how to serve, one per line: its
/// role, the socket path it binds, the path it unlinks when it goes, the file
/// it appends each request to, the file whose creation says it is listening,
/// and the file whose existence tells it to go.
#[cfg(unix)]
const CHILD_ENV: &str = "ATERM_CTL_REDIAL_TEST_CHILD";

/// The body of a fake successor process for a relay test: it has a pid of its
/// own, so the relay takes it for the successor of an in-process one (or the
/// reverse). Idle unless [`CHILD_ENV`] is set. It binds its socket at once and
/// says so; a test moves the socket into place by renaming it (a bound socket
/// answers at its new path). It answers `version` probes `OK` and plays its
/// role on the first other request:
///
/// - `second`: records it, answers for its `s-new` with that session's
///   `exited`, and leaves;
/// - `carrier`: records it, answers for the `s-old` it carried (an update's
///   successor), waits to be told to go, then leaves;
/// - `mute`: records only the request's first 32 bytes, waits to be told to
///   go, then leaves without reading the rest, so a request larger than the
///   socket buffers fails to write;
/// - `refuses`: refuses `@s-old` with `ERR no such session` and answers any
///   other request for its `s-new` with `exited`, recording each;
/// - `old`: records it, answers for its own `s-old` (one block), and serves
///   until killed: the instance a stream starts on, which a test crashes.
#[cfg(unix)]
#[test]
fn serve_as_a_child_successor() {
    let Some(spec) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    let spec = spec.into_string().expect("a UTF-8 spec");
    let parts: Vec<&str> = spec.split('\n').collect();
    let [role, bind, leave, seen, bound, go] = parts[..] else {
        panic!("spec {spec:?}");
    };
    let listener = aterm_uds::CtlListener::bind(bind).expect("bind");
    std::fs::write(bound, b"").expect("say it listens");
    let record = |request: &[u8]| {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(seen)
            .expect("the record");
        file.write_all(request).expect("record the request");
    };
    let leave_when_told = || {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !std::path::Path::new(go).exists() {
            assert!(Instant::now() < deadline, "never told to go");
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = std::fs::remove_file(leave);
    };
    loop {
        let Ok((conn, _)) = listener.accept() else {
            return;
        };
        if role == "mute" {
            let mut head = [0u8; 32];
            if (&conn).read_exact(&mut head[..8]).is_err() {
                continue;
            }
            if &head[..8] == b"version\n" {
                let _ = (&conn).write_all(b"OK aterm test\n");
                continue;
            }
            if (&conn).read_exact(&mut head[8..]).is_err() {
                continue;
            }
            record(&[&head[..], b"\n"].concat());
            leave_when_told();
            // The connection closes here with the rest of the request unread.
            return;
        }
        let mut reader = std::io::BufReader::new(&conn);
        let mut request = String::new();
        if reader.read_line(&mut request).unwrap_or(0) == 0 {
            continue;
        }
        if request.starts_with("version") {
            let _ = (&conn).write_all(b"OK aterm test\n");
            continue;
        }
        record(request.as_bytes());
        match role {
            "old" => {
                let _ = (&conn)
                    .write_all(b"OK subscribe 1\nsub 0 s-old\nEVENT 0 block-complete 12 exit=0\n");
                // Held open until the process dies: its end is the hang-up.
                loop {
                    std::thread::park();
                }
            }
            "carrier" => {
                let _ = (&conn).write_all(b"OK subscribe 1\nsub 5 s-old\n");
                leave_when_told();
                return;
            }
            "refuses" if request.contains("@s-old") => {
                let _ = (&conn).write_all(b"ERR no such session\n");
            }
            _ => {
                let _ = (&conn).write_all(b"OK subscribe 1\nsub 5 s-new\nEVENT 5 exited\n");
                let _ = std::fs::remove_file(leave);
                return;
            }
        }
    }
}

/// A [`serve_as_a_child_successor`] process, listening, with the files it
/// shares with its test (all in that test's scratch dir).
#[cfg(unix)]
struct ChildSuccessor {
    /// Where it listens until the test renames its socket into place.
    sock: PathBuf,
    /// What it was asked, one request per line.
    seen: PathBuf,
    /// Created by the test: go.
    go: PathBuf,
    process: Served,
}

/// How a [`ChildSuccessor`] runs: this test process's own child, or an orphan
/// (a grandchild whose parent has exited), as an aterm is not its relay's
/// child.
#[cfg(unix)]
enum Served {
    Child(Reaped),
    Orphan(Orphan),
}

#[cfg(unix)]
impl ChildSuccessor {
    /// Spawn one playing `role` in `dir`, which unlinks `leave` when it goes,
    /// and wait (bounded) until it listens.
    fn spawn(dir: &std::path::Path, role: &str, leave: &std::path::Path) -> Self {
        Self::start(dir, role, leave, false)
    }

    /// [`ChildSuccessor::spawn`] as an orphan ([`Served::Orphan`]): started in
    /// the background by a `/bin/sh` that exits at once.
    fn spawn_orphan(dir: &std::path::Path, role: &str, leave: &std::path::Path) -> Self {
        Self::start(dir, role, leave, true)
    }

    fn start(dir: &std::path::Path, role: &str, leave: &std::path::Path, orphan: bool) -> Self {
        let sock = dir.join("child.sock");
        let seen = dir.join("child-seen");
        let bound = dir.join("child-bound");
        let go = dir.join("child-go");
        let spec = [
            role,
            &sock.display().to_string(),
            &leave.display().to_string(),
            &seen.display().to_string(),
            &bound.display().to_string(),
            &go.display().to_string(),
        ]
        .join("\n");
        let exe = std::env::current_exe().expect("exe");
        // `spawnfd`'s, the shell and the child alike: each holds its stdio and
        // nothing else, so neither (nor the orphan the shell leaves) keeps a
        // descriptor another test thread had open unflagged at the spawn.
        let process = if orphan {
            let shell = aterm_uds::spawnfd::command("/bin/sh")
                .arg("-c")
                .arg(r#""$@" </dev/null >/dev/null 2>&1 & echo $!"#)
                .arg("sh")
                .arg(exe)
                .args(CHILD)
                .env(CHILD_ENV, spec)
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .stderr(aterm_uds::spawnfd::Stdio::null())
                .output()
                .expect("start the orphan through /bin/sh");
            let pid = String::from_utf8_lossy(&shell.stdout)
                .trim()
                .parse()
                .expect("the orphan's pid, as /bin/sh printed it");
            Served::Orphan(Orphan(Some(pid)))
        } else {
            Served::Child(Reaped(
                aterm_uds::spawnfd::command(exe)
                    .args(CHILD)
                    .env(CHILD_ENV, spec)
                    .stdin(aterm_uds::spawnfd::Stdio::null())
                    .stdout(aterm_uds::spawnfd::Stdio::null())
                    .stderr(aterm_uds::spawnfd::Stdio::null())
                    .spawn()
                    .expect("spawn the child successor"),
            ))
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        while !bound.exists() {
            assert!(
                Instant::now() < deadline,
                "the child successor never listened"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        Self {
            sock,
            seen,
            go,
            process,
        }
    }

    /// What it was asked, one request per line.
    fn asked(&self) -> String {
        std::fs::read_to_string(&self.seen).unwrap_or_default()
    }

    /// Its pid.
    fn pid(&self) -> u32 {
        match &self.process {
            Served::Child(child) => child.0.id(),
            Served::Orphan(orphan) => orphan.0.expect("a live orphan"),
        }
    }

    /// Crash it — `SIGKILL`, every connection closed with the process — and
    /// reap it (an orphan is reaped by whatever adopted it).
    fn crash(&mut self) {
        match &mut self.process {
            Served::Child(child) => {
                child.0.kill().expect("SIGKILL the child");
                child.0.wait().expect("reap the child");
            }
            Served::Orphan(orphan) => orphan.kill(),
        }
    }
}

/// A child process killed and reaped on drop, if it has not exited by then.
#[cfg(unix)]
struct Reaped(aterm_uds::spawnfd::Child);

#[cfg(unix)]
impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// An orphan's pid, `SIGKILL`ed on drop unless [`Orphan::kill`] already
/// did (then `None`: the pid may name another process by then).
#[cfg(unix)]
struct Orphan(Option<u32>);

#[cfg(unix)]
impl Orphan {
    /// `SIGKILL` it, once.
    fn kill(&mut self) {
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        const SIGKILL: i32 = 9;
        if let Some(pid) = self.0.take().and_then(|pid| i32::try_from(pid).ok()) {
            // SAFETY: no pointers; the pid is the orphan's, alive until now.
            unsafe { kill(pid, SIGKILL) };
        }
    }

    /// Its parent's pid: field 4 of `/proc/<pid>/stat` on Linux, else as
    /// `ps` reads it.
    fn parent(&self) -> Option<u32> {
        let pid = self.0?;
        if cfg!(target_os = "linux") {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let fields = &stat[stat.rfind(')')? + 1..];
            return fields.split_ascii_whitespace().nth(1)?.parse().ok();
        }
        let out = std::process::Command::new("ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }
}

#[cfg(unix)]
impl Drop for Orphan {
    fn drop(&mut self) {
        self.kill();
    }
}

/// The fallback's leg reset, through the relay: a self stream on `s-old` falls
/// back to a fresh successor's own `s-new`, which relays its block 2, and is
/// then replaced again by a second successor that carried `s-new` (an update).
/// That one is asked for `s-new` with `s-new`'s anchor alone; resetting the
/// fallback leg as the same session would send `since-block=12 since-turn=7`.
#[cfg(unix)]
#[test]
fn a_fallen_back_stream_asks_the_next_successor_for_its_new_sessions_anchors_only() {
    let (dir, sock) = scratch_sock("rd-two");
    let path = sock.to_str().unwrap().to_string();
    let second = ChildSuccessor::spawn(dir.path(), "second", &sock);
    let request = "subscribe @. cursor,events\n";
    let (seen_tx, seen) = std::sync::mpsc::channel();
    let old = fake_server(
        aterm_uds::CtlListener::bind(&sock).unwrap(),
        sock.clone(),
        "OK subscribe 1\nsub 0 s-old\nEVENT 0 block-complete 12 exit=0\n\
         EVENT 0 turn 7 status=settled\n",
        true,
        seen_tx.clone(),
    );
    let stream = CtlStream::connect(&sock).expect("connect");
    let mut reader = BufReader::new(&stream);
    handshake(&stream, &mut reader, request);
    old.join().unwrap();
    // The first successor, in this process and fresh: it lacks `s-old`, serves
    // its own `s-new` (block 2), then goes as the old one did, the second's
    // socket renamed over its own.
    let first_path = sock.clone();
    let second_sock = second.sock.clone();
    let first = std::thread::spawn(move || {
        let listener = aterm_uds::CtlListener::bind(&first_path).unwrap();
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
            let pinned = request.contains("@s-old");
            let _ = seen_tx.send(request);
            if pinned {
                let _ = (&conn).write_all(b"ERR no such session\n");
                continue;
            }
            std::fs::rename(&second_sock, &first_path).expect("the second takes the path");
            let _ = (&conn)
                .write_all(b"OK subscribe 1\nsub 4 s-new\nEVENT 4 block-complete 2 exit=0\n");
            return;
        }
    });
    let mut out = Vec::new();
    let code = relay_subscription(
        FirstLeg {
            stream: &stream,
            reader: &mut reader,
            dialed: &path,
            server: Some(gone_pid()),
            listener: None,
        },
        request,
        None,
        &Redial::new(Follow::Path(path.clone()))
            .with_bound(REACHED)
            .with_started(started_later),
        &mut out,
    )
    .expect("relay");
    first.join().unwrap();
    let asked: Vec<String> = seen.try_iter().collect();
    assert_eq!(
        asked,
        [
            request,
            "subscribe @s-old cursor,events since-block=12 since-turn=7\n",
            "subscribe @. cursor,events\n",
        ],
        "the old server, then the first successor: pinned, then self"
    );
    assert_eq!(
        second.asked(),
        "subscribe @s-new cursor,events since-block=2\n",
        "what the second successor was asked"
    );
    assert_eq!(code, ExitCode::SUCCESS, "the second successor's `exited`");
    assert!(
        String::from_utf8(out).unwrap().contains("sub 5 s-new\n"),
        "the second successor's stream relayed"
    );
}

/// A connection to a fresh fake server on `sock`, and the server side of it
/// ([`ask_once`]'s tests): `behave` finishes with the connection before the
/// client writes a byte (the server thread is joined), so a write fails
/// deterministically.
#[cfg(unix)]
fn conn_after(
    tag: &str,
    behave: impl FnOnce(aterm_uds::CtlStream) + Send + 'static,
) -> (aterm_tempfile::TempDir, CtlStream) {
    let (dir, sock) = scratch_sock(tag);
    let listener = aterm_uds::CtlListener::bind(&sock).unwrap();
    let client = CtlStream::connect(&sock).expect("connect");
    let server = std::thread::spawn(move || {
        let (conn, _) = listener.accept().expect("accept");
        behave(conn);
    });
    server.join().unwrap();
    (dir, client)
}

/// What one ask of a successor came to. A busy listener writes its refusal and
/// closes without reading, so the client's write of a 4 MiB request fails; the
/// refusal left behind is the answer. A close with nothing left behind is
/// `Gone`; an `OK`, and a close after the request, are lines.
#[cfg(unix)]
#[test]
fn a_write_that_fails_reads_the_refusal_the_successor_left() {
    let big = format!("subscribe @s-a cursor {}\n", "x".repeat(4 << 20));
    let ask = |client: &CtlStream| {
        let mut reader = BufReader::new(client);
        ask_once(client, &mut reader, None, &big)
    };
    // Busy: the reason written, the connection closed, the request unread.
    let (_d, client) = conn_after("rd-ask-busy", |conn| {
        let _ = (&conn).write_all(b"ERR control server busy; retry\n");
    });
    assert!(
        matches!(ask(&client), Answer::Line(l) if l == "ERR control server busy; retry"),
        "the refusal it left is the answer, not a broken pipe"
    );
    // Gone: closed before the request, nothing left behind.
    let (_d, client) = conn_after("rd-ask-gone", |_conn| {});
    assert!(matches!(ask(&client), Answer::Gone));
    // A close after the request was written and read: a line, empty.
    let (_d, sock) = scratch_sock("rd-ask-eof");
    let listener = aterm_uds::CtlListener::bind(&sock).unwrap();
    let client = CtlStream::connect(&sock).expect("connect");
    let server = std::thread::spawn(move || {
        let (conn, _) = listener.accept().expect("accept");
        let mut request = String::new();
        std::io::BufReader::new(&conn)
            .read_line(&mut request)
            .unwrap();
        assert!(request.starts_with("subscribe"), "{request}");
    });
    let mut reader = BufReader::new(&client);
    assert!(
        matches!(ask_once(&client, &mut reader, None, "subscribe @s-a cursor\n"), Answer::Line(l) if l.is_empty())
    );
    server.join().unwrap();
    // Control: an `OK` is a line too.
    let (_d, sock) = scratch_sock("rd-ask-ok");
    let listener = aterm_uds::CtlListener::bind(&sock).unwrap();
    let client = CtlStream::connect(&sock).expect("connect");
    let server = std::thread::spawn(move || {
        let (conn, _) = listener.accept().expect("accept");
        let mut request = String::new();
        std::io::BufReader::new(&conn)
            .read_line(&mut request)
            .unwrap();
        (&conn).write_all(b"OK subscribe 1\n").unwrap();
    });
    let mut reader = BufReader::new(&client);
    assert!(
        matches!(ask_once(&client, &mut reader, None, "subscribe @s-a cursor\n"), Answer::Line(l) if l == "OK subscribe 1")
    );
    server.join().unwrap();
}

/// A SUCCESSOR THAT CLOSES ON THE RESUME IS ASKED AGAIN, AND IS NEVER EXIT 0
/// WITHOUT ITS STREAM. A live successor can close on the resume ask (the
/// listener with no lane free answers busy and closes without reading; macOS
/// can discard that line with the unread request): that is neither a successor
/// gone away nor an end on purpose. The connection the successor is handed for
/// an ask is held 50 ms and closed, its request unread; the request is 4 MiB,
/// so the client is still writing when the close lands and the WRITE FAILS,
/// deterministically. BUSY (its reason written) and SILENT (nothing left
/// behind), once each: the same successor is asked again and the stream goes
/// on. SATURATED: silent once, and then — every lane at work — the next three
/// connections, the probe that judges the silence among them, are answered busy
/// unread; the busy probe is the successor still there (by `version`'s `OK`
/// alone it would read as gone). SILENT FOREVER: asked again until the
/// successor wait is spent, then exit 75 and nothing relayed — never 0. FAILS
/// WITHOUT THE FIX: exit 75 after the first close, with nothing relayed;
/// SATURATED, after the whole successor wait.
#[cfg(unix)]
#[test]
fn a_successor_that_closes_on_the_resume_is_asked_again_and_never_a_silent_exit_0() {
    let filler = "x".repeat(4 << 20);
    let request = format!("subscribe @s-a cursor,events {filler}\n");
    for (shape, refusal, closes, saturated) in [
        ("busy", Some("ERR control server busy; retry\n"), 1, 0),
        ("silent", None, 1, 0),
        ("saturated", None, 1, 3),
        ("silent-forever", None, usize::MAX, 0),
    ] {
        let (_dir, sock) = scratch_sock(&format!("rd-close-{shape}"));
        let path = sock.to_str().unwrap().to_string();
        let (seen_tx, _seen) = std::sync::mpsc::channel();
        let old = fake_server(
            aterm_uds::CtlListener::bind(&sock).unwrap(),
            sock.clone(),
            "OK subscribe 1\nsub 1 s-a\n",
            true,
            seen_tx,
        );
        let stream = CtlStream::connect(&sock).expect("connect");
        let mut reader = BufReader::new(&stream);
        handshake(&stream, &mut reader, &request);
        old.join().unwrap();
        // Bound before the relay looks, so the successor is there to find.
        let listener = aterm_uds::CtlListener::bind(&sock).unwrap();
        let asks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&asks);
        let successor = std::thread::spawn(move || {
            // Connections still to answer busy, unread, whatever they are.
            let mut busy = 0;
            loop {
                let Ok((conn, _)) = listener.accept() else {
                    return;
                };
                if busy > 0 {
                    busy -= 1;
                    let _ = (&conn).write_all(b"ERR control server busy; retry\n");
                    continue;
                }
                // Classified by its first eight bytes and no more: a probe
                // is `version\n`, a `stop` is `stop\n`, and anything else is
                // an ask. One it closes on leaves its 4 MiB request UNREAD —
                // reading it would let the client's write succeed and the
                // close never happen (a mutation check showed the test
                // passing with the fix removed when this fake read the whole
                // line).
                use std::io::Read as _;
                let _ = conn.set_read_timeout(Some(Duration::from_millis(500)));
                let mut head = [0u8; 8];
                let n = (&conn).read(&mut head).unwrap_or(0);
                if n == 0 {
                    continue;
                }
                if head[..n].starts_with(b"version") {
                    let _ = (&conn).write_all(b"OK aterm test\n");
                } else if head[..n].starts_with(b"stop") {
                    return;
                } else if counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < closes {
                    // Held until the client is mid-write, then closed with
                    // the request unread.
                    std::thread::sleep(Duration::from_millis(50));
                    if let Some(reason) = refusal {
                        let _ = (&conn).write_all(reason.as_bytes());
                    }
                    busy = saturated;
                } else {
                    // Taken: the whole request read, then the stream.
                    let _ = conn.set_read_timeout(None);
                    let mut rest = String::new();
                    let _ = std::io::BufReader::new(&conn).read_line(&mut rest);
                    let _ = (&conn).write_all(b"OK subscribe 1\nsub 1 s-a\nEVENT 1 exited\n");
                }
            }
        });
        // The forever-silent shape spends the whole successor wait asking:
        // short, and whichever way it ends, it is 75.
        let bound = if closes == usize::MAX {
            Duration::from_secs(2)
        } else {
            REACHED
        };
        let mut out = Vec::new();
        let code = relay_subscription(
            FirstLeg {
                stream: &stream,
                reader: &mut reader,
                dialed: &path,
                server: Some(gone_pid()),
                listener: None,
            },
            &request,
            None,
            &Redial::new(Follow::Path(path.clone()))
                .with_bound(bound)
                .with_started(started_later),
            &mut out,
        )
        .expect("relay");
        stop(&sock);
        successor.join().unwrap();
        let out = String::from_utf8(out).unwrap();
        if closes == usize::MAX {
            assert_eq!(code, ExitCode::from(EXIT_REPLACED), "{shape}: never 0");
            assert!(
                !out.contains("EVENT"),
                "{shape}: nothing of the successor's stream relayed"
            );
        } else {
            assert_eq!(
                code,
                ExitCode::SUCCESS,
                "{shape}: the stream resumed: {out}"
            );
            assert!(
                out.ends_with("sub 1 s-a\nEVENT 1 exited\n"),
                "{shape}: the successor's leg was relayed: {out}"
            );
            assert_eq!(
                asks.load(std::sync::atomic::Ordering::SeqCst),
                2,
                "{shape}: asked again after the close"
            );
        }
    }
}

/// A BUSY SUCCESSOR IS ASKED AGAIN: right after a Commit every client
/// re-dials the successor at once, and a listener with no lane free answers
/// `ERR control server busy; retry` — a passing refusal from a successor that
/// is live and holds the session. The same successor is asked again within
/// the successor wait: here it answers busy twice, then takes the
/// stream, which goes on (its session's `exited` ends it, exit 0). FAILS
/// WITHOUT THE FIX: exit 75 after the first busy answer.
#[cfg(unix)]
#[test]
fn a_busy_successor_is_asked_again_and_the_stream_resumes() {
    let request = "subscribe @s-a cursor,events\n";
    let (_dir, sock) = scratch_sock("rd-busy-again");
    let path = sock.to_str().unwrap().to_string();
    let (seen_tx, _seen) = std::sync::mpsc::channel();
    let old = fake_server(
        aterm_uds::CtlListener::bind(&sock).unwrap(),
        sock.clone(),
        "OK subscribe 1\nsub 1 s-a\n",
        true,
        seen_tx,
    );
    let stream = CtlStream::connect(&sock).expect("connect");
    let mut reader = BufReader::new(&stream);
    handshake(&stream, &mut reader, request);
    old.join().unwrap();
    let listener = aterm_uds::CtlListener::bind(&sock).unwrap();
    let asks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = std::sync::Arc::clone(&asks);
    let successor = std::thread::spawn(move || {
        loop {
            let Ok((conn, _)) = listener.accept() else {
                return;
            };
            let mut line = String::new();
            if std::io::BufReader::new(&conn)
                .read_line(&mut line)
                .unwrap_or(0)
                == 0
            {
                continue;
            }
            if line.starts_with("version") {
                let _ = (&conn).write_all(b"OK aterm test\n");
            } else if line.starts_with("stop") {
                return;
            } else if counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
                let _ = (&conn).write_all(b"ERR control server busy; retry\n");
            } else {
                let _ = (&conn).write_all(b"OK subscribe 1\nsub 1 s-a\nEVENT 1 exited\n");
            }
        }
    });
    let mut out = Vec::new();
    let code = relay_subscription(
        FirstLeg {
            stream: &stream,
            reader: &mut reader,
            dialed: &path,
            server: Some(gone_pid()),
            listener: None,
        },
        request,
        None,
        &Redial::new(Follow::Path(path.clone()))
            .with_bound(REACHED)
            .with_started(started_later),
        &mut out,
    )
    .expect("relay");
    stop(&sock);
    successor.join().unwrap();
    let out = String::from_utf8(out).unwrap();
    assert_eq!(code, ExitCode::SUCCESS, "the stream resumed: {out}");
    assert_eq!(asks.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert!(
        out.ends_with("sub 1 s-a\nEVENT 1 exited\n"),
        "the successor's leg was relayed: {out}"
    );
}

/// THE HOLE BETWEEN TWO LEGS IS MARKED ON THE STREAM. A successor's resumed
/// watch starts live: the PTY bytes its readers drained between the Commit and
/// the resubscribe, and whatever mail and timeline records came in between, are
/// never pushed (a `subscribe … bytes > build.log` would lose `BUILD OK` in
/// silence). Each target the last leg named gets `GAP <local> replaced` between
/// the legs, the stream's own loss marker. FAILS WITHOUT THE FIX: the legs are
/// spliced bare.
#[cfg(unix)]
#[test]
fn a_resumed_stream_marks_the_hole_between_its_legs() {
    let request = "subscribe @s-a bytes\n";
    let (_dir, sock) = scratch_sock("rd-gap");
    let path = sock.to_str().unwrap().to_string();
    let (seen_tx, _seen) = std::sync::mpsc::channel();
    let old = fake_server(
        aterm_uds::CtlListener::bind(&sock).unwrap(),
        sock.clone(),
        "OK subscribe 1\nsub 1 s-a\nBYTES 1 3\nabc\n",
        true,
        seen_tx,
    );
    let stream = CtlStream::connect(&sock).expect("connect");
    let mut reader = BufReader::new(&stream);
    handshake(&stream, &mut reader, request);
    old.join().unwrap();
    let (asked_tx, _asked) = std::sync::mpsc::channel();
    let successor = fake_server(
        aterm_uds::CtlListener::bind(&sock).unwrap(),
        sock.clone(),
        "OK subscribe 1\nsub 1 s-a\nBYTES 1 3\nxyz\nEVENT 1 exited\n",
        false,
        asked_tx,
    );
    let mut out = Vec::new();
    let code = relay_subscription(
        FirstLeg {
            stream: &stream,
            reader: &mut reader,
            dialed: &path,
            server: Some(gone_pid()),
            listener: None,
        },
        request,
        None,
        &Redial::new(Follow::Path(path.clone())).with_bound(REACHED),
        &mut out,
    )
    .expect("relay");
    stop(&sock);
    successor.join().unwrap();
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "sub 1 s-a\nBYTES 1 3\nabc\nGAP 1 replaced\nsub 1 s-a\nBYTES 1 3\nxyz\nEVENT 1 exited\n",
        "the hole is marked between the legs, before the successor's frames"
    );
}

/// What a forwarding aterm writes when the aterm it forwarded the connection
/// to went away under it: the host's pid, and the socket it was reached at.
#[cfg(unix)]
fn host_gone_marker(pid: u32, sock: &std::path::Path) -> String {
    format!("ERR gone pid={pid} sock={}\n", sock.display())
}

/// Publish `sid`'s graph entry in `dir`, naming `sock` — what an update's
/// successor does once it binds.
#[cfg(unix)]
fn publish_entry(dir: &std::path::Path, sid: &str, sock: &std::path::Path) {
    std::fs::create_dir_all(dir.join("graph")).unwrap();
    std::fs::write(
        dir.join("graph").join(sid),
        format!("sock {}\n", sock.display()),
    )
    .unwrap();
}

/// A FORWARDED STREAM FOLLOWS THE HOST THAT SERVES IT, NOT THE RELAY. A
/// sibling aterm (A) that forwards `subscribe @s-abc …` to the aterm hosting
/// the session (B) stays up when B self-updates and `_exit`s at its Commit,
/// so an EOF from a live A is no end on purpose. A says so on the stream
/// (`ERR gone pid=<B> sock=<B's socket>`, its last line), and the relay
/// follows B's successor where B's sessions are republished (`graph/<sid>`
/// beside B's socket): asked from the last block relayed, the marker never
/// reaching the consumer. With none there within the wait: exit 75, never 0.
/// FAILS WITHOUT THE FIX: the marker is relayed, A still answering reads as a
/// deliberate end, and the stream exits 0 — in both cases.
#[cfg(unix)]
#[test]
fn a_forwarded_stream_follows_its_hosts_successor_not_the_relay() {
    let request = "subscribe @s-abc events\n";
    for successor_up in [true, false] {
        let (dir, relay_sock) = scratch_sock("rd-fwd");
        let relay_path = relay_sock.to_str().unwrap().to_string();
        // B's socket: nothing answers there any more.
        let host = dir.path().join("b.sock");
        let gone = gone_pid();
        let reply: &'static str = Box::leak(
            format!(
                "OK subscribe 1\nsub 1 s-abc\nEVENT 1 block-complete 3 exit=0\n{}",
                host_gone_marker(gone, &host)
            )
            .into_boxed_str(),
        );
        let (seen_tx, _seen) = std::sync::mpsc::channel();
        // A: it answers the stream (B's frames, then its marker), closes
        // that connection, and goes on answering `version`.
        let relay = fake_server(
            aterm_uds::CtlListener::bind(&relay_sock).unwrap(),
            relay_sock.clone(),
            reply,
            false,
            seen_tx,
        );
        let (asked_tx, asked) = std::sync::mpsc::channel();
        let successor_sock = dir.path().join("b2.sock");
        let successor = successor_up.then(|| {
            let server = fake_server(
                aterm_uds::CtlListener::bind(&successor_sock).unwrap(),
                successor_sock.clone(),
                "OK subscribe 1\nsub 1 s-abc\nEVENT 1 exited\n",
                false,
                asked_tx,
            );
            publish_entry(dir.path(), "s-abc", &successor_sock);
            server
        });
        let stream = CtlStream::connect(&relay_sock).expect("connect");
        let mut reader = BufReader::new(&stream);
        handshake(&stream, &mut reader, request);
        let mut out = Vec::new();
        let code = relay_subscription(
            FirstLeg {
                stream: &stream,
                reader: &mut reader,
                dialed: &relay_path,
                server: Some(std::process::id()),
                listener: listener_at(&relay_path),
            },
            request,
            None,
            &Redial::new(Follow::Path(relay_path.clone())).with_bound(if successor_up {
                REACHED
            } else {
                Duration::from_millis(300)
            }),
            &mut out,
        )
        .expect("relay");
        stop(&relay_sock);
        relay.join().unwrap();
        if let Some(successor) = successor {
            stop(&successor_sock);
            successor.join().unwrap();
        }
        let out = String::from_utf8(out).unwrap();
        assert!(
            !out.contains("ERR gone"),
            "the marker is not a frame: {out}"
        );
        if successor_up {
            assert_eq!(
                asked.try_recv().as_deref(),
                Ok("subscribe @s-abc events since-block=3\n"),
                "B's successor was asked, from the last block relayed"
            );
            assert_eq!(code, ExitCode::SUCCESS, "{out}");
            assert!(
                out.ends_with("sub 1 s-abc\nEVENT 1 exited\n"),
                "the successor's leg was relayed: {out}"
            );
        } else {
            assert_eq!(code, ExitCode::from(EXIT_REPLACED), "never 0: {out}");
        }
    }
}

/// A leg that relays the session the stream followed, asked with `request`.
fn same_leg(request: &str) -> Ask {
    Ask {
        request: request.to_owned(),
        session: LegSession::Same,
    }
}

/// THE RESUME NOTE SAYS WHAT WAS NOT DELIVERED: it names the hole (`GAP
/// <local> replaced`), what is lost in it, and goes on from the last relayed
/// only for the events whose anchor the resume carried (a multi-target stream
/// carries none).
#[test]
fn the_resume_note_says_what_was_not_delivered() {
    let note = |asked: &str| {
        resumed_note(
            7,
            9,
            "/r/a.sock",
            asked,
            &same_leg(asked),
            Arrival::Predates,
        )
    };
    let both = note("subscribe @s-a bytes,events since-block=3 since-turn=2\n");
    assert!(
        both.contains("`GAP <local> replaced`") && both.contains("output, mail and records"),
        "{both}"
    );
    assert!(!both.contains("screen frames between"), "{both}");
    assert!(
        both.ends_with("its block and turn events go on after the last ones relayed"),
        "{both}"
    );
    assert!(note("subscribe @s-a events since-block=3\n").ends_with(
        "its block events go on after the last one relayed, and its turn events start \
             from the successor's newest"
    ));
    assert!(note("subscribe @s-a events since-turn=2\n").ends_with(
        "its turn events go on after the last one relayed, and its block events start \
             from the successor's newest"
    ));
    assert!(
        note("subscribe @s-a,@s-b events\n")
            .ends_with("its block and turn events start from the successor's newest")
    );
}

/// THE RESUME NOTE SAYS WHAT THE SUCCESSOR WAS, AS FAR AS IT IS KNOWN. One
/// running when the hang-up was seen is an update's successor; one that
/// started after it on the fixed socket and took the stream pinned to its
/// session is a relaunch that recovered the session (its keeper did), whose
/// block and turn counts started over, so some events between the two may
/// not have been delivered — the server replays after an anchor at or below
/// its own count, so the note does not say none were; one that took `@*` or
/// a self target never tied to a sid recovered nothing the note can name,
/// and it says a relaunch and no more; an unreadable or shared arrival is
/// not known to be either, and the note says only that aterm was replaced.
/// CONTROL: the update wording is unchanged.
#[test]
fn the_resume_note_says_what_the_successor_was_only_as_far_as_it_is_known() {
    let pinned = same_leg("subscribe @s-a events since-block=12 since-turn=4\n");
    let note = |arrival| resumed_note(7, 9, "/r/a.sock", "subscribe @. events\n", &pinned, arrival);
    let update = note(Arrival::Predates);
    assert!(
        update.contains("pid 7 -> 9, an update)") && !update.contains("fresh"),
        "{update}"
    );
    assert!(
        update.contains("go on after the last ones relayed"),
        "{update}"
    );
    let relaunch = note(Arrival::Fresh);
    assert!(
        relaunch.contains("a relaunch that recovered the session"),
        "{relaunch}"
    );
    assert!(!relaunch.contains("an update"), "{relaunch}");
    assert!(
        relaunch.contains(
            "its block and turn events start over with the relaunch's own count, so some \
             between the two may not have been delivered"
        ),
        "{relaunch}"
    );
    assert!(!relaunch.contains("live high"), "{relaunch}");
    // Unpinned, nothing recovered: `@*`, and a self stream whose `sub` line
    // tied no sid, resolve among the relaunch's own sessions.
    for asked in [
        "subscribe @* events\n",
        "subscribe @. events\n",
        "subscribe events\n",
    ] {
        let fresh = resumed_note(7, 9, "/r/a.sock", asked, &same_leg(asked), Arrival::Fresh);
        assert!(fresh.contains("(pid 7 -> 9, a relaunch);"), "{fresh}");
        assert!(!fresh.contains("recovered"), "{fresh}");
        assert!(
            fresh.ends_with("its block and turn events start from the relaunch's newest"),
            "{fresh}"
        );
    }
    // A stream that named several sessions by id carries no anchors, and a
    // relaunch that took it holds them.
    let named = resumed_note(
        7,
        9,
        "/r/a.sock",
        "subscribe @1,@2 events\n",
        &same_leg("subscribe @s-a,@s-b events\n"),
        Arrival::Fresh,
    );
    assert!(named.contains("a relaunch that recovered"), "{named}");
    for arrival in [Arrival::Unknown, Arrival::Shared] {
        let unknown = note(arrival);
        assert!(
            unknown.contains("aterm was replaced (pid 7 -> 9);"),
            "{unknown}"
        );
        assert!(
            !unknown.contains("update") && !unknown.contains("relaunch"),
            "{unknown}"
        );
    }
}

/// The moment [`a_later_successor_is_judged_against_the_hangup_it_follows`]
/// puts between two hang-ups: when its second successor "started".
#[cfg(unix)]
static BETWEEN: std::sync::Mutex<Option<Stamp>> = std::sync::Mutex::new(None);

/// A successor that started at [`BETWEEN`].
#[cfg(unix)]
fn started_between(_: u32) -> Option<Stamp> {
    *BETWEEN.lock().unwrap()
}

/// Each successor is judged against the hang-up it follows. A self stream on A
/// (`s-old`) moves to S1, a child process, and loses it too; then S2 (this
/// process) answers on the same fixed socket, lacks `s-old`, and started
/// between the two hang-ups ([`BETWEEN`]): an update successor of S1. Against
/// S1's hang-up it predates, so the stream ends 75 and S2 is asked only for
/// `s-old`. Two ways to lose S1: `after-a-leg` (S1 took the pinned request and
/// its stream hung up) and `after-a-failed-send` (S1 closed while a 4 MiB
/// request was still being written; it reads 32 bytes). Controls (`…, fresh`):
/// the same S2 started after both hang-ups is followed.
#[cfg(unix)]
#[test]
fn a_later_successor_is_judged_against_the_hangup_it_follows() {
    type Started = fn(u32) -> Option<Stamp>;
    let filler = "x".repeat(4 << 20);
    for (case, role, started, followed) in [
        ("after-a-leg", "carrier", started_between as Started, false),
        ("after-a-failed-send", "mute", started_between, false),
        ("after-a-leg, fresh", "carrier", started_later, true),
        ("after-a-failed-send, fresh", "mute", started_later, true),
    ] {
        let (dir, sock) = scratch_sock("rd-hung");
        let path = sock.to_str().unwrap().to_string();
        let s1 = ChildSuccessor::spawn(dir.path(), role, &sock);
        let request = if role == "mute" {
            format!("subscribe @. cursor,events {filler}\n")
        } else {
            "subscribe @. cursor,events\n".to_string()
        };
        let (seen_tx, seen) = std::sync::mpsc::channel();
        let old = fake_server(
            aterm_uds::CtlListener::bind(&sock).unwrap(),
            sock.clone(),
            "OK subscribe 1\nsub 0 s-old\nEVENT 0 block-complete 12 exit=0\n",
            true,
            seen_tx.clone(),
        );
        let stream = CtlStream::connect(&sock).expect("connect");
        let mut reader = BufReader::new(&stream);
        handshake(&stream, &mut reader, &request);
        old.join().unwrap();
        // A is gone; S1 takes its path.
        std::fs::rename(&s1.sock, &sock).expect("S1 takes the path");
        // S2: once S1 holds the stream, "starts", tells S1 to go, and binds
        // the path S1 leaves.
        let (s2_path, s1_seen, s1_go) = (sock.clone(), s1.seen.clone(), s1.go.clone());
        let s2 = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(60);
            while std::fs::metadata(&s1_seen).map_or(true, |m| m.len() == 0) {
                assert!(Instant::now() < deadline, "S1 was never asked");
                std::thread::sleep(Duration::from_millis(10));
            }
            *BETWEEN.lock().unwrap() = Some(Stamp::now());
            std::fs::write(&s1_go, b"").expect("tell S1 to go");
            while s2_path.exists() {
                assert!(Instant::now() < deadline, "S1 never left");
                std::thread::sleep(Duration::from_millis(10));
            }
            let listener = aterm_uds::CtlListener::bind(&s2_path).unwrap();
            serve_by(
                &listener,
                |asked| {
                    if asked.contains("@s-old") {
                        "ERR no such session\n"
                    } else {
                        "OK subscribe 1\nsub 4 s-new\nEVENT 4 exited\n"
                    }
                },
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
                listener: None,
            },
            &request,
            None,
            &Redial::new(Follow::Path(path.clone()))
                .with_bound(REACHED)
                .with_started(started),
            &mut out,
        )
        .expect("relay");
        stop(&sock);
        s2.join().unwrap();
        let brief = |r: String| r.replace(&filler, "<4 MiB>");
        let (s1_asked, s2_pinned, s2_self) = if role == "mute" {
            (
                "subscribe @s-old cursor,events x\n",
                "subscribe @s-old cursor,events <4 MiB> since-block=12\n",
                "subscribe @. cursor,events <4 MiB>\n",
            )
        } else {
            (
                "subscribe @s-old cursor,events since-block=12\n",
                "subscribe @s-old cursor,events since-block=12\n",
                "subscribe @. cursor,events\n",
            )
        };
        assert_eq!(s1.asked(), s1_asked, "{case}: what S1 was asked");
        let s2_asked: Vec<String> = seen.try_iter().skip(1).map(brief).collect();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(
            text.contains("sub 5 s-old"),
            role == "carrier",
            "{case}: S1's leg relayed only when it took the request: {text}"
        );
        if followed {
            assert_eq!(s2_asked, [s2_pinned, s2_self], "{case}: what S2 was asked");
            assert_eq!(code, ExitCode::SUCCESS, "{case}");
            assert!(text.contains("sub 4 s-new"), "{case}: {text}");
        } else {
            assert_eq!(s2_asked, [s2_pinned], "{case}: what S2 was asked");
            assert_eq!(code, ExitCode::from(EXIT_REPLACED), "{case}");
            assert!(!text.contains("sub 4"), "{case}: {text}");
        }
    }
}

/// A self stream moves only on the fixed socket the call named. Every
/// successor here is fresh and lacks `s-old`. On a fixed `--sock` path only a
/// relaunch of the instance configured there can bind, so the stream moves to
/// its own session (`fixed`). Elsewhere a fresh process may be another
/// window's (e.g. its update successor republishing the `latest` alias), so the
/// stream ends 75 and is never asked for its self there: `--sock` to the alias
/// (`alias`), the flagless follow of the alias from outside any session
/// (`alias-follow`), `--sock` to another link (`link`), to a plain socket named
/// as the alias (`alias-name`), or to an instance's own `aterm-<pid>.sock`
/// (`instance`). Nor does a call made inside a session whose stale graph entry
/// names a fixed socket a relaunch now holds (`graph-entry`). The rule is
/// [`only_a_relaunch_binds`] plus the `--sock` arm in [`Redial::arrival`].
#[cfg(unix)]
#[test]
fn a_self_stream_moves_only_on_the_fixed_socket_the_call_named() {
    let request = "subscribe @. cursor,events\n";
    let pinned = "subscribe @s-old cursor,events since-block=12\n";
    let (gone, own) = (gone_pid(), std::process::id());
    for case in [
        "fixed",
        "alias",
        "alias-follow",
        "link",
        "alias-name",
        "instance",
        "graph-entry",
    ] {
        let (_scratch, dir) = scratch_rendezvous(&format!("rd-where-{case}"));
        let alias = dir.join(super::super::SOCK_FILE);
        let old_instance = dir.join(format!("aterm-{gone}.sock"));
        let new_instance = dir.join(format!("aterm-{own}.sock"));
        let named = |p: &std::path::Path| p.to_str().unwrap().to_string();
        // (the old server's socket, the successor's, the path dialled (a link
        // to the old one's when it differs), and what the call follows)
        let (old_at, new_at, dialed, follow) = match case {
            "fixed" => {
                let fixed = dir.join("fixed.sock");
                (
                    fixed.clone(),
                    fixed.clone(),
                    fixed.clone(),
                    Follow::Path(named(&fixed)),
                )
            }
            "alias" => (
                old_instance.clone(),
                new_instance.clone(),
                alias.clone(),
                Follow::Path(named(&alias)),
            ),
            "alias-follow" => (
                old_instance.clone(),
                new_instance.clone(),
                alias.clone(),
                Follow::Flagless {
                    dir: dir.clone(),
                    self_sid: None,
                },
            ),
            "link" => {
                let link = dir.join("l.sock");
                (
                    dir.join("a.sock"),
                    dir.join("b.sock"),
                    link.clone(),
                    Follow::Path(named(&link)),
                )
            }
            "alias-name" => (
                alias.clone(),
                alias.clone(),
                alias.clone(),
                Follow::Path(named(&alias)),
            ),
            "graph-entry" => {
                let fixed = dir.join("fixed.sock");
                publish_graph_entry(&dir, "s-00ab", &fixed);
                (
                    fixed.clone(),
                    fixed.clone(),
                    fixed.clone(),
                    Follow::Flagless {
                        dir: dir.clone(),
                        self_sid: Some("s-00ab".to_string()),
                    },
                )
            }
            _ => (
                old_instance.clone(),
                old_instance.clone(),
                old_instance.clone(),
                Follow::Path(named(&old_instance)),
            ),
        };
        let (seen_tx, seen) = std::sync::mpsc::channel();
        let old = fake_server(
            aterm_uds::CtlListener::bind(&old_at).unwrap(),
            old_at.clone(),
            "OK subscribe 1\nsub 0 s-old\nEVENT 0 block-complete 12 exit=0\n",
            true,
            seen_tx.clone(),
        );
        if dialed != old_at {
            std::os::unix::fs::symlink(&old_at, &dialed).unwrap();
        }
        let stream = CtlStream::connect(&dialed).expect("connect");
        let mut reader = BufReader::new(&stream);
        handshake(&stream, &mut reader, request);
        old.join().unwrap();
        let (bind_at, link) = (new_at.clone(), (dialed != old_at).then(|| dialed.clone()));
        let successor = std::thread::spawn(move || {
            let listener = aterm_uds::CtlListener::bind(&bind_at).unwrap();
            if let Some(link) = link {
                aterm_uds::latest::publish(&link, bind_at.to_str().unwrap());
            }
            serve_by(
                &listener,
                |asked| {
                    if asked.contains("@s-old") {
                        "ERR no such session\n"
                    } else {
                        "OK subscribe 1\nsub 4 s-new\nEVENT 4 exited\n"
                    }
                },
                &seen_tx,
            );
        });
        let mut out = Vec::new();
        let code = relay_subscription(
            FirstLeg {
                stream: &stream,
                reader: &mut reader,
                dialed: &named(&dialed),
                server: Some(gone),
                listener: None,
            },
            request,
            None,
            &Redial::new(follow)
                .with_bound(REACHED)
                .with_started(started_later),
            &mut out,
        )
        .expect("relay");
        stop(&new_at);
        successor.join().unwrap();
        let asked: Vec<String> = seen.try_iter().skip(1).collect();
        let text = String::from_utf8(out).unwrap();
        if case == "fixed" {
            assert_eq!(asked, [pinned, request], "{case}");
            assert_eq!(code, ExitCode::SUCCESS, "{case}");
            assert!(text.contains("sub 4 s-new"), "{case}: {text}");
        } else {
            assert_eq!(asked, [pinned], "{case}: never asked for its self");
            assert_eq!(code, ExitCode::from(EXIT_REPLACED), "{case}");
            assert!(!text.contains("sub 4"), "{case}: {text}");
        }
    }
}

/// A successor's start for [`a_refusal_is_judged_by_the_process_that_gave_it`]:
/// this process started after the hang-up, any other long before it.
#[cfg(unix)]
fn fresh_only_here(pid: u32) -> Option<Stamp> {
    if pid == std::process::id() {
        started_later(pid)
    } else {
        Some(Stamp {
            boot: Some(BootClock(0)),
            wall: Some(UNIX_EPOCH),
        })
    }
}

/// The process asked is the one judged: every ask is a new connection, and the
/// socket can change hands after the probe. P1 (this process, fresh) answers
/// the probe on the fixed socket, then P2 (a child already running at the
/// hang-up) has its socket renamed over P1's before the ask. P2 lacks `s-old`
/// and predates the hang-up, so the stream ends 75 and P2 is asked only for
/// `s-old`. Control (`both fresh`): with P2 fresh too, the hand-over is
/// followed to P2's own session.
#[cfg(unix)]
#[test]
fn a_refusal_is_judged_by_the_process_that_gave_it() {
    type Started = fn(u32) -> Option<Stamp>;
    let request = "subscribe @. cursor,events\n";
    let pinned = "subscribe @s-old cursor,events since-block=12\n";
    for (case, started, followed) in [
        ("handed over", fresh_only_here as Started, false),
        ("both fresh", started_later, true),
    ] {
        let (dir, old_sock) = scratch_sock("rd-asked");
        let fixed = dir.path().join("fixed.sock");
        let p2 = ChildSuccessor::spawn(dir.path(), "refuses", &fixed);
        let (seen_tx, seen) = std::sync::mpsc::channel();
        let old = fake_server(
            aterm_uds::CtlListener::bind(&old_sock).unwrap(),
            old_sock.clone(),
            "OK subscribe 1\nsub 0 s-old\nEVENT 0 block-complete 12 exit=0\n",
            true,
            seen_tx.clone(),
        );
        let stream = CtlStream::connect(&old_sock).expect("connect");
        let mut reader = BufReader::new(&stream);
        handshake(&stream, &mut reader, request);
        old.join().unwrap();
        // P1 answers one probe, handing the path to P2 first; a request it
        // is asked is recorded (none should be).
        let p1_listener = aterm_uds::CtlListener::bind(&fixed).unwrap();
        let (p1_path, p2_sock) = (fixed.clone(), p2.sock.clone());
        let p1 = std::thread::spawn(move || {
            loop {
                let Ok((conn, _)) = p1_listener.accept() else {
                    return;
                };
                let mut reader = std::io::BufReader::new(&conn);
                let mut request = String::new();
                if reader.read_line(&mut request).unwrap_or(0) == 0 {
                    continue;
                }
                if request.starts_with("version") {
                    std::fs::rename(&p2_sock, &p1_path).expect("P2 takes the path");
                    let _ = (&conn).write_all(b"OK aterm test\n");
                    return;
                }
                let _ = seen_tx.send(request);
                let _ = (&conn).write_all(b"ERR not this one\n");
            }
        });
        let mut out = Vec::new();
        let code = relay_subscription(
            FirstLeg {
                stream: &stream,
                reader: &mut reader,
                dialed: old_sock.to_str().unwrap(),
                server: Some(gone_pid()),
                listener: None,
            },
            request,
            None,
            &Redial::new(Follow::Path(fixed.to_str().unwrap().to_string()))
                .with_bound(REACHED)
                .with_started(started),
            &mut out,
        )
        .expect("relay");
        p1.join().unwrap();
        assert_eq!(
            seen.try_iter().skip(1).count(),
            0,
            "{case}: P1 was never asked"
        );
        let text = String::from_utf8(out).unwrap();
        if followed {
            assert_eq!(p2.asked(), format!("{pinned}{request}"), "{case}");
            assert_eq!(code, ExitCode::SUCCESS, "{case}");
            assert!(text.contains("sub 5 s-new"), "{case}: {text}");
        } else {
            assert_eq!(p2.asked(), pinned, "{case}: P2 asked for `s-old` alone");
            assert_eq!(code, ExitCode::from(EXIT_REPLACED), "{case}");
            assert!(!text.contains("sub 5"), "{case}: {text}");
        }
    }
}

/// A leg resumed on a fresh instance's own session says it was a relaunch
/// without the stream's session, and names self as the caller wrote it —
/// `@.`, `@`, or no selector — never `@.` for the other two. One that did not
/// move names the instance answering now and that it lacks the stream's
/// session, in one line whatever the arrival was; the same pid back on a fresh
/// listener reads as that, not as two processes. NEGATIVE CONTROL: neither
/// reads "replaced", nor explains the clocks it was judged by. (The
/// carried-session leg still reads "an update", never "fresh":
/// [`the_resume_note_says_what_the_successor_was_only_as_far_as_it_is_known`].)
#[test]
fn the_notes_say_what_the_successor_was() {
    for (original, words) in [
        ("subscribe @. events\n", "(`@.` resolved there)"),
        ("@ subscribe events\n", "(`@` resolved there)"),
        (
            "subscribe events\n",
            "(the request names no session, and it resolved there)",
        ),
    ] {
        let note = resumed_note(
            7,
            9,
            "/r/a.sock",
            original,
            &Ask {
                request: original.to_owned(),
                session: LegSession::Other,
            },
            Arrival::Predates,
        );
        assert!(
            note.starts_with(
                "aterm was replaced (pid 7 -> 9, a relaunch without this stream's session); \
                 the stream resumed on /r/a.sock"
            ) && note.contains("(`GAP <local> replaced` on each target)"),
            "{note}"
        );
        assert!(note.contains(words), "{original:?}: {note}");
        assert_eq!(note.contains("@."), original.contains("@."), "{note}");
    }
    for why in [Unresumed::Predates, Unresumed::Unknown, Unresumed::Shared] {
        assert_eq!(
            unresumed_note(7, 9, "/r/a.sock", why),
            "aterm (pid 7) went away; /r/a.sock now answers as pid 9, which does not hold the \
             session this stream followed; re-issue it"
        );
        assert_eq!(
            unresumed_note(7, 7, "/r/a.sock", why),
            "aterm (pid 7) reopened its control socket without the session this stream \
             followed; re-issue it"
        );
    }
    assert_eq!(
        unresumed_note(7, 9, "/r/a.sock", Unresumed::Busy),
        "aterm was replaced (pid 7 -> 9), and its successor stayed too busy to resume this \
         subscription; re-issue it"
    );
}

/// A successor's start is the kernel's record, on the boot clock the hang-up
/// is read from (macOS and Linux). A process spawned after a moment reads as
/// [`Arrival::Fresh`] against it, this test process (started before) as
/// [`Arrival::Predates`]; an ended process has no start. The child is spawned
/// two Linux clock ticks after the moment, since Linux records a start by its
/// tick. Where the platform keeps no start record every start is unknown and
/// the relay fails closed ([`Arrival::Unknown`]).
#[cfg(unix)]
#[test]
fn a_process_started_after_a_moment_is_fresh_against_it() {
    let moment = Stamp::now();
    std::thread::sleep(Duration::from_millis(20));
    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn /bin/sleep");
    let child_started = process_started(child.id());
    let own_started = process_started(std::process::id());
    let _ = child.kill();
    let _ = child.wait();
    if cfg!(any(target_vendor = "apple", target_os = "linux")) {
        assert!(
            child_started.is_some_and(|s| s.boot.is_some()),
            "a boot-clock start: {child_started:?}"
        );
        assert_eq!(
            arrival_of(child_started, moment),
            Arrival::Fresh,
            "{child_started:?} against {moment:?}"
        );
        assert_eq!(
            arrival_of(own_started, moment),
            Arrival::Predates,
            "{own_started:?} against {moment:?}"
        );
        assert_eq!(process_started(gone_pid()), None, "an ended process");
    } else {
        assert_eq!(arrival_of(child_started, moment), Arrival::Unknown);
    }
}

/// A self stream (`subscribe @. cursor,events`) on an in-process server at a
/// fixed `--sock` path hangs up; a successor in this process then answers on
/// that path, lacking `s-old` (`ERR no such session` to the pinned resume) and
/// serving its own `s-new` otherwise. Returns what `redial` (with the test's
/// clock and start record) makes of it: exit code, requests seen, stdout.
#[cfg(unix)]
fn relay_to_a_successor_without_the_session(
    tag: &str,
    redial: impl FnOnce(Redial) -> Redial,
) -> (ExitCode, Vec<String>, String) {
    let request = "subscribe @. cursor,events\n";
    let (_dir, sock) = scratch_sock(tag);
    let path = sock.to_str().unwrap().to_string();
    let (seen_tx, seen) = std::sync::mpsc::channel();
    let old = fake_server(
        aterm_uds::CtlListener::bind(&sock).unwrap(),
        sock.clone(),
        "OK subscribe 1\nsub 0 s-old\nEVENT 0 block-complete 12 exit=0\n",
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
        serve_by(
            &listener,
            |asked| {
                if asked.contains("@s-old") {
                    "ERR no such session\n"
                } else {
                    "OK subscribe 1\nsub 4 s-new\nEVENT 4 exited\n"
                }
            },
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
            listener: None,
        },
        request,
        None,
        &redial(Redial::new(Follow::Path(path.clone())).with_bound(REACHED)),
        &mut out,
    )
    .expect("relay");
    stop(&sock);
    successor.join().unwrap();
    let asked = seen.try_iter().skip(1).collect();
    (code, asked, String::from_utf8(out).unwrap())
}

/// A hang-up read on a Linux boot clock ([`linux_clock`]): 1234.567 89 s after
/// boot, inside clock tick 123456 at 100 Hz.
const LINUX_HANGUP: u64 = 1_234_567_890_000;

/// This relay's clock on Linux: the boot clock alone (`CLOCK_BOOTTIME`, in
/// nanoseconds), read at [`LINUX_HANGUP`].
fn linux_clock() -> Stamp {
    Stamp {
        boot: Some(BootClock(LINUX_HANGUP)),
        wall: None,
    }
}

/// `pid`'s start as Linux records it (a `/proc/<pid>/stat` line with `state`
/// and `starttime` `ticks`, and a `status` with effective uid 501), read through
/// the Linux relay's parser by a uid-501 reader at 100 Hz.
fn linux_started(pid: u32, ticks: u64, state: char) -> Option<Stamp> {
    let stat = format!(
        "{pid} (aterm) {state} 1 {pid} {pid} 0 -1 4194368 63 0 0 0 0 0 0 0 20 0 1 0 {ticks} \
         7032832 463 18446744073709551615 205458244435968 205458248030472 281474793899808 0 0 0 \
         0 128 0 1 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n"
    );
    let status = "Name:\taterm\nUid:\t501\t501\t501\t501\n";
    let boot = aterm_uds::process::procfs::start(
        &stat,
        status,
        &stat,
        i32::try_from(pid).ok()?,
        501,
        100,
    )?;
    Some(Stamp {
        boot: Some(boot),
        wall: None,
    })
}

/// Started in the tick after the hang-up's.
fn linux_next_tick(pid: u32) -> Option<Stamp> {
    linux_started(pid, 123_457, 'S')
}

/// Started in the hang-up's own tick — before or after it, the record cannot
/// say.
fn linux_same_tick(pid: u32) -> Option<Stamp> {
    linux_started(pid, 123_456, 'S')
}

/// Started well before the hang-up.
fn linux_earlier(pid: u32) -> Option<Stamp> {
    linux_started(pid, 120_000, 'S')
}

/// A record whose process has already ended.
fn linux_zombie(pid: u32) -> Option<Stamp> {
    linux_started(pid, 123_457, 'Z')
}

/// The Linux start record moves a self stream only for a relaunch. Run on any
/// host through the relay, with the relay's clock and the successor's start
/// read as Linux reads them (boot clock in nanoseconds, `/proc/<pid>/stat`
/// field 22 in ticks, [`linux_started`]). A successor started in the tick after
/// the hang-up's is a relaunch: asked again unpinned, and the stream continues
/// with its `s-new` (exit 0). Negative controls, each ending 75 with only the
/// pinned `s-old` asked: a start well before the hang-up; a start in the
/// hang-up's own tick (read as the tick's first instant); and a record of an
/// ended process. On Linux the same order is driven with real processes in
/// [`a_process_started_after_a_moment_is_fresh_against_it`] and
/// [`a_relaunch_between_the_old_servers_exit_and_the_relays_read_is_fresh`].
#[cfg(unix)]
#[test]
fn a_linux_start_record_moves_a_self_stream_only_for_a_relaunch() {
    let request = "subscribe @. cursor,events\n";
    let pinned = "subscribe @s-old cursor,events since-block=12\n";
    type Started = fn(u32) -> Option<Stamp>;
    for (case, started, followed) in [
        ("next tick", linux_next_tick as Started, true),
        ("same tick", linux_same_tick, false),
        ("earlier", linux_earlier, false),
        ("zombie", linux_zombie, false),
    ] {
        let (code, asked, text) = relay_to_a_successor_without_the_session(
            &format!("rd-linux-{}", case.replace(' ', "-")),
            |r| r.with_clock(linux_clock).with_started(started),
        );
        if followed {
            assert_eq!(asked, [pinned, request], "{case}");
            assert_eq!(code, ExitCode::SUCCESS, "{case}");
            assert!(text.contains("sub 4 s-new"), "{case}: {text}");
        } else {
            assert_eq!(asked, [pinned], "{case}: never asked for its self");
            assert_eq!(code, ExitCode::from(EXIT_REPLACED), "{case}");
            assert!(!text.contains("sub 4"), "{case}: {text}");
        }
    }
}

/// A fixed wall-clock moment the clock stand-ins below read around.
fn wall_at(seconds: u64) -> Option<SystemTime> {
    UNIX_EPOCH.checked_add(Duration::from_secs(1_800_000_000 + seconds))
}

/// A hang-up as the x86_64 slice reads it under ROSETTA
/// ([`aterm_uds::process::boot_clock_now`] answers `None` there): the wall
/// clock alone, at [`wall_at`] 10.
fn rosetta_clock() -> Stamp {
    Stamp {
        boot: None,
        wall: wall_at(10),
    }
}

/// The same hang-up had the translated `mach_absolute_time` (nanoseconds) been
/// taken for the boot clock.
fn rosetta_clock_in_nanoseconds() -> Stamp {
    Stamp {
        boot: Some(BootClock(705_901_910_571_583)),
        wall: wall_at(10),
    }
}

/// A relaunch's start as the kernel records it on Apple silicon, in its own
/// 24 MHz ticks, with its wall record after [`rosetta_clock`]'s.
fn started_in_kernel_ticks_after(_: u32) -> Option<Stamp> {
    Some(Stamp {
        boot: Some(BootClock(16_941_646_465_128)),
        wall: wall_at(11),
    })
}

/// The same record, its wall time before the hang-up's: already running.
fn started_in_kernel_ticks_before(_: u32) -> Option<Stamp> {
    Some(Stamp {
        boot: Some(BootClock(16_941_646_465_128)),
        wall: wall_at(9),
    })
}

/// A relay under Rosetta orders a relaunch on the wall clock: the translated
/// x86_64 slice reads no boot clock ([`rosetta_clock`]) while start records
/// are in kernel ticks. `relaunch` (wall later) is followed; `already running`
/// (wall earlier) ends 75. Control: the translated reading taken for the boot
/// clock ([`rosetta_clock_in_nanoseconds`]) puts the hang-up ×125/3 after every
/// start, and the same relaunch ends 75. The withholding itself is
/// [`aterm_uds::process::apple_boot_clock`]'s, tested by
/// `a_translated_process_reads_no_boot_clock`.
#[cfg(unix)]
#[test]
fn a_relay_under_rosetta_orders_a_relaunch_on_the_wall_clock() {
    let request = "subscribe @. cursor,events\n";
    let pinned = "subscribe @s-old cursor,events since-block=12\n";
    type Clock = fn() -> Stamp;
    type Started = fn(u32) -> Option<Stamp>;
    for (case, clock, started, followed) in [
        (
            "relaunch",
            rosetta_clock as Clock,
            started_in_kernel_ticks_after as Started,
            true,
        ),
        (
            "already running",
            rosetta_clock,
            started_in_kernel_ticks_before,
            false,
        ),
        (
            "nanoseconds as boot ticks",
            rosetta_clock_in_nanoseconds,
            started_in_kernel_ticks_after,
            false,
        ),
    ] {
        let (code, asked, text) = relay_to_a_successor_without_the_session(
            &format!("rd-rosetta-{}", case.replace(' ', "-")),
            |r| r.with_clock(clock).with_started(started),
        );
        if followed {
            assert_eq!(asked, [pinned, request], "{case}");
            assert_eq!(code, ExitCode::SUCCESS, "{case}");
            assert!(text.contains("sub 4 s-new"), "{case}: {text}");
        } else {
            assert_eq!(asked, [pinned], "{case}: never asked for its self");
            assert_eq!(code, ExitCode::from(EXIT_REPLACED), "{case}");
            assert!(!text.contains("sub 4"), "{case}: {text}");
        }
    }
}

/// THE ORDER, pure ([`later`], [`arrival_of`], [`earliest`]) — what
/// [`Redial::arrival`] judges every relay's successor by: the boot clock
/// decides wherever both sides were read on it, whatever the wall clock says
/// (the first two cases: a wall clock stepped back would read a relaunch as
/// already running, one stepped ahead an update's successor as a relaunch);
/// the wall clock only where one side has no boot reading; nothing where the
/// two share no clock (a Linux start, boot-only, against a hang-up read
/// without a boot clock) — [`Arrival::Unknown`], which ends the stream. A
/// hang-up is the EARLIER of the relay's read and the exit's stamp, each of
/// which is at or after the exit; the read alone when there is no stamp or
/// the two cannot be ordered.
#[test]
fn the_boot_clock_orders_a_start_and_a_hangup_wherever_both_have_it() {
    let at = |boot: Option<u64>, wall: Option<u64>| Stamp {
        boot: boot.map(BootClock),
        wall: wall.and_then(wall_at),
    };
    let hung_up = at(Some(1_000), Some(3_600));
    for (case, start, arrival) in [
        (
            "boot later, wall earlier",
            at(Some(2_000), Some(0)),
            Arrival::Fresh,
        ),
        (
            "boot earlier, wall later",
            at(Some(500), Some(7_200)),
            Arrival::Predates,
        ),
        (
            "boot equal",
            at(Some(1_000), Some(7_200)),
            Arrival::Predates,
        ),
        ("wall only, later", at(None, Some(7_200)), Arrival::Fresh),
        ("wall only, earlier", at(None, Some(0)), Arrival::Predates),
        ("boot only, later", at(Some(2_000), None), Arrival::Fresh),
    ] {
        assert_eq!(arrival_of(Some(start), hung_up), arrival, "{case}");
    }
    assert_eq!(
        arrival_of(Some(at(Some(2_000), None)), at(None, Some(0))),
        Arrival::Unknown,
        "no shared clock"
    );
    assert_eq!(arrival_of(None, hung_up), Arrival::Unknown, "no record");

    let seen = at(Some(5_000), Some(100));
    assert_eq!(earliest(seen, None), seen, "no exit stamp: the read");
    let exited = at(Some(4_000), Some(200));
    assert_eq!(earliest(seen, Some(exited)), exited, "the exit came first");
    let after = at(Some(6_000), Some(50));
    assert_eq!(earliest(seen, Some(after)), seen, "an exit stamped later");
    let foreign = at(None, None);
    assert_eq!(earliest(seen, Some(foreign)), seen, "cannot be ordered");
}

/// A consumer that stops reading at the first write holding its trigger: that
/// write and all later ones block until [`Stall::release`], so a relay writing
/// to it cannot read its server's hang-up before then. [`Stall::blocked`]
/// waits until one blocks.
#[cfg(unix)]
#[derive(Clone)]
struct Stall(std::sync::Arc<(std::sync::Mutex<StallState>, std::sync::Condvar)>);

#[cfg(unix)]
struct StallState {
    trigger: &'static str,
    released: bool,
    blocked: bool,
    bytes: Vec<u8>,
}

#[cfg(unix)]
impl Stall {
    /// A consumer that stops at the first write holding `trigger`.
    fn at(trigger: &'static str) -> Self {
        Self(std::sync::Arc::new((
            std::sync::Mutex::new(StallState {
                trigger,
                released: false,
                blocked: false,
                bytes: Vec::new(),
            }),
            std::sync::Condvar::new(),
        )))
    }

    /// Wait (bounded, a hang detector) until a write is blocked.
    fn blocked(&self) {
        let (lock, wake) = &*self.0;
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut state = lock.lock().unwrap();
        while !state.blocked {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "the relay never wrote");
            state = wake.wait_timeout(state, left).unwrap().0;
        }
    }

    /// Let every write through.
    fn release(&self) {
        let (lock, wake) = &*self.0;
        lock.lock().unwrap().released = true;
        wake.notify_all();
    }

    /// What was written, once released.
    fn text(&self) -> String {
        String::from_utf8(self.0.0.lock().unwrap().bytes.clone()).unwrap()
    }
}

/// Releases its [`Stall`] when dropped, including when a test fails while the
/// relay is blocked on it.
#[cfg(unix)]
struct Drains<'a>(&'a Stall);

#[cfg(unix)]
impl Drop for Drains<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[cfg(unix)]
impl io::Write for Stall {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let (lock, wake) = &*self.0;
        let mut state = lock.lock().unwrap();
        if !state.blocked && String::from_utf8_lossy(buf).contains(state.trigger) {
            state.blocked = true;
            wake.notify_all();
        }
        while state.blocked && !state.released {
            state = wake.wait(state).unwrap();
        }
        state.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Every exit an [`ExitMoment`] stamped through [`noting_clock`]: the
/// thread's name (which names the pid), and the stamp.
#[cfg(unix)]
static EXIT_STAMPS: std::sync::Mutex<Vec<(String, Stamp)>> = std::sync::Mutex::new(Vec::new());

/// The real clock, recording each read an exit watch's thread makes, so
/// [`a_relaunch_between_the_old_servers_exit_and_the_relays_read_is_fresh`]
/// knows the exit is stamped before it starts the relaunch.
#[cfg(unix)]
fn noting_clock() -> Stamp {
    let now = Stamp::now();
    if let Some(name) = std::thread::current().name()
        && name.starts_with("aterm-exit-moment-")
    {
        EXIT_STAMPS.lock().unwrap().push((name.to_string(), now));
    }
    now
}

/// Wait (bounded, a hang detector) until the exit of `pid` is stamped
/// through [`noting_clock`].
#[cfg(unix)]
fn exit_stamped(pid: u32) {
    let name = aterm_uds::exitwatch::exit_moment_thread(pid);
    let deadline = Instant::now() + Duration::from_secs(60);
    while !EXIT_STAMPS.lock().unwrap().iter().any(|(n, _)| *n == name) {
        assert!(
            Instant::now() < deadline,
            "the exit of {pid} was never stamped"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Each server whose exit a relay under test registered a watch on
/// ([`note_watch`]).
#[cfg(unix)]
static WATCHED: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

/// [`Redial::with_watch_note`]'s note for the tests below: records the pid.
#[cfg(unix)]
fn note_watch(pid: u32) {
    WATCHED.lock().unwrap().push(pid);
}

/// Whether a relay under test registered a watch on `pid`'s exit.
#[cfg(unix)]
fn watched(pid: u32) -> bool {
    WATCHED.lock().unwrap().contains(&pid)
}

/// Crash `server` and, where the relay watches its exit ([`watched`]), wait
/// until the exit is stamped, so a relaunch started next starts after the
/// stamp. Without a watch the test goes straight on, and a missing watch shows
/// in what the relay does (it judges against its own late read and ends 75).
#[cfg(unix)]
fn crash_and_let_its_watch_stamp_it(server: &mut ChildSuccessor) {
    let pid = server.pid();
    let watch = watched(pid);
    server.crash();
    if watch {
        exit_stamped(pid);
    }
}

/// A relaunch between the old server's exit and the relay's read is fresh.
/// The old instance is a real process on the fixed socket; it is `SIGKILL`ed
/// while the relay is blocked writing to a stalled consumer ([`Stall`]), and a
/// real relaunch starts after the crash and before the relay reads the EOF.
/// Judged against the exit the kernel reported to the relay's watch it is
/// fresh, and the stream continues with its `s-new`. `relaunch, orphaned
/// server`: the same with the old instance an orphan, so the watch is
/// registered on a same-uid process that is not this one's child, as in
/// production (Darwin gates `NOTE_EXITSTATUS` on parentage or the right to
/// signal). Controls, each ending 75 with only `s-old` asked: `already
/// running` (the successor starts after the watch is registered and before the
/// crash, as in a real update, so it predates the exit; a watch stamping at
/// registration would get this wrong), and `no watch` (the old server was gone
/// before a watch could be registered, so the relay's own read decides).
#[cfg(unix)]
#[test]
fn a_relaunch_between_the_old_servers_exit_and_the_relays_read_is_fresh() {
    let request = "subscribe @. cursor,events\n";
    let pinned = "subscribe @s-old cursor,events since-block=12\n";
    for (case, orphan, followed) in [
        ("relaunch", false, true),
        ("relaunch, orphaned server", true, true),
        ("already running", false, false),
        ("no watch", false, false),
    ] {
        let (old_dir, sock) = scratch_sock("rd-exit-old");
        let (new_dir, _) = scratch_sock("rd-exit-new");
        let mut old = if orphan {
            ChildSuccessor::spawn_orphan(old_dir.path(), "old", &sock)
        } else {
            ChildSuccessor::spawn(old_dir.path(), "old", &sock)
        };
        let old_pid = old.pid();
        if let Served::Orphan(process) = &old.process {
            let parent = process.parent();
            assert!(
                parent.is_some_and(|parent| parent != std::process::id()),
                "{case}: the old instance is not this process's child: parent {parent:?}"
            );
        }
        std::fs::rename(&old.sock, &sock).expect("the old instance takes the path");
        let path = sock.to_str().unwrap().to_string();
        let stream = CtlStream::connect(&sock).expect("connect");
        let server = server_pid(&stream);
        assert_eq!(server, Some(old_pid), "{case}: the old instance serves");
        let mut reader = BufReader::new(&stream);
        handshake(&stream, &mut reader, request);
        if case == "no watch" {
            old.crash();
        }
        let stall = Stall::at("sub 0 s-old");
        let redial = Redial::new(Follow::Path(path.clone()))
            .with_bound(REACHED)
            .with_clock(noting_clock)
            .with_watch_note(note_watch);
        let mut out = stall.clone();
        let (code, successor) = std::thread::scope(|scope| {
            let relay = scope.spawn(|| {
                relay_subscription(
                    FirstLeg {
                        stream: &stream,
                        reader: &mut reader,
                        dialed: &path,
                        server,
                        listener: None,
                    },
                    request,
                    None,
                    &redial,
                    &mut out,
                )
                .expect("relay")
            });
            let _drains = Drains(&stall);
            // The relay holds its frames; any watch it registers is
            // registered before it relays a frame.
            stall.blocked();
            // An update's successor: up while the old server still serves
            // the stream.
            let early = (case == "already running")
                .then(|| ChildSuccessor::spawn(new_dir.path(), "refuses", &sock));
            if case != "no watch" {
                crash_and_let_its_watch_stamp_it(&mut old);
            }
            // Two Linux clock ticks: a start is recorded by its tick.
            std::thread::sleep(Duration::from_millis(20));
            let successor =
                early.unwrap_or_else(|| ChildSuccessor::spawn(new_dir.path(), "refuses", &sock));
            std::fs::rename(&successor.sock, &sock).expect("the successor takes the path");
            stall.release();
            (relay.join().unwrap(), successor)
        });
        let text = stall.text();
        assert!(
            text.contains("sub 0 s-old"),
            "{case}: the first leg: {text}"
        );
        if followed {
            assert_eq!(successor.asked(), format!("{pinned}{request}"), "{case}");
            assert_eq!(code, ExitCode::SUCCESS, "{case}");
            assert!(text.contains("sub 5 s-new"), "{case}: {text}");
        } else {
            assert_eq!(
                successor.asked(),
                pinned,
                "{case}: never asked for its self"
            );
            assert_eq!(code, ExitCode::from(EXIT_REPLACED), "{case}");
            assert!(!text.contains("sub 5"), "{case}: {text}");
        }
        assert_eq!(
            watched(old_pid),
            case != "no watch",
            "{case}: the relay watched the old instance's exit"
        );
    }
}

/// Each leg's server is watched from its ask: a self stream rides an update to
/// S1, a real process that carried `s-old`; S1 then crashes while the relay is
/// blocked writing its frames, and a relaunch R starts before the relay reads
/// the EOF. R is judged against S1's exit as reported to the watch registered
/// at the ask, so it is fresh and the stream continues with R's `s-new`. The
/// first leg's watch is covered by
/// [`a_relaunch_between_the_old_servers_exit_and_the_relays_read_is_fresh`].
#[cfg(unix)]
#[test]
fn a_relaunch_after_a_later_legs_server_crashed_is_judged_by_that_exit() {
    let request = "subscribe @. cursor,events\n";
    let pinned = "subscribe @s-old cursor,events since-block=12\n";
    let (s1_dir, sock) = scratch_sock("rd-exit-s1");
    let (r_dir, _) = scratch_sock("rd-exit-r");
    let path = sock.to_str().unwrap().to_string();
    let mut s1 = ChildSuccessor::spawn(s1_dir.path(), "carrier", &sock);
    let (seen_tx, _seen) = std::sync::mpsc::channel();
    let old = fake_server(
        aterm_uds::CtlListener::bind(&sock).unwrap(),
        sock.clone(),
        "OK subscribe 1\nsub 0 s-old\nEVENT 0 block-complete 12 exit=0\n",
        true,
        seen_tx,
    );
    let stream = CtlStream::connect(&sock).expect("connect");
    let mut reader = BufReader::new(&stream);
    handshake(&stream, &mut reader, request);
    old.join().unwrap();
    // The old instance is gone; S1, its update's successor, takes the path.
    std::fs::rename(&s1.sock, &sock).expect("S1 takes the path");
    let stall = Stall::at("sub 5 s-old");
    let redial = Redial::new(Follow::Path(path.clone()))
        .with_bound(REACHED)
        .with_clock(noting_clock)
        .with_watch_note(note_watch);
    let mut out = stall.clone();
    let (code, relaunch) = std::thread::scope(|scope| {
        let relay = scope.spawn(|| {
            relay_subscription(
                FirstLeg {
                    stream: &stream,
                    reader: &mut reader,
                    dialed: &path,
                    server: Some(gone_pid()),
                    listener: None,
                },
                request,
                None,
                &redial,
                &mut out,
            )
            .expect("relay")
        });
        let _drains = Drains(&stall);
        // The relay holds S1's frames, S1 having taken the pinned request.
        stall.blocked();
        crash_and_let_its_watch_stamp_it(&mut s1);
        std::thread::sleep(Duration::from_millis(20));
        let relaunch = ChildSuccessor::spawn(r_dir.path(), "refuses", &sock);
        std::fs::rename(&relaunch.sock, &sock).expect("R takes the path");
        stall.release();
        (relay.join().unwrap(), relaunch)
    });
    let text = stall.text();
    assert_eq!(s1.asked(), pinned, "S1 took the pinned request");
    assert!(text.contains("sub 5 s-old"), "S1's leg: {text}");
    assert_eq!(
        relaunch.asked(),
        format!("{pinned}{request}"),
        "what R was asked"
    );
    assert_eq!(code, ExitCode::SUCCESS, "R's `exited`");
    assert!(text.contains("sub 5 s-new"), "R's leg: {text}");
}

/// A local number is an old-process anchor: a successor numbers adopted
/// sessions afresh. Each local target is rewritten to the sid the stream's
/// `sub <n> <sid>` line named; one never tied to a sid cannot be followed.
/// Negative control: sid, self and roster targets pass unchanged.
#[test]
fn a_resumed_subscribe_names_a_local_target_by_its_sid() {
    let tied = [
        ("3".to_string(), "s-aaaa".to_string()),
        ("5".to_string(), "s-bbbb".to_string()),
    ];
    assert_eq!(
        resume_request(
            "subscribe @3 screen,events since=9\n",
            &tied,
            &EventAnchors::default()
        )
        .as_deref(),
        Some("subscribe @s-aaaa screen,events\n")
    );
    assert_eq!(
        resume_request(
            "subscribe @3,@s-cccc,@.,@5 events\n",
            &tied,
            &EventAnchors::default()
        )
        .as_deref(),
        Some("subscribe @s-aaaa,@s-cccc,@.,@s-bbbb events\n")
    );
    assert_eq!(
        resume_request("subscribe @4 events\n", &tied, &EventAnchors::default()),
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
        "inbox --peek",
        "inbox 16 --meta",
        "await idle 500",
        "await match prompt",
        "await inbox",
    ] {
        assert!(retries_across_replacement(&parts(yes)), "{yes}");
        assert!(is_blocking_read(&parts(yes)), "{yes}");
    }
    for no in [
        // Anchored at the moment of the arm: the event may have happened on the
        // successor before a retry could arm.
        "await seq",
        "await seq timeout=0",
        "await block",
        "@s-a await block timeout=600000",
        "await consent",
        "await seq 40",
        "await inbox since=12",
        "inbox get 4",
        "inbox since=7",
        "inbox 16 since=7",
        "@s-a await seq 3 timeout=500",
        // A local number names a session of the old process.
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

/// A `turn` or a `post` cut by its server going away is reported as a
/// replacement — exit 75 with a note that its text may already have been typed,
/// or the post queued and carried — never the bare exit-1 transport error a
/// caller retries (typing the prompt twice, or sending a duplicate beside the
/// carried copy). It is a write, so it is never asked again. NEGATIVE CONTROL:
/// the other writes keep their old error.
#[test]
fn a_cut_turn_or_post_is_reported_as_a_replacement_and_never_asked_again() {
    let parts = |s: &str| -> Vec<String> { s.split_whitespace().map(String::from).collect() };
    const TYPED: &[&str] = &["may already have been typed"];
    const POSTED: &[&str] = &["never post it again blind", "key="];
    for (write, words) in [
        ("turn hi", TYPED),
        ("@s-a turn timeout=1800000 implement X", TYPED),
        ("@3 turn go", TYPED),
        ("post to=@s-b hi", POSTED),
        ("post to=h-andrew kind=ask --wait=30000 which?", POSTED),
        ("@s-a post to=@s-b kind=task --wait-ack=60000 go", POSTED),
    ] {
        assert!(reports_replacement(&parts(write)), "{write}");
        assert!(!retries_across_replacement(&parts(write)), "{write}");
        let note = not_asked_again_note(&parts(write));
        for &word in words {
            assert!(note.contains(word), "{write}: {note}");
        }
    }
    for write in ["send hi", "key enter"] {
        assert!(!reports_replacement(&parts(write)), "{write}");
    }
    assert!(reports_replacement(&parts("await block")));
    assert!(not_asked_again_note(&parts("await block")).contains("before waiting again"));
    assert!(not_asked_again_note(&parts("await seq")).contains("before waiting again"));
    assert!(not_asked_again_note(&parts("await seq 4")).contains("re-issue it"));
}

/// A listener that accepts but never answers (a process mid-exit whose socket
/// still takes connections into its backlog) is not a server that still
/// serves; one that answers `version` is.
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

/// A successor bound for a verdict that must be REACHED — the fake server's
/// thread answering the probe: a hang detector (AGENTS.md: a minute, never a
/// latency budget), left the moment the server answers. The short bounds below
/// are only for `Gone`, where waiting the bound out is the verdict: under a
/// 300-400 ms bound a loaded box could leave the fake server unscheduled past
/// the probe's timeout, and a server that answers read as gone.
#[cfg(unix)]
const REACHED: Duration = Duration::from_secs(60);

/// A successor bound only a REGRESSION waits out — a call that should not wait
/// for a successor at all, or should stop at its own deadline: five minutes, so
/// [`REACHED`], a minute's hang detector, tells the two apart whatever the
/// machine's load (AGENTS.md: never a latency budget of seconds).
#[cfg(unix)]
const FAR: Duration = Duration::from_secs(300);

/// The four verdicts, each against a real socket.
#[cfg(unix)]
#[test]
fn a_hangup_is_classified_by_who_serves_the_path_now() {
    let (_dir, sock) = scratch_sock("rd-class");
    let path = sock.to_str().unwrap().to_string();
    let own = std::process::id();
    let gone = gone_pid();
    let follow = Redial::new(Follow::Path(path.clone())).with_bound(Duration::from_millis(300));
    let served = Redial::new(Follow::Path(path.clone())).with_bound(REACHED);

    assert_eq!(
        follow.after_hangup(&path, None, None, None),
        Hangup::Unknown
    );
    // Nothing answers: the server is gone and nobody replaced it.
    let started = Instant::now();
    assert!(matches!(
        follow.after_hangup(&path, Some(gone), None, None),
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
    // The same process still answers: it ended the exchange on purpose.
    assert_eq!(
        served.after_hangup(&path, Some(own), None, None),
        Hangup::StillServing
    );
    // A different live process answers on the path: the successor.
    assert_eq!(
        served.after_hangup(&path, Some(gone), None, None),
        Hangup::Successor {
            path: path.clone(),
            pid: own
        }
    );
    stop(&sock);
    server.join().unwrap();
    // A call that follows nothing (the one retry) says so at once: well under
    // the successor bound a follow would wait, here [`FAR`].
    let started = Instant::now();
    assert_eq!(
        Redial::none()
            .with_bound(FAR)
            .after_hangup(&path, Some(gone), None, None),
        Hangup::Gone {
            waited: Duration::ZERO,
            by_deadline: false
        }
    );
    assert!(started.elapsed() < REACHED, "{:?}", started.elapsed());
}

/// THE COLD UPDATE LANE, replayed: aterm-gui's `Endpoint::retire` closes the
/// listening socket and cuts every stream right before the `exec`, which
/// keeps the pid; the exec'd image binds the same path, and an `exec` that
/// returned reopens it — a fresh listener renamed over the path — from the
/// same process. The relay here is that process's own client, up only once
/// the fresh listener is: the stream follows the SAME pid to it, and when that
/// listener, still serving, ends the resumed stream, the stream is over (exit
/// 0), never followed again. A `--pid` pin follows it as `--sock` does: that
/// pid is the one process the pin names.
#[cfg(unix)]
#[test]
fn a_stream_cut_for_an_exec_follows_the_same_pid_to_its_fresh_listener() {
    let me = std::process::id();
    for flag in ["--sock", "--pid"] {
        let (_dir, sock) = scratch_sock("rd-exec");
        let path = sock.to_str().unwrap().to_string();
        let follow = match flag {
            "--sock" => Follow::Path(path.clone()),
            _ => Follow::Nothing(Unfollowed::PidPin(me)),
        };
        let request = "subscribe @* events\n";
        let (seen_tx, seen) = std::sync::mpsc::channel();
        let retired = fake_server(
            aterm_uds::CtlListener::bind(&sock).unwrap(),
            sock.clone(),
            "OK subscribe 0\nsub 1 s-a\n",
            false,
            seen_tx.clone(),
        );
        let stream = CtlStream::connect(&sock).expect("connect");
        let accepted_through = listener_at(&path);
        let mut reader = BufReader::new(&stream);
        handshake(&stream, &mut reader, request);
        // The retire: the listening socket closes, its file stays.
        stop(&sock);
        retired.join().unwrap();
        let fresh = sock.with_file_name("fresh.sock");
        let listener = aterm_uds::CtlListener::bind(&fresh).unwrap();
        std::fs::rename(&fresh, &sock).unwrap();
        let successor = fake_server(
            listener,
            sock.clone(),
            "OK subscribe 0\nsub 1 s-b\n",
            false,
            seen_tx,
        );

        let mut out = Vec::new();
        let code = relay_subscription(
            FirstLeg {
                stream: &stream,
                reader: &mut reader,
                dialed: &path,
                server: Some(me),
                listener: accepted_through,
            },
            request,
            None,
            &Redial::new(follow).with_bound(REACHED),
            &mut out,
        )
        .expect("relay");
        stop(&sock);
        successor.join().unwrap();
        let text = String::from_utf8(out).unwrap();
        let requests: Vec<String> = seen.try_iter().collect();
        assert_eq!(requests, [request, request], "{flag}: {text}");
        // The hole between the legs is marked.
        assert_eq!(text, "sub 1 s-a\nGAP 1 replaced\nsub 1 s-b\n", "{flag}");
        assert_eq!(code, ExitCode::SUCCESS, "{flag}: {text}");
    }
}

/// A `--pid` pin whose process has ended is gone at once, even unreaped. The
/// pinned instance is this test's own child, `SIGKILL`ed and left a zombie,
/// which `kill(pid, 0)` still finds; the pin must not wait out the successor
/// bound for it.
#[cfg(unix)]
#[test]
fn a_pid_pin_whose_process_has_ended_unreaped_is_gone_at_once() {
    use std::io::Read as _;
    let (dir, sock) = scratch_sock("rd-pin-zombie");
    let mut old = ChildSuccessor::spawn(dir.path(), "old", &sock);
    let pid = old.pid();
    // The call names its process first ([`Redial::new`]), then reaches it.
    let redial = Redial::new(Follow::Nothing(Unfollowed::PidPin(pid))).with_bound(REACHED);
    let path = old.sock.to_str().unwrap().to_string();
    let stream = CtlStream::connect(&old.sock).expect("connect");
    let server = server_pid(&stream);
    assert_eq!(server, Some(pid), "the pinned instance serves");
    let listener = listener_at(&path);
    assert!(
        listener.is_some(),
        "a listener identity: the pin may follow"
    );
    let mut reader = BufReader::new(&stream);
    handshake(&stream, &mut reader, "subscribe @s-old events\n");
    let Served::Child(child) = &mut old.process else {
        unreachable!("spawned as this process's child")
    };
    child.0.kill().expect("SIGKILL the pinned instance");
    // The hang-up: the stream reads to its end once the process has gone. A
    // minute is a hang detector.
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("read timeout");
    let mut rest = Vec::new();
    reader.read_to_end(&mut rest).expect("the stream ends");
    assert!(
        aterm_uds::process::pid_alive(pid),
        "unreaped, `kill(pid, 0)` still finds it"
    );
    assert_eq!(
        redial.after_hangup(&path, server, listener, None),
        Hangup::Gone {
            waited: Duration::ZERO,
            by_deadline: false
        }
    );
}

/// A `subscribe` whose server is replaced mid-stream: the old leg's frames,
/// then the successor's (answering on the same fixed path), all on one stdout;
/// the resumed request carries no `since=`; the relay ends 0 only when the
/// stream says its session exited. Negative control: a plain copy-to-EOF relay
/// stops at the hang-up.
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
            listener: None,
        },
        request,
        None,
        &Redial::new(Follow::Path(path.clone())).with_bound(REACHED),
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

/// The old server acknowledged `subscribe @3 …` with `sub 3 s-aaaa`; the
/// successor is asked for `@s-aaaa`, not `@3`. A local target the stream never
/// tied to a sid (no `sub` line read) is not followed: exit 75, the successor
/// asked nothing.
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
                listener: None,
            },
            request,
            None,
            &Redial::new(Follow::Path(path.clone())).with_bound(REACHED),
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

/// [`serve`] for a successor that answers by request: every request but
/// `version` and `stop` is recorded in `seen` and answered with
/// `answer(request)`, then closed, until a `stop` ([`stop`]).
#[cfg(unix)]
fn serve_by(
    listener: &aterm_uds::CtlListener,
    answer: impl Fn(&str) -> &'static str,
    seen: &std::sync::mpsc::Sender<String>,
) {
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
        let reply = answer(&request);
        let _ = seen.send(request);
        let _ = (&conn).write_all(reply.as_bytes());
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
            listener: None,
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

/// A server that still answers ended the stream on purpose (a fixed-target
/// subscription whose sessions closed): exit 0 at once, no successor wait.
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
            listener: None,
        },
        request,
        None,
        &Redial::new(Follow::Path(path.clone())).with_bound(FAR),
        &mut out,
    )
    .unwrap();
    assert_eq!(code, ExitCode::SUCCESS);
    // At once: well under the bound a missed StillServing would wait out.
    assert!(started.elapsed() < REACHED, "{:?}", started.elapsed());
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
    // Every case below with a live server on the path must reach it: under the
    // short bound a slow fake server reads as gone, which also exits 75 and
    // would hide an anchored wait being asked again.
    let served = Redial::new(Follow::Path(path.clone())).with_bound(REACHED);
    let gone = gone_pid();

    // Nothing answers: 75, and `again` never runs.
    let code = super::super::replaced_before_reply(
        &path,
        Some(gone),
        None,
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
        None,
        &parts("@s-a await idle 500"),
        &served,
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
        None,
        &parts("await seq 40"),
        &served,
        None,
        |_| panic!("a seq anchor means nothing to the successor"),
        None,
    )
    .unwrap();
    assert_eq!(code, ExitCode::from(EXIT_REPLACED));
    // Nor an inbox row id: each process numbers its own rows.
    let code = super::super::replaced_before_reply(
        &path,
        Some(gone),
        None,
        &parts("@s-a inbox get 4"),
        &served,
        None,
        |_| panic!("an inbox id means nothing to the successor"),
        None,
    )
    .unwrap();
    assert_eq!(code, ExitCode::from(EXIT_REPLACED));
    // Nor a wait whose anchor is its own arm, nor a `turn`, which types: 75,
    // never asked again.
    for cut in ["@s-a await block timeout=600000", "@s-a turn implement X"] {
        let code = super::super::replaced_before_reply(
            &path,
            Some(gone),
            None,
            &parts(cut),
            &redial,
            None,
            |_| panic!("`{cut}` is not asked again"),
            None,
        )
        .unwrap();
        assert_eq!(code, ExitCode::from(EXIT_REPLACED), "{cut}");
    }
    // The same server still answering: the original error, as before.
    let err = super::super::replaced_before_reply(
        &path,
        Some(std::process::id()),
        None,
        &parts("await idle 500"),
        &served,
        None,
        |_| panic!("not replaced"),
        None,
    )
    .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    stop(&sock);
    server.join().unwrap();
}

/// An explicit `--timeout` bounds the wait for a successor. Against the real
/// 30 s [`SUCCESSOR_BOUND`], the classification, a `subscribe` relay under a
/// short watch, and a blocking read with little left each give up at about
/// the deadline (exit 75, naming the deadline). Negative control: with no
/// deadline the whole bound is waited (on a 1 s bound of its own).
///
/// Sized so load cannot decide it. The legs run against a [`FAR`] successor
/// bound of five minutes, and each must end inside `deadline..`[`REACHED`]: the
/// floor is safe because every leg's clock starts before its deadline is fixed,
/// and it shows each really waited for a successor; the ceiling is a minute's
/// hang detector (AGENTS.md) and still sits minutes under the bound that
/// ignoring the deadline would wait. The 2 s deadline is also the relay's
/// headroom to read its already-buffered frames and EOF before its watch
/// expires. The control runs on a 1 s bound of its own, so the test does not
/// pay the bound.
#[cfg(unix)]
#[test]
fn an_explicit_timeout_bounds_the_wait_for_a_successor() {
    let (_dir, sock) = scratch_sock("rd-deadline");
    let path = sock.to_str().unwrap().to_string();
    let gone = gone_pid();
    let deadline = Duration::from_secs(2);
    let redial = Redial::new(Follow::Path(path.clone())).with_bound(FAR);
    let parts = |s: &str| -> Vec<String> { s.split_whitespace().map(String::from).collect() };
    let within_the_deadline = |started: Instant| {
        let took = started.elapsed();
        assert!(
            took >= deadline && took < REACHED,
            "took {took:?} against a {deadline:?} deadline and a {FAR:?} bound"
        );
    };

    let started = Instant::now();
    let verdict = redial.after_hangup(&path, Some(gone), None, Some(started + deadline));
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
            listener: None,
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
        None,
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
            .after_hangup(&path, Some(gone), None, None),
        Hangup::Gone {
            by_deadline: false,
            ..
        }
    ));
    assert!(started.elapsed() >= bound, "{:?}", started.elapsed());
    assert!(gone_note(Some(gone), deadline, true, None).contains("its --timeout left"));
}

/// A scratch rendezvous dir — what `aterm_uds::control_socket_dir` names —
/// with its `graph/` subdir, short enough for socket paths inside it.
#[cfg(unix)]
fn scratch_rendezvous(tag: &str) -> (aterm_tempfile::TempDir, PathBuf) {
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

/// The self-update path (`Follow::Flagless`): the successor binds its own
/// `aterm-<pid>.sock` and is reached through the calling session's graph entry
/// it republishes, or outside a session through the `latest` alias it
/// repoints. Control: a fixed-path follow of the dialled socket never finds a
/// successor on another socket.
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
    .with_bound(REACHED);
    assert_eq!(
        in_session.after_hangup(old, Some(gone), None, None),
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
    .with_bound(REACHED);
    assert_eq!(
        outside.after_hangup(old, Some(gone), None, None),
        Hangup::Successor {
            path: alias.to_str().unwrap().to_string(),
            pid: own
        }
    );

    let pinned = Redial::new(Follow::Path(old.to_string())).with_bound(Duration::from_millis(300));
    assert!(matches!(
        pinned.after_hangup(old, Some(gone), None, None),
        Hangup::Gone { .. }
    ));
    stop(&new_sock);
    successor.join().unwrap();
}

/// …and never a stranger: a live instance in the same dir that neither the
/// session's graph entry nor the `latest` alias names is not a successor.
/// Control: once the entry names it (its successor republished it), it is.
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
        follow.after_hangup(old, Some(gone), None, None),
        Hangup::Gone { .. }
    ));
    // The alias names the stranger (two windows, the alias on the newer): still
    // no successor for a call made inside a session, neither through the alias
    // nor through a dialled path that was the alias.
    let alias = dir.join(super::super::SOCK_FILE);
    std::os::unix::fs::symlink(&stranger_sock, &alias).unwrap();
    assert!(matches!(
        follow.after_hangup(old, Some(gone), None, None),
        Hangup::Gone { .. }
    ));
    assert!(matches!(
        follow.after_hangup(alias.to_str().unwrap(), Some(gone), None, None),
        Hangup::Gone { .. }
    ));
    // Outside any session the alias IS what the call followed: taken.
    let outside = Redial::new(Follow::Flagless {
        dir: dir.clone(),
        self_sid: None,
    })
    .with_bound(REACHED);
    assert!(matches!(
        outside.after_hangup(old, Some(gone), None, None),
        Hangup::Successor { pid, .. } if pid == own
    ));
    std::fs::remove_file(&alias).unwrap();

    publish_graph_entry(&dir, sid, &stranger_sock);
    let reached = Redial::new(Follow::Flagless {
        dir: dir.clone(),
        self_sid: Some(sid.to_string()),
    })
    .with_bound(REACHED);
    assert_eq!(
        reached.after_hangup(old, Some(gone), None, None),
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
/// The successor binds AFTER the hang-up, while the client waits, and is found
/// inside the bound.
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
            listener: None,
        },
        request,
        None,
        &Redial::new(Follow::Flagless {
            dir: dir.clone(),
            self_sid: Some(sid.to_string()),
        })
        .with_bound(REACHED),
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

/// Where a redial looks follows how the call resolved: a `--pid` pin follows no
/// successor, `--sock` follows its path, and the flagless default the calling
/// session's graph entry and the alias in the rendezvous dir (`Nothing` with no
/// such dir). No environment variable selects a socket, so none steers a
/// redial.
#[test]
fn a_redial_follows_what_the_call_resolved_through() {
    use super::super::redial_follow_in as follow;
    let dir = PathBuf::from("/r");
    let sid = Some("s-ab".to_string());
    assert_eq!(
        follow(Some(dir.clone()), Some("/x.sock"), Some(42), sid.clone()),
        Follow::Nothing(Unfollowed::PidPin(42)),
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
        Follow::Nothing(Unfollowed::NoRendezvousDir),
        "no rendezvous dir"
    );
}

/// A `dial … subscribe` stream the local server hangs up while still serving:
/// it only relays a dial, so the remote connection closed under it; exit 75.
/// Negative control: the same stream without `dial` is a deliberate end, exit 0
/// ([`a_deliberate_end_by_a_live_server_is_still_exit_0_and_immediate`]).
#[cfg(unix)]
#[test]
fn a_dial_stream_whose_remote_closes_is_exit_75_not_0() {
    let (_dir, sock) = scratch_sock("rd-dial");
    let path = sock.to_str().unwrap().to_string();
    let request = "dial work subscribe @s-a screen\n";
    let (tx, _rx) = std::sync::mpsc::channel();
    let server = fake_server(
        aterm_uds::CtlListener::bind(&sock).unwrap(),
        sock.clone(),
        "OK subscribe 1\nsub 1 s-a\nDELTA 1 seq=2 screen 1\nhello\n",
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
            listener: None,
        },
        request,
        None,
        &Redial::new(Follow::Path(path.clone())).with_bound(FAR),
        &mut out,
    )
    .unwrap();
    assert_eq!(code, ExitCode::from(EXIT_REPLACED));
    assert!(String::from_utf8(out).unwrap().contains("hello"));
    // A hang detector (AGENTS.md): a successor wait sits out [`FAR`].
    assert!(started.elapsed() < REACHED, "no successor wait");
    stop(&sock);
    server.join().unwrap();
}

/// Each exit-75 note says the one reason the call knows, never a list of the
/// reasons it could be.
#[test]
fn each_exit_75_note_gives_the_one_reason_it_knows() {
    let second = Duration::from_secs(1);
    assert_eq!(
        gone_note(Some(7), Duration::ZERO, false, Some(&Unfollowed::PidPin(7))),
        "aterm (pid 7) went away; a --pid call follows no other instance; the exchange cannot \
         be resumed (exit 75)"
    );
    assert!(
        gone_note(None, Duration::ZERO, false, Some(&Unfollowed::PidPin(7)))
            .starts_with("aterm (pid 7) went away; a --pid call follows no other instance"),
        "a --pid call names its process even when no hello said it"
    );
    assert_eq!(
        gone_note(Some(7), Duration::ZERO, false, Some(&Unfollowed::AskedOnce)),
        "aterm (pid 7) went away; this was already the one retry; the exchange cannot be \
         resumed (exit 75)"
    );
    assert_eq!(
        gone_note(
            Some(7),
            Duration::ZERO,
            false,
            Some(&Unfollowed::NoRendezvousDir)
        ),
        "aterm (pid 7) went away; with $HOME and $XDG_RUNTIME_DIR unset there is nowhere to \
         find a successor; the exchange cannot be resumed (exit 75)"
    );
    assert_eq!(
        gone_note(Some(7), 30 * second, false, None),
        "aterm (pid 7) went away and nothing replaced it within 30.0 s; the exchange cannot be \
         resumed (exit 75)"
    );
    assert_eq!(
        remote_closed_note_for(&["dial", "work", "text"], false).as_deref(),
        Some(
            "the connection to the remote 'work' closed before it answered; the exchange cannot \
             be resumed (exit 75)"
        )
    );
    assert_eq!(
        remote_closed_note_for(&["dial", "work", "subscribe", "@s-a", "screen"], true).as_deref(),
        Some(
            "the connection to the remote 'work' closed before its sessions exited; the \
             exchange cannot be resumed (exit 75)"
        )
    );

    let parts = |s: &str| -> Vec<String> { s.split_whitespace().map(String::from).collect() };
    for (request, anchor) in [
        ("await seq 40", Some("seq 40")),
        ("await inbox since=12", Some("since=12")),
        ("@s-a inbox get 4", Some("inbox get 4")),
        ("inbox --peek", None),
        ("@3 await match DONE", Some("@3")),
        ("await seq timeout=0", None),
        ("await", None),
    ] {
        assert_eq!(
            old_process_anchor(&parts(request)).as_deref(),
            anchor,
            "{request}"
        );
    }
    // The replacement note names the anchor when the request has one, and
    // claims no reason when it has none.
    assert_eq!(
        not_asked_again_note(&parts("await inbox since=12")),
        "`since=12` names a position in the old process — re-issue it"
    );
    assert_eq!(
        not_asked_again_note(&parts("@3 await match DONE")),
        "`@3` names a position in the old process — re-issue it"
    );
    assert_eq!(
        not_asked_again_note(&parts("await")),
        "re-issue the request"
    );
    assert_eq!(
        not_asked_again_note(&parts("@s-a await")),
        "re-issue the request"
    );
    // The whole line `replaced_before_reply` prints: the replacement, the one
    // reason, and the exit code every exit-75 note names.
    assert_eq!(
        super::super::not_asked_again_line(7, 8, &parts("await inbox since=12")),
        "aterm was replaced (pid 7 -> 8) before it answered; `since=12` names a \
         position in the old process — re-issue it (exit 75)"
    );
    assert_eq!(
        super::super::not_asked_again_line(7, 8, &parts("turn hi")),
        "aterm was replaced (pid 7 -> 8) before it answered; a `turn` TYPES: its \
         text may already have been typed, and submitted, in the session — read the screen \
         (`text`) before sending it again; never retry it blind (exit 75)"
    );
    assert_eq!(
        super::super::not_asked_again_line(7, 8, &parts("await")),
        "aterm was replaced (pid 7 -> 8) before it answered; re-issue the request \
         (exit 75)"
    );
    assert_eq!(
        untied_local_target("subscribe @s-a,@3 screen\n", &[("1".into(), "s-a".into())]).as_deref(),
        Some("@3")
    );
    assert_eq!(
        dial_name(&["dial", "work", "text"]),
        Some("work"),
        "the dial's connection"
    );
    assert_eq!(dial_name(&["text"]), None);
}

/// A `dial` whose remote closes before answering exits 75, and a dialled `turn`
/// or `post` (possibly already typed or queued on the remote) says not to send
/// it again blind, as [`not_asked_again_note`] does after an update. Each case
/// is the whole request, as both call sites pass it to
/// [`remote_closed_note_for`], so parsing the relayed request past `dial
/// <name>` is pinned too.
#[test]
fn a_dialled_turn_or_post_cut_by_its_remote_is_never_sent_again_blind() {
    for (request, words) in [
        (
            &["dial", "work", "turn", "hi"][..],
            "its `turn` may already have been typed",
        ),
        (
            &["dial", "work", "@s-a", "turn", "timeout=1800000", "go"][..],
            "never retry it blind",
        ),
        (
            &["dial", "work", "post", "to=@s-b", "hi"][..],
            "never post it again blind",
        ),
        (
            &["dial", "work", "@s-a", "post", "key=k1", "go"][..],
            "the same `key=`",
        ),
    ] {
        let note = remote_closed_note_for(request, false).expect("a dial");
        assert!(
            note.starts_with("the connection to the remote 'work' closed before it answered; "),
            "{note}"
        );
        assert!(note.contains(words), "{request:?}: {note}");
        assert!(!note.contains("cannot be resumed"), "{request:?}: {note}");
        assert!(note.ends_with("(exit 75)"), "{note}");
    }
    assert_eq!(
        remote_closed_note_for(&["turn", "hi"], false),
        None,
        "not a dial"
    );
    // A bare `await seq` with a parameter is still bare: the arm note, not the
    // anchor list.
    let parts = |s: &str| -> Vec<String> { s.split_whitespace().map(String::from).collect() };
    assert!(
        not_asked_again_note(&parts("await seq timeout=0")).starts_with("a bare `await seq`"),
        "{}",
        not_asked_again_note(&parts("await seq timeout=0"))
    );
    assert_eq!(
        not_asked_again_note(&parts("await seq 40")),
        "`seq 40` names a position in the old process — re-issue it"
    );
}

/// A dialled wait for a change after its arm (`await block`, `await consent`,
/// a bare `await seq`) cut by its remote says to read the session there before
/// waiting again: a self-updating remote's successor parses the finished
/// command's output before any re-dial can arm. Negative control: a dialled
/// plain read (`await idle`) and an `await seq` with a counter keep the plain
/// note.
#[test]
fn a_dialled_wait_from_its_arm_cut_by_its_remote_reads_the_session_first() {
    for (request, wait) in [
        (
            &["dial", "work", "await", "block", "timeout=600000"][..],
            "await block",
        ),
        (
            &["dial", "work", "@s-a", "await", "consent"][..],
            "await consent",
        ),
        (&["dial", "work", "await", "seq"][..], "await seq"),
        (
            &["dial", "work", "@s-a", "await", "seq", "timeout=0"][..],
            "await seq",
        ),
    ] {
        assert_eq!(
            remote_closed_note_for(request, false),
            Some(format!(
                "the connection to the remote 'work' closed before it answered; its `{wait}` \
                 waits for a change after it was armed, which may have happened on the remote; \
                 read the session there (`status`, `text`) before waiting again (exit 75)"
            )),
            "{request:?}"
        );
    }
    for plain in [
        &["dial", "work", "await", "idle", "500"][..],
        &["dial", "work", "await", "seq", "40"][..],
    ] {
        assert_eq!(
            remote_closed_note_for(plain, false).as_deref(),
            Some(
                "the connection to the remote 'work' closed before it answered; the exchange \
                 cannot be resumed (exit 75)"
            ),
            "{plain:?}"
        );
    }
}

/// End to end: a `dial` request cut before its reply while the relay still
/// serves is the remote closing the connection, and exits 75 — not the exit-1
/// `server closed the connection without responding` a caller retries, which
/// for a `turn` or `post` typed a prompt twice or sent a post beside a queued
/// copy. The note's words, and the relayed request both call sites hand it,
/// are pinned by [`each_exit_75_note_gives_the_one_reason_it_knows`] and
/// [`a_dialled_turn_or_post_cut_by_its_remote_is_never_sent_again_blind`];
/// stderr is not captured here.
#[cfg(unix)]
#[test]
fn a_dialled_turn_or_post_cut_while_the_relay_serves_is_exit_75() {
    for (tag, request, verb) in [
        ("rd-dial-cut", "dial work await idle 500\n", "await"),
        ("rd-dial-turn", "dial work turn implement X\n", "turn"),
        ("rd-dial-post", "dial work post to=@s-b key=k1 hi\n", "post"),
    ] {
        let (_dir, sock) = scratch_sock(tag);
        let path = sock.to_str().unwrap().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        let server = fake_server(
            aterm_uds::CtlListener::bind(&sock).unwrap(),
            sock.clone(),
            "",
            false,
            tx,
        );
        let frame = request.trim_end().trim_start_matches("dial work ");
        let code = super::super::exchange(
            &path,
            super::super::TargetOrigin::Pinned,
            request,
            verb,
            frame,
            Some(Duration::from_secs(60)),
            false,
            &Redial::new(Follow::Path(path.clone())).with_bound(REACHED),
        )
        .expect("a closed remote is an outcome, not an error");
        assert_eq!(code, ExitCode::from(EXIT_REPLACED), "{request}");
        assert_eq!(rx.recv().unwrap(), request);
        stop(&sock);
        server.join().unwrap();
    }
}
