// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `aterm-ctl` transparent redial across seamless updates and crash recovery.

use super::*;
#[cfg(unix)]
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

/// A scripted fake server on `listener`, in its own thread.
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

/// [`relay_subscription`] of the first leg on `stream` (its status line read
/// through `reader`), from `server` at `dialed`, with no listener identity
/// and no deadline: the exit code and what it relayed.
#[cfg(unix)]
fn relay<'b>(
    stream: &'b CtlStream,
    reader: &mut BufReader<&'b CtlStream>,
    dialed: &str,
    server: Option<u32>,
    request: &str,
    redial: &Redial,
) -> (ExitCode, String) {
    let mut out = Vec::new();
    let code = relay_subscription(
        FirstLeg {
            stream,
            reader,
            dialed,
            server,
            listener: None,
        },
        request,
        None,
        redial,
        &mut out,
    )
    .expect("relay");
    (code, String::from_utf8(out).expect("UTF-8 frames"))
}

/// The relay's frame parser: a session ends only on every target's
/// `exited` frame (`@*` follows the live roster and never ends), a body that
/// reads like a frame is skipped by its declared length, a header split
/// across reads is not a frame yet, and unframeable input means "cannot say".
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

    let mut t = FrameTracker::new("subscribe @s-a screen,bytes,events\n");
    t.feed(b"sub 1 s-a\nDELTA 1 seq=4 screen 2\nEVENT 1 exited\nsub 9 s-row\n");
    assert!(!t.session_over(), "screen rows are rows, not frames");
    assert_eq!(
        t.sids(),
        [("1".to_string(), "s-a".to_string())],
        "a body that reads like a `sub` line is no target"
    );
    t.feed(b"BYTES 1 15\nEVENT 1 exit");
    t.feed(b"ed\n\n");
    assert!(!t.session_over(), "raw PTY bytes are a body, not a frame");
    t.feed(b"DELTA 1 seq=5 cells 3\nabc\nEVENT 1 exited\n");
    assert!(t.session_over(), "the real frame after the bodies");

    let mut t = FrameTracker::new("subscribe @* events\n");
    t.feed(b"sub 1 s-a\nEVENT 1 exited\n");
    assert!(!t.session_over());

    let mut t = FrameTracker::new("subscribe @s-a events\n");
    t.feed(
        b"sub 1 s-a\nEVENT 1 block-complete 12 exit=0\n\
          EVENT 1 turn 7 submitted=1 status=settled dur_ms=5\nEVENT 1 exited\n",
    );
    t.start_leg(LegSession::Same);
    assert!(!t.session_over());
    assert_eq!(
        t.anchors(),
        EventAnchors {
            block: Some(12),
            turn: Some(7),
            ..EventAnchors::default()
        }
    );
    let mut t = FrameTracker::new("subscribe @s-a events\n");
    t.feed(b"sub 1 s-a\nEVENT 1 exited\nDELTA 1 seq=1 screen x\n");
    assert!(!t.session_over());
}

/// What a resumed `subscribe` asks the successor.
#[test]
fn a_resumed_subscribe_carries_only_what_the_successor_can_answer() {
    let none = EventAnchors::default();
    let seen = EventAnchors {
        block: Some(12),
        turn: Some(7),
        ..EventAnchors::default()
    };
    let tied = [("1".to_string(), "s-aaaa".to_string())];
    let local = [
        ("3".to_string(), "s-aaaa".to_string()),
        ("5".to_string(), "s-bbbb".to_string()),
    ];
    for (request, sids, anchors, resumed) in [
        (
            "@s-a subscribe screen,events since=41 since-turn=7 since-block=3 every-frame\n",
            &[][..],
            none,
            Some("@s-a subscribe screen,events every-frame since-block=3 since-turn=7\n"),
        ),
        (
            "subscribe @* sessions\n",
            &[][..],
            none,
            Some("subscribe @* sessions\n"),
        ),
        (
            "subscribe @s-a events since-block=20\n",
            &[][..],
            EventAnchors {
                block: Some(12),
                turn: None,
                ..EventAnchors::default()
            },
            Some("subscribe @s-a events since-block=20\n"),
        ),
        (
            "subscribe @s-a,@s-b events\n",
            &[][..],
            seen,
            Some("subscribe @s-a,@s-b events\n"),
        ),
        (
            "subscribe @s-bbbb events\n",
            &[][..],
            seen,
            Some("subscribe @s-bbbb events since-block=12 since-turn=7\n"),
        ),
        (
            "subscribe @3 screen,events since=9\n",
            &local[..],
            none,
            Some("subscribe @s-aaaa screen,events\n"),
        ),
        (
            "subscribe @3,@s-cccc,@.,@5 events\n",
            &local[..],
            none,
            Some("subscribe @s-aaaa,@s-cccc,@.,@s-bbbb events\n"),
        ),
        ("subscribe @4 events\n", &local[..], none, None),
        (
            "subscribe @. events\n",
            &tied[..],
            seen,
            Some("subscribe @s-aaaa events since-block=12 since-turn=7\n"),
        ),
        (
            "subscribe events\n",
            &tied[..],
            seen,
            Some("subscribe @s-aaaa events since-block=12 since-turn=7\n"),
        ),
        (
            "@. subscribe screen,events since=4\n",
            &tied[..],
            seen,
            Some("@s-aaaa subscribe screen,events since-block=12 since-turn=7\n"),
        ),
        (
            "subscribe @. events since-block=3\n",
            &[][..],
            seen,
            Some("subscribe @. events\n"),
        ),
        (
            "subscribe events\n",
            &[][..],
            seen,
            Some("subscribe events\n"),
        ),
    ] {
        assert_eq!(
            resume_request(request, sids, &anchors).as_deref(),
            resumed,
            "{request}"
        );
    }
    for (request, sids, fallback) in [
        (
            "subscribe @. events\n",
            &tied[..],
            Some("subscribe @. events\n"),
        ),
        ("subscribe events\n", &tied[..], Some("subscribe events\n")),
        (
            "@. subscribe screen,events since=4\n",
            &tied[..],
            Some("@. subscribe screen,events\n"),
        ),
        ("subscribe @s-bbbb events\n", &tied[..], None),
        ("subscribe @. events\n", &[][..], None),
        ("subscribe @.,@s-cccc events\n", &tied[..], None),
        ("@. subscribe @s-cccc events\n", &tied[..], None),
        ("subscribe @1 events\n", &tied[..], None),
        ("subscribe @* sessions\n", &tied[..], None),
    ] {
        assert_eq!(
            unpinned_resume_request(request, sids, &seen).as_deref(),
            fallback,
            "{request}"
        );
    }
}

