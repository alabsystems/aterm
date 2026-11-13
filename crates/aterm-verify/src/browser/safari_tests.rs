// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[cfg(target_vendor = "apple")]
use aterm_uds::process::Birth;

/// A stand-in's next connection, blocking: macOS hands an accepted socket the listener's
/// `O_NONBLOCK`, so a read before the request arrived would fail `EAGAIN`. `Err` once `done`
/// reads true before one arrives, or the listener fails.
fn next_connection(listener: &TcpListener, done: &dyn Fn() -> bool) -> std::io::Result<TcpStream> {
    loop {
        match aterm_uds::accept(
            listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ) {
            Ok((stream, _)) => return Ok(stream),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && !done() => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => return Err(e),
        }
    }
}

/// The stand-in driver's one connection ([`next_connection`]), within the hang detector.
fn accept(listener: &TcpListener) -> TcpStream {
    let until = hang_deadline(Instant::now());
    next_connection(listener, &|| Instant::now() >= until)
        .unwrap_or_else(|e| panic!("the inert driver did not receive its connection: {e}"))
}

/// A stand-in's connection blocks though its listener does not.
#[cfg(unix)]
#[test]
fn a_stand_in_connection_blocks() {
    use std::os::fd::AsRawFd as _;
    let nonblocking = |s: &TcpStream| {
        // SAFETY: `F_GETFL` reads the flags of a descriptor `s` owns.
        let flags = unsafe { libc::fcntl(s.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0, "{}", std::io::Error::last_os_error());
        flags & libc::O_NONBLOCK != 0
    };
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listen");
    let port = listener.local_addr().expect("addr").port();
    let _client = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    assert!(!nonblocking(&accept(&listener)), "a stand-in's connection");
}

/// Only macOS has a Safari, and only where its driver is.
#[test]
fn only_macos_with_its_driver_has_a_safari() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let driver = dir.path().join("safaridriver");
    assert!(find_at("linux", &driver).is_err_and(|e| e.contains("macOS's own")));
    assert!(find_at("macos", &driver).is_err_and(|e| e.contains("not here")));
    std::fs::write(&driver, "").expect("driver");
    assert_eq!(find_at("macos", &driver).expect("found").driver, driver);
}

/// A New Session answer gives its id; a refusal for remote automation names the
/// administrator's one-time fix, and another refusal names no fix.
#[test]
fn a_refused_session_names_the_enable_fix_only_for_remote_automation() {
    let ok = json!({"value": {"sessionId": "S1", "capabilities": {}}});
    assert_eq!(session_id(200, &ok).as_deref(), Ok("S1"));
    let off = json!({"value": {"error": "session not created", "message":
        "You must enable 'Allow remote automation' in the Developer section of Safari \
         Settings to control Safari via WebDriver.", "stacktrace": ""}});
    let why = session_id(500, &off).expect_err("refused");
    assert!(why.contains("sudo safaridriver --enable"), "{why}");
    assert!(why.contains("session not created"), "{why}");
    let busy = json!({"value": {"error": "session not created", "message":
        "Could not create a session: The Safari instance is already paired with another \
         WebDriver session."}});
    let why = session_id(500, &busy).expect_err("refused");
    assert!(
        !why.contains("--enable") && why.contains("already paired"),
        "{why}"
    );
    assert!(
        session_id(500, &ok).is_err(),
        "a sessionId on an error status"
    );
}