/// A resumed `log` stream asks for what follows the LAST RECORD IT RELAYED, in
/// the epoch it was read in — never the request's own `since-log=` (whose
/// replay would push every relayed record again) and never nothing (which
/// would lose what came between the legs): from the `sub` line's start, each
/// `LOG` line, and a `GAP`'s `resync_to=`/`ep=`.
#[test]
fn a_resumed_log_stream_continues_after_the_last_record_relayed() {
    let request = "subscribe @s-a log since-log=5 log-ep=1\n";
    let mut t = FrameTracker::new(request);
    t.feed(
        b"OK subscribe 1\nsub 1 s-a log_seq=5 log_ep=1\n\
          LOG 1 6 open r1 cmd src=shell conf=strong t=1\n\
          LOG 1 9 close r1 cmd out=ok src=shell conf=strong t=2\n",
    );
    let resumed = resume_request(request, &t.sids(), &t.anchors()).expect("resumes");
    assert_eq!(resumed, "subscribe @s-a log since-log=9 log-ep=1\n");
    assert!(
        !resumed.contains("since-log=5"),
        "NEGATIVE CONTROL: the request's own anchor would replay 6..9"
    );
    let live = "subscribe @s-a log\n";
    let mut t = FrameTracker::new(live);
    t.feed(b"OK subscribe 1\nsub 1 s-a log_seq=12 log_ep=3\n");
    assert_eq!(
        resume_request(live, &t.sids(), &t.anchors()).as_deref(),
        Some("subscribe @s-a log since-log=12 log-ep=3\n"),
        "nothing relayed yet: from where it started"
    );
    t.start_leg(LegSession::Same);
    t.feed(
        b"OK subscribe 1\nsub 1 s-a log_seq=12 log_ep=3\n\
          GAP 1 log-lost=0 why=handoff resync_to=1012 ep=4\n\
          LOG 1 1013 mark - handoff src=aterm conf=exact t=3\n",
    );
    assert_eq!(
        resume_request(live, &t.sids(), &t.anchors()).as_deref(),
        Some("subscribe @s-a log since-log=1013 log-ep=4\n"),
        "a gap's epoch, then the record after it"
    );
    let mut t = FrameTracker::new(live);
    t.feed(b"OK subscribe 1\nsub 1 s-a\nLOG 1 4 mark - bell src=engine conf=strong t=1\n");
    assert_eq!(
        resume_request(live, &t.sids(), &t.anchors()).as_deref(),
        Some(live)
    );
    let tied = [("1".to_string(), "s-a".to_string())];
    let seen = EventAnchors {
        log_seq: Some(9),
        log_ep: Some(1),
        ..EventAnchors::default()
    };
    let own = "subscribe @. log since-log=5 log-ep=1\n";
    assert_eq!(
        resume_request(own, &tied, &seen).as_deref(),
        Some("subscribe @s-a log since-log=9 log-ep=1\n")
    );
    assert_eq!(
        unpinned_resume_request(own, &tied, &seen).as_deref(),
        Some("subscribe @. log\n")
    );
    assert_eq!(
        resume_request("subscribe @s-a,@s-b log\n", &[], &seen).as_deref(),
        Some("subscribe @s-a,@s-b log\n")
    );
    let mut t = FrameTracker::new(live);
    t.feed(b"sub 1 s-a log_seq=7 log_ep=2\n");
    t.start_leg(LegSession::Other);
    assert_eq!(t.anchors(), EventAnchors::default());
}

/// The fallback derives from the original request, not from a last leg pinned to
/// its sid; a stream that named its session by sid has no fallback.
#[test]
fn a_fallback_to_self_survives_a_leg_that_was_pinned() {
    let seen = EventAnchors {
        block: Some(5),
        turn: None,
        ..EventAnchors::default()
    };
    let tied = [("1".to_string(), "s-a".to_string())];
    let original = "subscribe @. cursor,events\n";
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

/// A successor's start after any hang-up: a fresh instance.
#[cfg(unix)]
fn started_later(_: u32) -> Option<Stamp> {
    Some(Stamp {
        boot: Some(BootClock(u64::MAX)),
        wall: SystemTime::now().checked_add(Duration::from_secs(3600)),
    })
}

/// The decision at a refused resume: asked again, self as written, only after
/// `ERR no such session` to the pinned form from a fresh successor, and never a
/// third time.
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

/// A fake successor process for relay tests: its own pid, so the relay takes it
/// for the successor of an in-process server (or the reverse).
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
            // SAFETY: sends a signal to the live orphan pid spawned by this test.
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

/// After a fallback to a fresh successor's `s-new`, the next successor (which
/// carried `s-new`) is asked with `s-new`'s anchor alone, never `s-old`'s.
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
    let (code, out) = relay(
        &stream,
        &mut reader,
        &path,
        Some(gone_pid()),
        request,
        &Redial::new(Follow::Path(path.clone()))
            .with_bound(REACHED)
            .with_started(started_later),
    );
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
        out.contains("sub 5 s-new\n"),
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

/// A failed write reads the refusal a busy listener left before closing unread;
/// a close with nothing left is `Gone`; a close after the request was read is an
/// empty line.
#[cfg(unix)]
#[test]
fn a_write_that_fails_reads_the_refusal_the_successor_left() {
    let big = format!("subscribe @s-a cursor {}\n", "x".repeat(4 << 20));
    let ask = |client: &CtlStream| {
        let mut reader = BufReader::new(client);
        ask_once(client, &mut reader, None, &big)
    };
    let (_d, client) = conn_after("rd-ask-busy", |conn| {
        let _ = (&conn).write_all(b"ERR control server busy; retry\n");
    });
    assert!(
        matches!(ask(&client), Answer::Line(l) if l == "ERR control server busy; retry"),
        "the refusal it left is the answer, not a broken pipe"
    );
    let (_d, client) = conn_after("rd-ask-gone", |_conn| {});
    assert!(matches!(ask(&client), Answer::Gone));
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
}

/// A live successor that closes on the resume ask is asked again, and the
/// stream never ends 0 without it.
#[cfg(unix)]
#[test]
fn a_successor_that_closes_on_the_resume_is_asked_again_and_never_a_silent_exit_0() {
    let filler = "x".repeat(4 << 20);
    let request = format!("subscribe @s-a cursor,events {filler}\n");
    for (shape, closes, saturated) in [
        ("silent", 1, 0),
        ("saturated", 1, 3),
        ("silent-forever", usize::MAX, 0),
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
        let listener = aterm_uds::CtlListener::bind(&sock).unwrap();
        let asks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&asks);
        let successor = std::thread::spawn(move || {
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
                    std::thread::sleep(Duration::from_millis(50));
                    busy = saturated;
                } else {
                    let _ = conn.set_read_timeout(None);
                    let mut rest = String::new();
                    let _ = std::io::BufReader::new(&conn).read_line(&mut rest);
                    let _ = (&conn).write_all(b"OK subscribe 1\nsub 1 s-a\nEVENT 1 exited\n");
                }
            }
        });
        let bound = if closes == usize::MAX {
            Duration::from_millis(500)
        } else {
            REACHED
        };
        let (code, out) = relay(
            &stream,
            &mut reader,
            &path,
            Some(gone_pid()),
            &request,
            &Redial::new(Follow::Path(path.clone()))
                .with_bound(bound)
                .with_started(started_later),
        );
        stop(&sock);
        successor.join().unwrap();
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