/// The client reads a body by its length, by its chunks, or to the close.
#[test]
fn an_answer_is_read_by_length_chunks_or_close() {
    let by_length = b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\n\r\n{\"value\":null}trail";
    assert_eq!(parse_answer(by_length), Ok((200, json!({"value": null}))));
    let chunked = b"HTTP/1.1 500 Internal\r\ntransfer-encoding: Chunked\r\n\r\n\
                    6\r\n{\"valu\r\n7;x=y\r\ne\":\"s\"}\r\n0\r\n\r\n";
    assert_eq!(parse_answer(chunked), Ok((500, json!({"value": "s"}))));
    let to_close = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{\"a\":1}";
    assert_eq!(parse_answer(to_close), Ok((200, json!({"a": 1}))));
    assert_eq!(
        parse_answer(b"HTTP/1.1 200 OK\r\n\r\n"),
        Ok((200, Value::Null))
    );
    assert!(parse_answer(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n{}").is_err());
    assert!(parse_answer(b"HTTP/1.1 200 OK\r\n").is_err());
    assert!(parse_answer(b"HTTP/1.1 200 OK\r\n\r\n<html>").is_err());
    let overflowing = format!(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\nx\r\n0\r\n\r\n",
        usize::MAX
    );
    assert!(parse_answer(overflowing.as_bytes()).is_err());
    assert!(
        parse_answer(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}xx0\r\n\r\n")
            .is_err()
    );
}

/// Ordinary partial I/O completes, but bytes trickling through the same reader cannot
/// renew the command's deadline. The clock advances in the I/O seam, with no sleeps.
#[test]
fn progress_does_not_renew_a_commands_wall_deadline() {
    use std::cell::Cell;
    let start = Instant::now();
    let deadline = hang_deadline(start);
    let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
    let mut at = 0;
    let ordinary = exchange(
        b"request",
        deadline,
        || start,
        |_, bytes| Ok(bytes.len().min(2)),
        |_, out| {
            if at == response.len() {
                return Ok(0);
            }
            out[0] = response[at];
            at += 1;
            Ok(1)
        },
        "ordinary",
    )
    .unwrap();
    assert_eq!(parse_answer(&ordinary), Ok((200, json!({}))));

    let elapsed = Cell::new(Duration::ZERO);
    let reads = Cell::new(0);
    let drip = exchange(
        b"request",
        deadline,
        || start + elapsed.get(),
        |_, bytes| Ok(bytes.len()),
        |_, out| {
            out[0] = b' ';
            reads.set(reads.get() + 1);
            elapsed.set(elapsed.get() + HANG_DETECTOR / 4);
            Ok(1)
        },
        "dripping answer",
    );
    assert!(
        reads.get() > 1,
        "the reader must actually keep making progress"
    );
    assert!(
        drip.is_err(),
        "progress cannot renew the same command's deadline"
    );
    assert_eq!(elapsed.get(), HANG_DETECTOR);
}

#[test]
fn an_answer_over_the_byte_limit_is_refused_instead_of_truncated() {
    let start = Instant::now();
    let deadline = hang_deadline(start);
    let answer = exchange(
        b"request",
        deadline,
        || start,
        |_, bytes| Ok(bytes.len()),
        |_, out| {
            out.fill(b'x');
            Ok(out.len())
        },
        "oversized answer",
    );
    assert!(answer.is_err());
}

/// Cleanup addresses this session only. A driver with other sessions sees no request for
/// them, and a refusal stays a refusal when cleanup is called again or the guard drops.
#[test]
fn cleanup_addresses_only_the_owned_session_and_preserves_refusals() {
    for status in [200, 500] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let driver = std::thread::spawn(move || {
            let mut stream = accept(&listener);
            stream.set_read_timeout(Some(HANG_DETECTOR)).unwrap();
            stream.set_write_timeout(Some(HANG_DETECTOR)).unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                assert_eq!(stream.read(&mut byte).unwrap(), 1);
                request.push(byte[0]);
            }
            let line = std::str::from_utf8(&request)
                .unwrap()
                .lines()
                .next()
                .unwrap();
            let mut contexts = std::collections::BTreeSet::from(["person", "other", "ours"]);
            assert_eq!(line, "DELETE /session/ours HTTP/1.1");
            if status == 200 {
                contexts.remove("ours");
            }
            write!(stream, "HTTP/1.1 {status} Answer\r\nContent-Length: 14\r\nConnection: close\r\n\r\n{{\"value\":null}}").unwrap();
            contexts
        });
        let mut session = Session::new(port, "ours".to_owned());
        let first = session.close();
        let contexts = driver.join().unwrap();
        assert_eq!(first.is_ok(), status == 200);
        assert!(contexts.contains("person") && contexts.contains("other"));
        assert_eq!(contexts.contains("ours"), status != 200);
        assert_eq!(
            session.close(),
            first,
            "the result is kept without another request"
        );
        drop(session);
    }
}

/// A command goes out as one request with its JSON body, and its answer comes back parsed.
#[test]
fn a_command_round_trips_through_a_stand_in_driver() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listen");
    let port = listener.local_addr().expect("addr").port();
    let driver = std::thread::spawn(move || {
        let mut s = accept(&listener);
        s.set_read_timeout(Some(HANG_DETECTOR)).unwrap();
        s.set_write_timeout(Some(HANG_DETECTOR)).unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        while !got.ends_with(b"}") {
            let n = s.read(&mut buf).expect("read");
            assert!(n > 0, "the request ended early: {got:?}");
            got.extend_from_slice(&buf[..n]);
        }
        s.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Length: 30\r\n\r\n{\"value\":{\"sessionId\":\"S9\"}}  ",
        )
        .expect("answer");
        String::from_utf8(got).expect("utf-8")
    });
    let (status, body) = command(port, "POST", "/session", Some(&new_session())).expect("command");
    assert_eq!(session_id(status, &body).as_deref(), Ok("S9"));
    let request = driver.join().expect("driver");
    assert!(
        request.starts_with("POST /session HTTP/1.1\r\n"),
        "{request}"
    );
    let (_, sent) = request.split_once("\r\n\r\n").expect("body");
    assert_eq!(
        aterm_json::from_slice::<Value>(sent.as_bytes()).expect("json"),
        new_session()
    );
}

/// A stand-in server on a port of its own that answers each connection's request with
/// `answer`, until the test ends.
struct Answering {
    port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Answering {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Each connection's request is read to its head's end (the client's request may come in
/// more than one segment, and one closed on before it is read whole is answered with a
/// reset), then answered.
fn answering(answer: &'static [u8]) -> Answering {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listen");
    let port = listener.local_addr().expect("addr").port();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped = std::sync::Arc::clone(&stop);
    let worker = std::thread::spawn(move || {
        let done = || stopped.load(std::sync::atomic::Ordering::SeqCst);
        while let Ok(mut s) = next_connection(&listener, &done) {
            s.set_read_timeout(Some(HANG_DETECTOR)).unwrap();
            s.set_write_timeout(Some(HANG_DETECTOR)).unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut buf) {
                    Ok(n) if n > 0 => request.extend_from_slice(&buf[..n]),
                    _ => break,
                }
            }
            let _ = s.write_all(answer);
        }
    });
    Answering {
        port,
        stop,
        worker: Some(worker),
    }
}

/// The driver is listening once a WebDriver answers `GET /status` on its port, whatever it
/// says about being ready. NEGATIVE CONTROL: a port another program answers on, with no
/// WebDriver status, is refused at once as that program's; and a driver that ended is said.
#[test]
fn a_port_listens_once_a_webdriver_answers_its_status() {
    let ready = answering(
        b"HTTP/1.1 200 OK\r\nContent-Length: 38\r\n\r\n{\"value\":{\"ready\":false,\"message\":\"\"}}",
    );
    assert_eq!(listening(ready.port, || None), Ok(()));
    let other = answering(b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\n\r\n{}");
    let why = listening(other.port, || None).expect_err("another program's port");
    assert!(why.contains("HTTP 404, not as a WebDriver"), "{why}");
    let why = listening(ready.port, || Some("exit status: 1".to_string())).expect_err("ended");
    assert!(
        why.contains("safaridriver exited (exit status: 1)"),
        "{why}"
    );
}

/// A stand-in answers a request once its head is whole, so one that comes in two segments
/// is answered, never reset. The first half reads no answer in a tenth of a second, which a
/// stand-in that waits for the whole head never gives. NEGATIVE CONTROL: answer after one
/// read, and the first half is answered at once.
#[test]
fn a_stand_in_answers_once_the_requests_head_is_whole() {
    let server = answering(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}");
    let mut client = TcpStream::connect(("127.0.0.1", server.port)).expect("connect");
    client
        .write_all(b"GET /status HTTP/1.1\r\n")
        .expect("half the head");
    client
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let mut buf = [0u8; 64];
    let early = client.read(&mut buf);
    assert!(
        matches!(&early, Err(e) if matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        )),
        "answered half a head: {early:?}"
    );
    client
        .write_all(b"Host: 127.0.0.1\r\n\r\n")
        .expect("the rest of the head");
    client.set_read_timeout(Some(HANG_DETECTOR)).unwrap();
    let mut answer = Vec::new();
    client.read_to_end(&mut answer).expect("the answer");
    assert!(
        answer.starts_with(b"HTTP/1.1 200 OK"),
        "{}",
        String::from_utf8_lossy(&answer)
    );
}

/// The session's Safari has quit once every Safari listed for it is gone: a process that
/// still reads as itself runs. NEGATIVE CONTROL: a listing of none tells nothing.
#[cfg(target_vendor = "apple")]
#[test]
fn the_sessions_safari_quit_once_every_one_listed_is_gone() {
    let me = aterm_uds::process::read_identity(i32::try_from(std::process::id()).expect("a pid"))
        .expect("this process's identity");
    let gone = Identity {
        birth: Birth(me.birth.0.wrapping_add(1)),
        ..me
    };
    assert_eq!(quit(&[me]), None, "it runs");
    assert_eq!(quit(&[gone, me]), None, "one of them runs");
    assert_eq!(quit(&[gone]), Some(format!("Safari (pid {}) quit", me.pid)));
    assert_eq!(quit(&[]), None, "none listed");
}

/// Safari's user agent is Safari's and names its version; Playwright's bare WebKit and
/// Chromium's (which says `Safari/` too) are not.
#[test]
fn only_safari_s_user_agent_is_safari() {
    let safari = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
                  (KHTML, like Gecko) Version/27.0.1 Safari/605.1.15";
    let bare = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, \
                like Gecko)";
    let chrome = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                  (KHTML, like Gecko) HeadlessChrome/155.0.8059.84 Safari/537.36";
    assert!(is_safari(safari));
    assert!(!is_safari(bare) && !is_safari(chrome) && !is_safari(""));
    assert_eq!(product(safari), "Safari 27.0.1");
    assert_eq!(product(chrome), chrome);
}

/// A process path identifies the application for observation, never cleanup ownership.
#[test]
fn a_safari_process_path_is_only_an_observation() {
    assert!(is_safari_app(Path::new(
        "/System/Volumes/Preboot/Cryptexes/App/System/Applications/Safari.app/Contents/\
         MacOS/Safari"
    )));
    assert!(!is_safari_app(Path::new(
        "/Applications/Safari Technology Preview.app/Contents/MacOS/Safari Technology Preview"
    )));
}