/// A successor answering busy is asked again within the successor wait: here
/// busy twice, then it takes the stream.
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
    let (code, out) = relay(
        &stream,
        &mut reader,
        &path,
        Some(gone_pid()),
        request,
        &Redial::new(Follow::Path(path.clone()))
            .with_bound(REACHED)
            .with_started(started_later),
    );
    stop(&sock);
    successor.join().unwrap();
    assert_eq!(code, ExitCode::SUCCESS, "the stream resumed: {out}");
    assert_eq!(asks.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert!(
        out.ends_with("sub 1 s-a\nEVENT 1 exited\n"),
        "the successor's leg was relayed: {out}"
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

/// A relay (A) forwarding `subscribe @s-abc …` to a host (B) that went away
/// stays up and says so on the stream: the relay follows B's successor where it
/// republishes `graph/<sid>`, never passing the marker on; with none there
/// within the wait, exit 75, never 0.
#[cfg(unix)]
#[test]
fn a_forwarded_stream_follows_its_hosts_successor_not_the_relay() {
    let request = "subscribe @s-abc events\n";
    for successor_up in [true, false] {
        let (dir, relay_sock) = scratch_sock("rd-fwd");
        let relay_path = relay_sock.to_str().unwrap().to_string();
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

/// The moment [`a_later_successor_is_judged_against_the_hangup_it_follows`]
/// puts between two hang-ups: when its second successor "started".
#[cfg(unix)]
static BETWEEN: std::sync::Mutex<Option<Stamp>> = std::sync::Mutex::new(None);

/// A successor that started at [`BETWEEN`].
#[cfg(unix)]
fn started_between(_: u32) -> Option<Stamp> {
    *BETWEEN.lock().unwrap()
}

/// Each successor is judged against the hang-up it follows: S2, started
/// between the stream losing A and losing S1 (after a leg, or a failed send),
/// predates S1's hang-up and ends the stream 75; started after both, it is
/// followed.
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
        std::fs::rename(&s1.sock, &sock).expect("S1 takes the path");
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
        let (code, out) = relay(
            &stream,
            &mut reader,
            &path,
            Some(gone_pid()),
            &request,
            &Redial::new(Follow::Path(path.clone()))
                .with_bound(REACHED)
                .with_started(started),
        );
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
        let text = out;
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

/// A self stream moves only on the fixed socket the call named
/// ([`Redial::arrival`]): there only a relaunch of the instance configured on
/// it can bind, so a fresh successor is judged by its start.
#[cfg(unix)]
#[test]
fn a_self_stream_moves_only_on_the_fixed_socket_the_call_named() {
    let (_scratch, dir) = scratch_rendezvous("rd-where");
    let at = |name: &str| dir.join(name).to_str().unwrap().to_string();
    let (fixed, alias, link, instance) = (
        at("fixed.sock"),
        at(super::super::SOCK_FILE),
        at("l.sock"),
        at("aterm-4242.sock"),
    );
    std::os::unix::fs::symlink(dir.join("a.sock"), &link).unwrap();
    let flagless = |sid: Option<&str>| Follow::Flagless {
        dir: dir.clone(),
        sid: sid.map(String::from),
    };
    for (case, follow, answered_at, arrival) in [
        ("fixed", Follow::Path(fixed.clone()), &fixed, Arrival::Fresh),
        (
            "alias",
            Follow::Path(alias.clone()),
            &alias,
            Arrival::Shared,
        ),
        ("link", Follow::Path(link.clone()), &link, Arrival::Shared),
        (
            "instance",
            Follow::Path(instance.clone()),
            &instance,
            Arrival::Shared,
        ),
        (
            "elsewhere",
            Follow::Path(fixed.clone()),
            &alias,
            Arrival::Shared,
        ),
        ("alias-follow", flagless(None), &alias, Arrival::Shared),
        (
            "graph-entry",
            flagless(Some("s-00ab")),
            &fixed,
            Arrival::Shared,
        ),
    ] {
        let redial = Redial::new(follow).with_started(started_later);
        assert_eq!(
            redial.arrival(4242, answered_at, Stamp::now()),
            arrival,
            "{case}"
        );
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

/// The process asked is the one judged: P1 (fresh) answers the probe, then P2
/// (already running) takes the socket before the ask, so the stream ends 75
/// with P2 asked for `s-old` alone; with P2 fresh too, it is followed.
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
        let (code, out) = relay(
            &stream,
            &mut reader,
            old_sock.to_str().unwrap(),
            Some(gone_pid()),
            request,
            &Redial::new(Follow::Path(fixed.to_str().unwrap().to_string()))
                .with_bound(REACHED)
                .with_started(started),
        );
        p1.join().unwrap();
        assert_eq!(
            seen.try_iter().skip(1).count(),
            0,
            "{case}: P1 was never asked"
        );
        let text = out;
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

/// A successor's start is the kernel's boot-clock record (macOS and Linux): a
/// child spawned after a moment is fresh against it, this process predates it,
/// and an ended process has none; elsewhere every start is unknown.
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

/// A fixed wall-clock moment the clock stand-ins below read around.
fn wall_at(seconds: u64) -> Option<SystemTime> {
    UNIX_EPOCH.checked_add(Duration::from_secs(1_800_000_000 + seconds))
}

/// The boot clock orders a start and a hang-up wherever both have it, whatever
/// the wall clock says; the wall clock decides only where one side lacks it,
/// and with no shared clock the arrival is [`Arrival::Unknown`].
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

    let linux_hangup = at(Some(1_234_567_890_000), None);
    for (case, ticks, state, arrival) in [
        ("next tick", 123_457, 'S', Arrival::Fresh),
        ("same tick", 123_456, 'S', Arrival::Predates),
        ("earlier", 120_000, 'S', Arrival::Predates),
        ("zombie", 123_457, 'Z', Arrival::Unknown),
    ] {
        assert_eq!(
            arrival_of(linux_started(1, ticks, state), linux_hangup),
            arrival,
            "linux {case}"
        );
    }
    let kernel_ticks = |wall| at(Some(16_941_646_465_128), Some(wall));
    let rosetta = at(None, Some(10));
    assert_eq!(arrival_of(Some(kernel_ticks(11)), rosetta), Arrival::Fresh);
    assert_eq!(
        arrival_of(Some(kernel_ticks(9)), rosetta),
        Arrival::Predates
    );
    assert_eq!(
        arrival_of(
            Some(kernel_ticks(11)),
            at(Some(705_901_910_571_583), Some(10))
        ),
        Arrival::Predates,
        "nanoseconds taken for boot ticks"
    );

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
/// to it cannot read its server's hang-up before then.
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
/// stamp.
#[cfg(unix)]
fn crash_and_let_its_watch_stamp_it(server: &mut ChildSuccessor) {
    let pid = server.pid();
    let watch = watched(pid);
    server.crash();
    if watch {
        exit_stamped(pid);
    }
}

/// A relaunch between the old server's exit and the relay's read is fresh: the
/// old instance is `SIGKILL`ed while the relay is blocked on a stalled consumer
/// ([`Stall`]), and the relaunch is judged against the exit the kernel
/// reported.
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
            stall.blocked();
            let early = (case == "already running")
                .then(|| ChildSuccessor::spawn(new_dir.path(), "refuses", &sock));
            if case != "no watch" {
                crash_and_let_its_watch_stamp_it(&mut old);
            }
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

/// A later leg's server is watched from its ask: S1, reached through an update,
/// crashes under a stalled relay, and a relaunch started before the relay reads
/// the EOF is fresh against S1's exit.
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
        "@s-a await agent idle,prompt timeout=60000",
        "await log need",
        "@s-a await log need timeout=600000",
        "await log close=turn since=4 ep=1 timeout=600000",
        "await log need,need",
        "@s-a await log need,close=turn since=4 ep=1 timeout=600000",
        "@s-a await edit uri=file:///tmp/x timeout=600000",
    ] {
        assert!(retries_across_replacement(&parts(yes)), "{yes}");
        assert!(is_blocking_read(&parts(yes)), "{yes}");
    }
    for no in [
        "await seq",
        "await seq timeout=0",
        "await block",
        "@s-a await block timeout=600000",
        "await consent",
        "await seq 40",
        "await inbox since=12",
        "@s-a await agent idle,prompt since=7",
        "inbox get 4",
        "inbox since=7",
        "inbox 16 since=7",
        "@s-a await seq 3 timeout=500",
        "await log close=turn",
        "@s-a await log kind=verdict timeout=5000",
        "await log open=step timeout=1000",
        "await log close=turn,kind=note",
        "await log need,close=turn",
        "@s-a await log close=turn,need timeout=600000",
        "await log need,kind=note timeout=5000",
        "@s-a await edit ticket=7 timeout=600000",
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

/// A since-less `await log` cut by a replacement is not asked again, and its
/// note says how to wait again without missing what the update wrote: from
/// `status`'s cursor.
#[test]
fn a_cut_since_less_await_log_says_to_wait_again_from_status() {
    let parts = |s: &str| -> Vec<String> { s.split_whitespace().map(String::from).collect() };
    for request in [
        "@s-a await log close=turn timeout=600000",
        "@s-a await log need,close=turn timeout=600000",
    ] {
        let cut = parts(request);
        assert!(reports_replacement(&cut), "{request}");
        assert!(!retries_across_replacement(&cut), "{request}");
        let note = not_asked_again_note(&cut);
        assert!(
            note.contains("since=<log_seq> ep=<log_ep>"),
            "{request}: {note}"
        );
    }
}

/// A cut `await edit ticket=` is not asked again (no successor knows the
/// ticket), and its note says to wait by the file, which a successor does.
#[test]
fn a_cut_edit_wait_by_ticket_says_to_wait_by_the_file() {
    let parts = |s: &str| -> Vec<String> { s.split_whitespace().map(String::from).collect() };
    let cut = parts("@s-a await edit ticket=7 timeout=600000");
    assert!(reports_replacement(&cut));
    assert!(!retries_across_replacement(&cut));
    let note = not_asked_again_note(&cut);
    assert!(note.contains("await edit uri=<file-uri>"), "{note}");
}

/// A cut `turn` or `post` is reported as a replacement (exit 75, a note that it
/// may already have taken effect), never as a bare error a caller would retry,
/// and is never asked again; with nothing answering in time (`Hangup::Gone`)
/// its gone note carries the same advice.
#[test]
fn a_cut_turn_or_post_is_reported_as_a_replacement_and_never_asked_again() {
    let parts = |s: &str| -> Vec<String> { s.split_whitespace().map(String::from).collect() };
    let gone = gone_note(Some(7), Duration::from_secs(30), false, None);
    let head = gone
        .strip_suffix(" (exit 75)")
        .expect("a gone note ends in its exit");
    for (write, reported) in [
        ("turn hi", true),
        ("@s-a turn timeout=1800000 implement X", true),
        ("@3 turn go", true),
        ("post to=@s-b hi", true),
        ("@s-a post to=@s-b kind=task --wait-ack=60000 go", true),
        ("send hi", false),
        ("key enter", false),
    ] {
        assert_eq!(reports_replacement(&parts(write)), reported, "{write}");
        assert!(!retries_across_replacement(&parts(write)), "{write}");
        let note = gone_request_note(gone.clone(), &parts(write));
        if reported {
            assert!(note.starts_with(head), "{write}: {note}");
            assert!(
                note.contains(&not_asked_again_note(&parts(write))),
                "{write}: {note}"
            );
            assert!(note.ends_with(" (exit 75)"), "{write}: {note}");
        } else {
            assert_eq!(note, gone, "{write}");
        }
    }
    assert_eq!(gone_request_note(gone.clone(), &parts("text")), gone);
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

/// A successor bound for a verdict that must be reached: a one-minute hang
/// detector, left the moment the server answers.
#[cfg(unix)]
const REACHED: Duration = Duration::from_secs(60);

/// A successor bound only a regression waits out (a call that should not wait,
/// or should stop at its own deadline): far enough past [`REACHED`] to tell the
/// two apart under any load.
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
    assert!(matches!(
        follow.after_hangup(&path, Some(gone), None, None),
        Hangup::Gone {
            by_deadline: false,
            ..
        }
    ));

    let (tx, _rx) = std::sync::mpsc::channel();
    let server = fake_server(
        aterm_uds::CtlListener::bind(&sock).unwrap(),
        sock.clone(),
        "",
        false,
        tx,
    );
    assert_eq!(
        served.after_hangup(&path, Some(own), None, None),
        Hangup::StillServing
    );
    assert_eq!(
        served.after_hangup(&path, Some(gone), None, None),
        Hangup::Successor {
            path: path.clone(),
            pid: own
        }
    );
    stop(&sock);
    server.join().unwrap();
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

/// A cold update cuts every stream for an `exec` that keeps the pid (aterm-gui's
/// `Endpoint::retire`), then binds a fresh listener at the same path: the stream
/// follows the same pid there, under `--sock` and `--pid` alike, and that
/// listener ending the resumed stream ends it 0.
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
        assert_eq!(text, "sub 1 s-a\nGAP 1 replaced\nsub 1 s-b\n", "{flag}");
        assert_eq!(code, ExitCode::SUCCESS, "{flag}: {text}");
    }
}

/// A `--pid` pin whose process ended unreaped (a zombie `kill(pid, 0)` still
/// finds) is gone at once, without waiting out the successor bound.
#[cfg(unix)]
#[test]
fn a_pid_pin_whose_process_has_ended_unreaped_is_gone_at_once() {
    use std::io::Read as _;
    let (dir, sock) = scratch_sock("rd-pin-zombie");
    let mut old = ChildSuccessor::spawn(dir.path(), "old", &sock);
    let pid = old.pid();
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

/// A local target the stream never tied to a sid (no `sub` line read) is not
/// followed: a successor numbers adopted sessions afresh, so `@3` there may
/// be another session.
#[cfg(unix)]
#[test]
fn a_subscription_by_an_untied_local_number_is_not_followed() {
    let (_dir, sock) = scratch_sock("rd-local-untied");
    let path = sock.to_str().unwrap().to_string();
    let request = "subscribe @3 cursor,events\n";
    let (seen_tx, seen) = std::sync::mpsc::channel();
    let old = fake_server(
        aterm_uds::CtlListener::bind(&sock).unwrap(),
        sock.clone(),
        "OK subscribe 1\n",
        true,
        seen_tx.clone(),
    );
    let stream = CtlStream::connect(&sock).expect("connect");
    let mut reader = BufReader::new(&stream);
    handshake(&stream, &mut reader, request);
    old.join().unwrap();
    let successor = fake_server(
        aterm_uds::CtlListener::bind(&sock).unwrap(),
        sock.clone(),
        "OK subscribe 1\nsub 1 s-aaaa\nEVENT 1 exited\n",
        false,
        seen_tx,
    );
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
        &mut Vec::new(),
    )
    .expect("relay");
    stop(&sock);
    successor.join().unwrap();
    assert_eq!(code, ExitCode::from(EXIT_REPLACED));
    assert_eq!(
        seen.try_iter().count(),
        1,
        "the successor was asked nothing"
    );
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

/// An instance that said it was quitting (`EVENT * bye quit`) and hung up has
/// no successor to wait for: exit 75 at once, with the bound a successor wait
/// would take far past the hang detector (a stream that waited would end
/// only at that bound, [`FAR`]).
#[cfg(unix)]
#[test]
fn a_stream_whose_instance_said_bye_quit_ends_at_once_with_75() {
    let mut t = FrameTracker::new("subscribe @* events,sessions\n");
    t.feed(b"sub 1 s-a\nEVENT * bye handoff\nDELTA 1 seq=2 screen 1\nEVENT * bye quit\n");
    assert!(!t.said_quit(), "a handoff, and a screen row");
    t.feed(b"EVENT * bye quit\n");
    assert!(t.said_quit());
    t.start_leg(LegSession::Same);
    assert!(!t.said_quit(), "a new leg is a new instance's");

    let (_dir, sock) = scratch_sock("rd-quit");
    let path = sock.to_str().unwrap().to_string();
    let request = "subscribe @* events,sessions\n";
    let (tx, _rx) = std::sync::mpsc::channel();
    let old = fake_server(
        aterm_uds::CtlListener::bind(&sock).unwrap(),
        sock.clone(),
        "OK subscribe 1\nsub 1 s-a\nEVENT * bye quit\n",
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
            server: Some(gone_pid()),
            listener: None,
        },
        request,
        None,
        &Redial::new(Follow::Path(path.clone())).with_bound(FAR),
        &mut out,
    )
    .unwrap();
    assert_eq!(code, ExitCode::from(EXIT_REPLACED));
    assert!(started.elapsed() < REACHED, "{:?}", started.elapsed());
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("EVENT * bye quit\n")
    );
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
    let (code, _) = relay(
        &stream,
        &mut reader,
        &path,
        Some(std::process::id()),
        request,
        &Redial::new(Follow::Path(path.clone())).with_bound(FAR),
    );
    assert_eq!(code, ExitCode::SUCCESS);
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
    let served = Redial::new(Follow::Path(path.clone())).with_bound(REACHED);
    let gone = gone_pid();

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

/// An explicit `--timeout` bounds the wait for a successor: the classification,
/// a `subscribe` relay and a blocking read each give up at the deadline (exit
/// 75).
#[cfg(unix)]
#[test]
fn an_explicit_timeout_bounds_the_wait_for_a_successor() {
    let (_dir, sock) = scratch_sock("rd-deadline");
    let path = sock.to_str().unwrap().to_string();
    let gone = gone_pid();
    let deadline = Duration::from_secs(2);
    let short = Duration::from_millis(200);
    let until = Instant::now() + short;
    let ends = |w: Wait| (w.deadline, w.by_deadline, w.started);
    let (end, by_deadline, _) = ends(Wait::from_now(FAR, Some(until)));
    assert_eq!((end, by_deadline), (until, true));
    for later in [Some(until + FAR), None] {
        let (end, by_deadline, started) = ends(Wait::from_now(short, later));
        assert_eq!((end, by_deadline), (started + short, false), "{later:?}");
    }
    let (_refusing_dir, refusing) = scratch_sock("rd-passed");
    let listener = aterm_uds::CtlListener::bind(&refusing).unwrap();
    let looks = std::thread::spawn(move || {
        let mut looks = 0;
        while let Ok((conn, _)) = listener.accept() {
            let mut request = String::new();
            let _ = BufReader::new(&conn).read_line(&mut request);
            if request.starts_with("stop") {
                break;
            }
            looks += 1;
            let _ = (&conn).write_all(b"ERR busy\n");
        }
        looks
    });
    let nowhere = refusing.with_file_name("nowhere.sock");
    let follow = Redial::new(Follow::Path(refusing.to_str().unwrap().to_string()));
    let verdict = follow.with_bound(FAR).after_host_gone(
        nowhere.to_str().unwrap(),
        gone,
        Some(Instant::now()),
    );
    stop(&refusing);
    assert!(
        matches!(verdict, Hangup::Gone { waited, by_deadline: true } if !waited.is_zero()),
        "{verdict:?}"
    );
    assert_eq!(looks.join().unwrap(), 1);
    let redial = Redial::new(Follow::Path(path.clone())).with_bound(FAR);
    let parts = |s: &str| -> Vec<String> { s.split_whitespace().map(String::from).collect() };
    let within_the_deadline = |started: Instant| {
        let took = started.elapsed();
        assert!(
            took < REACHED,
            "took {took:?}: the deadline, not the {FAR:?} bound, ends the wait"
        );
    };

    let started = Instant::now();
    let verdict = redial.after_hangup(&path, Some(gone), None, Some(started + short));
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

    let started = Instant::now();
    let code = super::super::replaced_before_reply(
        &path,
        Some(gone),
        None,
        &parts("@s-a await match prompt"),
        &redial,
        Some(started + short),
        |_| panic!("no successor to ask"),
        None,
    )
    .unwrap();
    assert_eq!(code, ExitCode::from(EXIT_REPLACED));
    within_the_deadline(started);
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
/// repoints.
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
        sid: Some(sid.to_string()),
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
        sid: None,
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
    publish_graph_entry(&dir, sid, &old_sock);
    let follow = Redial::new(Follow::Flagless {
        dir: dir.clone(),
        sid: Some(sid.to_string()),
    })
    .with_bound(Duration::from_millis(400));
    assert!(matches!(
        follow.after_hangup(old, Some(gone), None, None),
        Hangup::Gone { .. }
    ));
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
    let outside = Redial::new(Follow::Flagless {
        dir: dir.clone(),
        sid: None,
    })
    .with_bound(REACHED);
    assert!(matches!(
        outside.after_hangup(old, Some(gone), None, None),
        Hangup::Successor { pid, .. } if pid == own
    ));
    stop(&stranger_sock);
    stranger.join().unwrap();
}

/// The session a request is dialled for, and whose host a forward's hang-up
/// follows, is read by the wire's grammar: a leading `@<sid>` (SP or HT after
/// it), or the one `@<sid>` a selectorless `subscribe` names second.
#[test]
fn the_session_a_request_names_is_read_by_the_wire_grammar() {
    let named = |line: &str| target_sid(line).map(|sid| sid.as_str().to_string());
    for (line, sid) in [
        ("@s-ab focus in\n", "s-ab"),
        ("@s-ab\tfocus in", "s-ab"),
        ("subscribe @s-ab screen\n", "s-ab"),
        ("subscribe\t@s-ab screen", "s-ab"),
        ("@s-ab subscribe @s-cd screen", "s-ab"),
    ] {
        assert_eq!(named(line).as_deref(), Some(sid), "{line:?}");
    }
    for line in [
        "@. focus in",
        "@ focus in",
        "@1 focus in",
        "@S-AB focus in",
        "subscribe @s-ab,@s-cd screen",
        "subscribe @* screen",
        "subscribe screen",
        "focus in",
    ] {
        assert_eq!(named(line), None, "{line:?}");
    }
}

/// A request naming another instance's session is dialled at that session's
/// host ([`super::super::targets_in`]), so a hang-up there follows that
/// session's graph entry: the caller's own instance, alive throughout and named
/// by the caller's own entry, is no successor of the peer's host; the host's
/// successor is, once it republishes the peer's entry.
#[cfg(unix)]
#[test]
fn a_call_to_a_peer_follows_the_peers_host_not_its_own() {
    let (_scratch, dir) = scratch_rendezvous("rd-peer");
    let gone = gone_pid();
    let live = std::process::id();
    let old_sock = dir.join(format!("aterm-{gone}.sock"));
    let old = old_sock.to_str().unwrap();
    let own_sock = dir.join("aterm-1.sock");
    let new_sock = dir.join("aterm-2.sock");
    let (peer, me) = ("s-0a1b", "s-2c3d");
    let (tx, _rx) = std::sync::mpsc::channel();
    let mine = fake_server(
        aterm_uds::CtlListener::bind(&own_sock).unwrap(),
        own_sock.clone(),
        "",
        false,
        tx.clone(),
    );
    publish_graph_entry(&dir, me, &own_sock);
    publish_graph_entry(&dir, peer, &old_sock);
    let targets = super::super::targets_in(
        Some(dir.clone()),
        None,
        None,
        Some(me.to_string()),
        aterm_types::control_request::Sid::parse(peer),
    )
    .expect("targets");
    let [to_peer, to_me] = targets.hosting.as_slice() else {
        panic!("the peer's host, then the caller's own: {targets:?}");
    };
    assert_eq!(to_peer.path, old);

    let peers_host = Redial::new(to_peer.follow.clone()).with_bound(Duration::from_millis(400));
    assert!(matches!(
        peers_host.after_hangup(old, Some(gone), None, None),
        Hangup::Gone { .. }
    ));
    let own_host = Redial::new(to_me.follow.clone()).with_bound(REACHED);
    assert_eq!(
        own_host.after_hangup(old, Some(gone), None, None),
        Hangup::Successor {
            path: own_sock.to_str().unwrap().to_string(),
            pid: live
        }
    );

    let successor = fake_server(
        aterm_uds::CtlListener::bind(&new_sock).unwrap(),
        new_sock.clone(),
        "",
        false,
        tx,
    );
    publish_graph_entry(&dir, peer, &new_sock);
    let peers_host = Redial::new(to_peer.follow.clone()).with_bound(REACHED);
    assert_eq!(
        peers_host.after_hangup(old, Some(gone), None, None),
        Hangup::Successor {
            path: new_sock.to_str().unwrap().to_string(),
            pid: live
        }
    );
    stop(&own_sock);
    stop(&new_sock);
    mine.join().unwrap();
    successor.join().unwrap();
}

/// Where a redial looks follows how the call resolved
/// ([`super::super::targets_in`]): a `--pid` pin follows no successor but its
/// own process, `--sock` its path, and the flagless rule the calling session's
/// graph entry or the alias.
#[test]
fn a_redial_follows_what_the_call_resolved_through() {
    use super::super::targets_in;
    let dir = PathBuf::from("/r");
    let sid = Some("s-ab".to_string());
    let follow = |sock: Option<&str>, pid: Option<u32>, sid: Option<String>| {
        targets_in(Some(dir.clone()), sock.map(String::from), pid, sid, None)
            .expect("targets")
            .last_target()
            .follow
    };
    assert_eq!(
        follow(None, Some(42), sid.clone()),
        Follow::Nothing(Unfollowed::PidPin(42)),
        "--pid"
    );
    assert_eq!(
        follow(Some("/x.sock"), None, sid.clone()),
        Follow::Path("/x.sock".to_string()),
        "--sock"
    );
    assert_eq!(
        follow(None, None, sid.clone()),
        Follow::Flagless {
            dir: dir.clone(),
            sid: sid.clone()
        }
    );
    assert_eq!(
        follow(None, None, None),
        Follow::Flagless {
            dir: dir.clone(),
            sid: None
        },
        "outside a session the flagless rule follows the alias alone"
    );
    assert!(
        targets_in(None, None, None, sid, None).is_err(),
        "no rendezvous dir"
    );
}
