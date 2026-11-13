// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

struct FragmentTransport {
    base: std::time::Instant,
    now: std::cell::Cell<std::time::Instant>,
    timeout: std::cell::Cell<std::time::Duration>,
    step: std::time::Duration,
    write_step: std::time::Duration,
    flush_step: std::time::Duration,
    write_chunk: usize,
    round_up: bool,
    fragments: std::collections::VecDeque<Vec<u8>>,
    sent: Vec<u8>,
}

impl FragmentTransport {
    fn new(fragments: &[&[u8]], step: std::time::Duration) -> Self {
        let base = std::time::Instant::now();
        Self {
            base,
            now: std::cell::Cell::new(base),
            timeout: std::cell::Cell::new(std::time::Duration::MAX),
            step,
            write_step: std::time::Duration::ZERO,
            flush_step: std::time::Duration::ZERO,
            write_chunk: usize::MAX,
            round_up: false,
            fragments: fragments.iter().map(|bytes| bytes.to_vec()).collect(),
            sent: Vec::new(),
        }
    }

    fn advance(&self, step: std::time::Duration) -> std::io::Result<()> {
        let allowed = self.timeout.get();
        let spent = if self.round_up {
            step
        } else {
            step.min(allowed)
        };
        self.now.set(self.now.get().checked_add(spent).unwrap());
        if step > allowed && !self.round_up {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        Ok(())
    }
}

impl std::io::Read for FragmentTransport {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.fragments.is_empty() {
            return Ok(0);
        }
        self.advance(self.step)?;
        let part = self.fragments.front_mut().unwrap();
        let count = part.len().min(buf.len());
        buf[..count].copy_from_slice(&part[..count]);
        part.drain(..count);
        if part.is_empty() {
            self.fragments.pop_front();
        }
        Ok(count)
    }
}

impl std::io::Write for FragmentTransport {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.advance(self.write_step)?;
        let count = bytes.len().min(self.write_chunk);
        self.sent.extend_from_slice(&bytes[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.advance(self.flush_step)
    }
}

fn arm_fragments(io: &FragmentTransport, wait: std::time::Duration) -> std::io::Result<()> {
    io.timeout.set(wait);
    Ok(())
}

fn fragment_clock(io: &FragmentTransport) -> std::time::Instant {
    io.now.get()
}

/// The injected clock advances only when a fragment's simulated blocking read completes.
/// Four 3ms fragments each satisfy a 10ms per-I/O timeout, but exceed one exchange budget.
#[test]
fn a_fragmented_counted_reply_keeps_one_exchange_budget() {
    let wait = std::time::Duration::from_millis(10);
    let io = FragmentTransport::new(
        &[b"OK 1\n", b"r", b"o", b"w\n"],
        std::time::Duration::from_millis(3),
    );
    let mut client = RelayClient::new(io);
    client.bound = Some((arm_fragments, wait));
    client.clock = fragment_clock;
    let got = client.request_counted("text");
    let elapsed = (client.clock)(&client.io).duration_since(client.io.base);
    assert!(
        matches!(got.as_ref(), Err(e) if e.kind() == std::io::ErrorKind::TimedOut),
        "reply {got:?}, controlled elapsed {elapsed:?}, exchange budget {wait:?}"
    );
    assert!(
        client.request_line("status").is_err(),
        "a partial body must poison the connection"
    );
    assert_eq!(client.io.sent, b"text\n");
}

fn bounded_fragments(
    io: FragmentTransport,
    wait: std::time::Duration,
) -> RelayClient<FragmentTransport> {
    let mut client = RelayClient::new(io);
    client.bound = Some((arm_fragments, wait));
    client.clock = fragment_clock;
    client
}

#[test]
fn fragment_control_per_io_timeouts_accept_a_late_counted_body() {
    let ms = std::time::Duration::from_millis;
    let io = FragmentTransport::new(&[b"OK 1\n", b"r", b"o", b"w\n"], ms(3));
    // Actual old behavior: the transport is armed once, with no total client budget.
    arm_fragments(&io, ms(10)).unwrap();
    let mut legacy = RelayClient::new(io);
    assert_eq!(
        legacy.request_counted("text").unwrap(),
        ("OK 1".into(), "row\n".into())
    );
    assert_eq!(legacy.io.now.get().duration_since(legacy.io.base), ms(12));
}

#[test]
fn fragment_deadline_checks_late_operations_and_byte_bodies() {
    let ms = std::time::Duration::from_millis;
    for round_up in [false, true] {
        let mut io = FragmentTransport::new(&[b"OK 4\n", b"a", b"b", b"cd"], ms(3));
        io.round_up = round_up;
        let mut client = bounded_fragments(io, ms(10));
        assert_eq!(
            client.request_bytes("image --bytes").unwrap_err().kind(),
            std::io::ErrorKind::TimedOut
        );
        assert!(client.request_frame("feed-bin", b"later").is_err());
        assert_eq!(client.io.sent, b"image --bytes\n");
    }
    let io = FragmentTransport::new(&[b"OK 2\n", b"\0", b"\xff", b"OK next\n"], ms(2));
    let mut client = bounded_fragments(io, ms(10));
    assert_eq!(
        client.request_bytes("image --bytes").unwrap(),
        ("OK 2".into(), vec![0, 255])
    );
    assert_eq!(client.request_line("status").unwrap(), "OK next");
}

#[test]
fn fragment_deadline_includes_partial_writes_and_flush() {
    let ms = std::time::Duration::from_millis;
    for frame in [false, true] {
        let mut io = FragmentTransport::new(&[b"OK done\n"], ms(3));
        io.write_chunk = 2;
        io.write_step = ms(2);
        io.flush_step = ms(2);
        let mut client = bounded_fragments(io, ms(10));
        let got = if frame {
            client.request_frame("feed-bin", b"x")
        } else {
            client.request_line("status")
        };
        assert_eq!(got.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        assert!(!client.io.sent.is_empty());
        let sent = client.io.sent.clone();
        assert!(client.request_line("later").is_err());
        assert_eq!(client.io.sent, sent);
    }
    let mut io = FragmentTransport::new(&[b"OK done\n"], ms(3));
    io.write_chunk = 4;
    io.write_step = ms(1);
    io.flush_step = ms(1);
    let mut client = bounded_fragments(io, ms(10));
    assert_eq!(client.request_frame("feed-bin", b"x").unwrap(), "OK done");
    assert_eq!(client.io.sent, b"feed-bin 1\nx");
}

#[test]
fn fragment_deadlines_preserve_named_waits_and_reset_between_requests() {
    let ms = std::time::Duration::from_millis;
    let io = FragmentTransport::new(&[b"OK 1\n", b"r", b"o", b"w\n", b"OK next\n"], ms(3));
    let mut client = bounded_fragments(io, ms(10));
    assert_eq!(
        client.request_counted("await idle timeout=10").unwrap(),
        ("OK 1".into(), "row\n".into())
    );
    assert_eq!(client.io.now.get().duration_since(client.io.base), ms(12));
    assert_eq!(client.request_line("status").unwrap(), "OK next");
}

#[test]
fn fragment_stream_reads_keep_transport_timeout_semantics() {
    let ms = std::time::Duration::from_millis;
    let io = FragmentTransport::new(
        &[b"OK\n", b"stream", b" progress", b" remains", b"\n"],
        ms(3),
    );
    let mut client = bounded_fragments(io, ms(10));
    assert_eq!(client.request_line("subscribe").unwrap(), "OK");
    assert_eq!(client.next_pushed().unwrap(), "stream progress remains");
    assert_eq!(client.io.now.get().duration_since(client.io.base), ms(15));

    let io = FragmentTransport::new(&[b"OK\n", b"stream\n"], ms(3));
    let mut client = bounded_fragments(io, ms(10));
    assert_eq!(client.request_line("subscribe").unwrap(), "OK");
    client.transport().timeout.set(ms(1));
    assert_eq!(
        client.next_pushed().unwrap_err().kind(),
        std::io::ErrorKind::TimedOut
    );
}

#[test]
fn malformed_or_cut_bodies_poison_but_never_count_as_auth_close() {
    for bytes in [
        &b"OK 2\none\n"[..],
        &b"OK 1\n\xff\n"[..],
        &b"OK 99999999999999999999999999\n"[..],
    ] {
        let mut client = RelayClient::new(RecordingTransport::new(bytes));
        assert!(client.request_counted("version").is_err());
        assert!(client.reply_unread());
        assert!(!client.closed_before_reply());
        assert!(client.request_line("later").is_err());
        assert_eq!(client.io.sent, b"version\n");
    }
    let mut client = RelayClient::new(RecordingTransport::new(b""));
    assert_eq!(
        client.request_line("version").unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
    assert!(client.closed_before_reply());
}
use super::*;

struct RecordingTransport {
    sent: Vec<u8>,
    to_read: std::io::Cursor<Vec<u8>>,
    max_read: usize,
    error_at: Option<u64>,
}
impl RecordingTransport {
    fn new(canned: &[u8]) -> Self {
        Self {
            sent: Vec::new(),
            to_read: std::io::Cursor::new(canned.to_vec()),
            max_read: usize::MAX,
            error_at: None,
        }
    }
}
impl Read for RecordingTransport {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.error_at == Some(self.to_read.position()) {
            self.error_at = None;
            return Err(std::io::ErrorKind::WouldBlock.into());
        }
        let len = out.len().min(self.max_read);
        self.to_read.read(&mut out[..len])
    }
}
impl Write for RecordingTransport {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.sent.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A request whose reply was left unread leaves the connection refusing every later one,
/// unsent, so the late reply answers none of them; a refusal that wrote nothing leaves it
/// as it was.
#[test]
fn a_reply_left_unread_answers_no_later_request() {
    let mut client = RelayClient::new(RecordingTransport::new(b"OK late\nOK next\n"));
    client.io.error_at = Some(0);
    assert!(!client.reply_unread());
    let cut = client
        .request_line("status")
        .expect_err("the reply did not come");
    assert!(unanswered(&cut), "{cut:?}");
    assert!(client.reply_unread());
    let refused = client.request_counted("who").expect_err("never sent");
    assert_eq!(refused.kind(), std::io::ErrorKind::NotConnected);
    assert!(
        !unanswered(&refused),
        "a request never sent is no unanswered one"
    );
    assert!(client.request_bytes("image --bytes").is_err());
    assert!(client.request_frame("feed-bin", b"x").is_err());
    assert_eq!(
        client.io.sent, b"status\n",
        "nothing after the unread reply"
    );

    let mut client = RelayClient::new(RecordingTransport::new(b"OK 2\nrow\n")).reads_only();
    let cut = client
        .request_counted("text")
        .expect_err("a body cut short");
    assert_eq!(cut.kind(), std::io::ErrorKind::UnexpectedEof);
    assert!(
        client.request_line("status").is_err(),
        "nothing after a cut body"
    );

    let mut client =
        RelayClient::new(RecordingTransport::new(b"OK 1\nrow\nOK next\n")).reads_only();
    let line = client.request_line("status\nwho").expect_err("two lines");
    assert_eq!(line.kind(), std::io::ErrorKind::InvalidInput);
    let send = client
        .request_line("@s-1 send x")
        .expect_err("a read-only send");
    assert_eq!(send.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(
        client
            .request_counted("text")
            .expect("a refusal left it whole"),
        ("OK 1".to_string(), "row\n".to_string())
    );
    assert_eq!(client.request_line("status").expect("the next"), "OK next");
    assert!(!client.reply_unread());
}

#[test]
fn a_callers_reply_bound_counts_every_line_and_ends_an_oversize_connection() {
    let mut fits = RelayClient::new(RecordingTransport::new(b"OK 2\nab\ncd\n"));
    assert_eq!(
        fits.request_counted_with_limit("text", 6).unwrap().1,
        "ab\ncd\n"
    );
    let mut too_long = RelayClient::new(RecordingTransport::new(b"OK 2\nab\ncd\nOK next\n"));
    assert_eq!(
        too_long
            .request_counted_with_limit("text", 5)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidData
    );
    assert!(too_long.request_line("status").is_err());
    assert_eq!(too_long.io.sent, b"text\n");
}

std::thread_local! {
    static ARMED: std::cell::RefCell<Vec<std::time::Duration>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[test]
fn unknown_closed_peer_reads_never_certify_an_empty_auth_reply() {
    struct FailedRead(std::io::ErrorKind);
    impl Read for FailedRead {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(self.0.into())
        }
    }
    impl Write for FailedRead {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for kind in [
        std::io::ErrorKind::Interrupted,
        std::io::ErrorKind::WouldBlock,
        std::io::ErrorKind::InvalidData,
        std::io::ErrorKind::NotConnected,
    ] {
        let mut client = RelayClient::new(FailedRead(kind));
        assert!(!client.closed_peer_without_reply(), "{kind:?} is not EOF");
    }
    for kind in [
        std::io::ErrorKind::UnexpectedEof,
        std::io::ErrorKind::BrokenPipe,
        std::io::ErrorKind::ConnectionReset,
    ] {
        let mut client = RelayClient::new(FailedRead(kind));
        assert!(client.closed_peer_without_reply(), "pre-reply {kind:?}");
    }
    let mut empty = RelayClient::new(RecordingTransport::new(b""));
    assert!(empty.closed_peer_without_reply());
    for bytes in [b"partial".as_slice(), b"OK\n", b"\xff"] {
        let mut client = RelayClient::new(RecordingTransport::new(bytes));
        assert!(
            !client.closed_peer_without_reply(),
            "queued bytes {bytes:?}"
        );
    }
    let mut buffered = RelayClient::new(RecordingTransport::new(b""));
    buffered.buf.push(b'x');
    assert!(!buffered.closed_peer_without_reply());
}

#[cfg(unix)]
#[test]
fn an_os_disconnect_certifies_no_reply_only_before_any_byte_was_observed() {
    struct Disconnected;
    impl Read for Disconnected {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from_raw_os_error(libc::ENOTCONN))
        }
    }
    impl Write for Disconnected {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut client = RelayClient::new(Disconnected);
    assert!(client.closed_peer_without_reply());
    client.reply_started = true;
    assert!(!client.closed_peer_without_reply());
}

#[cfg(target_os = "macos")]
#[test]
fn closed_peer_timeout_errors_preserve_the_queued_reply() {
    for errno in [libc::EINVAL, libc::ENOTCONN] {
        let (mut near, mut far) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
        near.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
        assert!(accept_closed_arm(&near, Err(std::io::Error::from_raw_os_error(errno))).is_err());
        far.write_all(b"OK partial").unwrap();
        far.shutdown(std::net::Shutdown::Both).unwrap();
        drop(far);
        aterm_spec::wait::until_peer_closed(&near);
        assert!(
            accept_closed_arm(&near, Err(std::io::Error::from_raw_os_error(libc::EPERM))).is_err()
        );
        accept_closed_arm(&near, Err(std::io::Error::from_raw_os_error(errno))).unwrap();
        let mut reply = Vec::new();
        near.read_to_end(&mut reply).unwrap();
        assert_eq!(
            reply, b"OK partial",
            "timeout handling must not discard bytes"
        );
    }
}

/// Reads `lines` request lines from `far`, then answers `OK` a moment later: long enough that
/// a nonblocking read which did not wait says `WouldBlock` first.
#[cfg(unix)]
fn answers_late(far: aterm_uds::CtlStream, lines: usize) -> std::thread::JoinHandle<()> {
    use std::io::BufRead as _;
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(&far);
        for _ in 0..lines {
            reader.read_line(&mut String::new()).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        (&far).write_all(b"OK\n").unwrap();
    })
}

/// A read of a nonblocking stream says `WouldBlock` at once, so a reply that arrives here was
/// waited for in `poll(2)`, the one wait the connection's end cannot pass by
/// ([`aterm_uds::ready::for_read`]).
#[cfg(unix)]
#[test]
fn a_bounded_connection_waits_for_its_reply_in_poll() {
    let (near, far) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    near.set_nonblocking(true).unwrap();
    let answering = answers_late(far, 1);
    let mut client = RelayClient::new(near).bounded(aterm_spec::wait::MUST_HAPPEN);
    assert_eq!(client.request_line("version").unwrap(), "OK");
    answering.join().unwrap();
}

/// [`a_bounded_connection_waits_for_its_reply_in_poll`], on a connection with no bound.
#[cfg(unix)]
#[test]
fn an_unbounded_connection_waits_for_its_reply_in_poll() {
    let dir = crate::supervise::ScratchDir::new("relay", "unbounded");
    let sock = dir.join("c.sock");
    let listener = aterm_uds::kept_out(|| aterm_uds::CtlListener::bind(&sock)).unwrap();
    let accepting = std::thread::spawn(move || {
        let (far, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        answers_late(far, 2).join().unwrap();
    });
    let mut client = RelayClient::connect_local(sock.to_str().unwrap(), "token").unwrap();
    client.transport().set_nonblocking(true).unwrap();
    assert_eq!(client.request_line("version").unwrap(), "OK");
    accepting.join().unwrap();
}

fn armed(_: &RecordingTransport, wait: std::time::Duration) -> std::io::Result<()> {
    ARMED.with(|a| a.borrow_mut().push(wait));
    Ok(())
}

/// A bounded connection arms each exchange with the bound and, on top, the wait the request
/// names: the first `timeout=` (or `timeout <ms>`) it carries, a typed text's words after it
/// being text.
#[test]
fn a_bounded_exchange_waits_the_bound_and_its_own_timeout() {
    let bound = std::time::Duration::from_millis(100);
    let mut client = RelayClient::new(RecordingTransport::new(&b"OK\n".repeat(4)));
    client.bound = Some((armed, bound));
    ARMED.with(|arms| arms.borrow_mut().clear());
    let ms = std::time::Duration::from_millis;
    for (line, wait) in [
        ("@s-1 status", bound),
        ("@s-1 await agent idle timeout=15000", bound + ms(15_000)),
        (
            "@s-1 turn yield=0.2 idle=1 timeout=10000 the timeout=1 of it",
            bound + ms(10_000),
        ),
        ("await seq 4 timeout 300", bound + ms(300)),
    ] {
        assert_eq!(exchange_wait(line, bound), wait);
        ARMED.with(|arms| arms.borrow_mut().clear());
        assert_eq!(client.request_line(line).expect(line), "OK");
        ARMED.with(|arms| {
            let arms = arms.borrow();
            assert_eq!(arms.first(), Some(&wait));
            assert_eq!(arms.last(), Some(&wait));
            assert!(arms.iter().all(|armed| !armed.is_zero() && *armed <= wait));
        });
    }
    assert_eq!(
        exchange_wait("await seq timeout=1", std::time::Duration::MAX),
        std::time::Duration::MAX,
        "a wait past the clock's range saturates"
    );
}

/// A bounded connection whose peer never answers ends the exchange at its bound and hangs
/// up, the client still held, so the peer reads its caller gone; one whose peer answers
/// reads the answer.
#[cfg(unix)]
#[test]
fn a_bounded_exchange_ends_at_its_bound() {
    use std::io::BufRead as _;
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("pair");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut client = RelayClient::new(ours).bounded(std::time::Duration::from_millis(50));
        let cut = client.request_line("status");
        let _ = tx.send((cut.map_err(|e| unanswered(&e)), client));
    });
    let (cut, client) = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the exchange within the hang detector");
    assert_eq!(cut, Err(true), "unanswered");
    assert!(client.reply_unread());
    theirs
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .expect("the peer's hang detector");
    let mut peer = std::io::BufReader::new(&theirs);
    let mut asked = String::new();
    peer.read_line(&mut asked).expect("the request");
    assert_eq!(asked, "status\n");
    asked.clear();
    assert_eq!(
        peer.read_line(&mut asked).map_err(|e| e.kind()),
        Ok(0),
        "the caller hung up while its client lives"
    );
    drop(client);

    let (ours, mut theirs) = std::os::unix::net::UnixStream::pair().expect("pair");
    theirs.write_all(b"OK answered\n").expect("the answer");
    let mut client = RelayClient::new(ours).bounded(DEFAULT_REPLY_WAIT);
    assert_eq!(
        client.request_line("status").expect("answered"),
        "OK answered"
    );
}

#[test]
fn relay_client_rejects_line_terminators_at_the_framing_boundary() {
    let mut client = RelayClient::new(RecordingTransport::new(b""));
    let error = client
        .write_line("AUTH token\nsend injected")
        .expect_err("a request must occupy exactly one line");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(client.io.sent.is_empty());
}

#[test]
fn relay_client_bounds_a_line_even_when_the_last_read_contains_its_newline() {
    for (len, accepted) in [(MAX_LINE_BYTES, true), (MAX_LINE_BYTES + 1, false)] {
        let mut reply = vec![b'x'; len];
        reply.extend_from_slice(b"\nOK next\n");
        let mut client = RelayClient::new(RecordingTransport::new(&reply));
        let result = client.request_line("status");
        if accepted {
            assert_eq!(result.expect("the exact content bound").len(), len);
            assert_eq!(client.request_line("status").unwrap(), "OK next");
        } else {
            assert_eq!(
                result
                    .expect_err("the final newline cannot bypass the cap")
                    .kind(),
                std::io::ErrorKind::InvalidData
            );
        }
    }
}

#[test]
fn relay_client_keeps_fragmented_utf8_crlf_and_later_lines_separate() {
    let expected = format!("prefix {} end", "α🦀z".repeat(4097));
    let mut reply = expected.as_bytes().to_vec();
    reply.extend_from_slice(b"\r\n\xff\nOK next\r\n");
    for chunk in [1, 3, 8191, 8192] {
        let mut transport = RecordingTransport::new(&reply);
        transport.max_read = chunk;
        let mut client = RelayClient::new(transport);
        assert_eq!(client.next_pushed().unwrap(), expected, "chunk={chunk}");
        assert_eq!(
            client.next_pushed().unwrap_err().kind(),
            std::io::ErrorKind::InvalidData,
            "invalid UTF-8 is confined to its own line, chunk={chunk}"
        );
        assert_eq!(client.next_pushed().unwrap(), "OK next", "chunk={chunk}");
    }
}

#[test]
fn relay_client_resumes_a_fragmented_line_after_a_transport_error() {
    let mut transport = RecordingTransport::new("prefix α🦀\r\nOK next\n".as_bytes());
    transport.max_read = 3;
    transport.error_at = Some(3);
    let mut client = RelayClient::new(transport);
    assert_eq!(
        client.next_pushed().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(client.next_pushed().unwrap(), "prefix α🦀");
    assert_eq!(client.next_pushed().unwrap(), "OK next");
}

#[test]
fn relay_client_counted_body_has_one_aggregate_allowance() {
    let mut exact = RelayClient::new(RecordingTransport::new(b"a\r\nb\nOK next\n"));
    assert_eq!(
        exact
            .read_body_lines(2, 4)
            .expect("the normalized body fits"),
        "a\nb\n"
    );
    assert_eq!(exact.read_line().expect("the next reply"), "OK next");

    let mut over = RelayClient::new(RecordingTransport::new(b"a\r\nb\nOK next\n"));
    let error = over
        .read_body_lines(2, 3)
        .expect_err("short rows must share the body allowance");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("aggregate"));
    assert_eq!(over.read_line().expect("no extra row consumed"), "OK next");

    let mut empty = RelayClient::new(RecordingTransport::new(b"\n\n"));
    assert_eq!(empty.read_body_lines(2, 2).unwrap(), "\n\n");
    assert!(empty.read_body_lines(0, 0).unwrap().is_empty());
    assert_eq!(
        empty
            .read_body_lines(MAX_TEXT_LINES + 1, usize::MAX)
            .expect_err("the count is bounded before a read")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
}

#[test]
fn relay_client_refuses_overflowing_counts_instead_of_reading_them_as_status() {
    let mut client = RelayClient::new(RecordingTransport::new(
        b"OK 99999999999999999999999999999999999\nbody\n",
    ));
    assert_eq!(
        client
            .request_counted("text")
            .expect_err("a numeric count overflow")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
    let mut client = RelayClient::new(RecordingTransport::new(
        b"OK 99999999999999999999999999999999999\nbody\n",
    ));
    assert_eq!(
        client
            .request_bytes("cast")
            .expect_err("a numeric count overflow")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
    assert_eq!(reply_count("OK schema=1").unwrap(), None);
    assert_eq!(reply_count("OK 2 incomplete").unwrap(), Some(2));
    assert!(reply_count("OK +99999999999999999999999999999999999").is_err());
    assert_eq!(reply_count("OK +2 incomplete").unwrap(), Some(2));
    assert!(reply_count("OK +").is_err());
    assert_eq!(
        reply_count("OK 99999999999999999999999999999999999x").unwrap(),
        None,
        "an overflowing-looking status token is still not a numeric count"
    );
}

#[test]
fn relay_client_frames_a_payload_byte_for_byte() {
    let mut client = RelayClient::new(RecordingTransport::new(b"OK 9 bytes\n"));
    let reply = client
        .request_frame("feed-bin", b"id=x go\\n")
        .expect("a framed request");
    assert_eq!(reply, "OK 9 bytes");
    assert_eq!(client.io.sent, b"feed-bin 9\nid=x go\\n");

    let mut client = RelayClient::new(RecordingTransport::new(b""));
    let err = client
        .request_frame("feed-bin\nsend evil", b"x")
        .expect_err("a header occupies one line");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(client.io.sent.is_empty());
}

#[test]
fn relay_client_reads_the_host_gone_line_as_the_connection_closing() {
    let mut client = RelayClient::new(RecordingTransport::new(
        b"ERR gone pid=4242 sock=/run/aterm/aterm-4242.sock\n",
    ));
    let err = client
        .request_line("@s-abc text")
        .expect_err("not an answer");
    assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof, "{err}");
    assert!(err.to_string().contains("pid 4242"), "{err}");
    let mut client = RelayClient::new(RecordingTransport::new(b"ERR gone fishing\n"));
    assert_eq!(
        client.request_line("text").expect("an answer"),
        "ERR gone fishing"
    );
    let row = "ERR gone pid=4242 sock=/run/aterm/aterm-4242.sock";
    let mut client = RelayClient::new(RecordingTransport::new(format!("OK 1\n{row}\n").as_bytes()));
    assert_eq!(
        client.request_counted("text").expect("a counted reply"),
        ("OK 1".to_string(), format!("{row}\n"))
    );
}

#[test]
fn a_silent_ctl_failure_still_says_something() {
    let reply = |code, stdout: &str, stderr: &str| supervise::CtlReply {
        code,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    };
    let cases = [
        (
            reply(1, "", "aterm-ctl: ERR no such session\n"),
            &["ERR no such session"][..],
        ),
        (reply(124, "OK timeout\n", ""), &["124", "OK timeout"][..]),
        (reply(3, "", " \n"), &["3"][..]),
    ];
    for (failed, tokens) in &cases {
        let words = failure_words(failed);
        for token in *tokens {
            assert!(words.contains(token), "{words:?} lacks {token:?}");
        }
    }
}

#[test]
fn relay_help_memoizes_only_protocol_success() {
    let mut client = RelayClient::new(RecordingTransport::new(b"OKAY\nOK 1\nfresh\n"));
    assert_eq!(client.help_counted("turn").unwrap().0, "OKAY");
    let fresh = client.help_counted("turn").unwrap();
    assert_eq!(fresh, ("OK 1".to_string(), "fresh\n".to_string()));
    assert_eq!(client.help_counted("turn").unwrap(), fresh);
    assert_eq!(client.io.sent, b"help turn\nhelp turn\n");
}

#[test]
fn a_read_only_connection_refuses_every_mutating_subform() {
    let reads = [
        "@s-1 meta",
        "metrics",
        "metrics percentiles",
        "metrics json",
        "metrics --json",
        "metrics percentiles json",
        "metrics --json percentiles",
        "metrics json percentiles --json",
    ];
    let mutations = [
        "@s-1 meta set attention held",
        "@s-1 meta unset attention",
        "metrics reset",
        "@s-1 metrics reset",
        "metrics reset json",
        "metrics json reset",
        "metrics --json reset",
        "metrics reset --json",
        "metrics json reset --json",
    ];
    let replies = b"OK\n".repeat(reads.len() + mutations.len());
    let mut ro = RelayClient::new(RecordingTransport::new(&replies)).reads_only();
    for read in reads {
        assert!(ro.request_line(read).is_ok(), "{read}");
    }
    let before = ro.io.sent.clone();
    let refused: Vec<_> = mutations
        .iter()
        .map(|request| {
            ro.request_line(request)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
        })
        .collect();
    let mut plain = RelayClient::new(RecordingTransport::new(b"OK\nOK\n"));
    assert!(plain.request_line("metrics reset").is_ok());
    assert!(plain.request_line("meta unset attention").is_ok());
    assert_eq!(
        (refused, ro.io.sent == before),
        (vec![true; mutations.len()], true),
        "no mutating form writes a byte; ordinary connections retain it"
    );
}

#[test]
fn a_read_only_connection_sends_doc_reads_alone() {
    let reads = [
        "doc ls",
        "doc ls uri=file:///tmp/x.txt",
        "@s-1 doc read 3 lines=1-20",
        "doc read file:///tmp/x.txt",
        "doc log 3 since=2",
    ];
    let writes = [
        "doc apply 3 base=4 len=12",
        "@s-1 doc save 3",
        "doc",
        "doc frob 3",
    ];
    let mut ro =
        RelayClient::new(RecordingTransport::new(&b"OK 0\n".repeat(reads.len()))).reads_only();
    for read in reads {
        assert!(ro.request_line(read).is_ok(), "{read}");
    }
    let sent = ro.io.sent.clone();
    for write in writes {
        let refused = ro.request_line(write).expect_err(write);
        assert_eq!(
            refused.kind(),
            std::io::ErrorKind::PermissionDenied,
            "{write}"
        );
    }
    assert_eq!(ro.io.sent, sent, "no write form wrote a byte");
    let mut plain = RelayClient::new(RecordingTransport::new(b"OK\n"));
    assert!(plain.request_line("doc save 3").is_ok());
}

#[test]
fn a_read_only_connection_sends_the_read_verbs_alone() {
    let reads = [
        "@s-1 text --json",
        "status",
        "@s-1 await seq 7 timeout=0",
        "@s-1 meta",
    ];
    let hands = [
        "@s-1 send hi",
        "@s-1 key enter",
        "@s-1 turn hi",
        "@s-1 signal TERM pid=7",
        "@s-1 lease acquire",
        "@s-1 meta set attention x",
        "@s-1 frobnicate",
    ];
    let mut ro = RelayClient::new(RecordingTransport::new(b"OK\nOK\nOK\nOK\n")).reads_only();
    for r in reads {
        assert!(ro.request_line(r).is_ok(), "{r}");
    }
    let read_bytes = ro.io.sent.len();
    for h in hands {
        let err = ro.request_line(h).expect_err(h);
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{h}");
    }
    assert!(ro.request_frame("@s-1 feed-bin", b"x").is_err());
    assert_eq!(ro.io.sent.len(), read_bytes, "nothing written for a hand");

    let mut plain = RelayClient::new(RecordingTransport::new(b"OK\nOK\n"));
    assert!(plain.request_line("@s-1 send hi").is_ok());
    assert!(plain.request_line("@s-1 meta set attention x").is_ok());
}

/// [`CtlClient::own_at`] runs `<exe> ctl --sock <sock> <args>` in that
/// order, and [`own_exe`] names a file that exists (this test binary).
#[cfg(unix)]
#[test]
fn the_own_client_runs_the_ctl_verb_first() {
    use std::os::unix::fs::PermissionsExt as _;
    assert!(own_exe().expect("this binary").is_file());
    let dir = crate::supervise::ScratchDir::new("own-ctl", "exe");
    let exe = dir.join("aterm");
    std::fs::write(&exe, "#!/bin/sh\necho \"$*\"\n").expect("write the stand-in");
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let own = CtlClient::own_at(&exe, Some("/s.sock".to_string()));
    assert_eq!(
        own.run(&["cursor"]).expect("it runs"),
        "ctl --sock /s.sock cursor\n"
    );
    let plain = CtlClient::new(&exe, Some("/s.sock".to_string()));
    assert_eq!(
        plain.run(&["cursor"]).expect("it runs"),
        "--sock /s.sock cursor\n"
    );
}

#[test]
fn the_ctl_client_is_this_image_even_once_its_file_is_replaced() {
    let me = std::env::current_exe().expect("the test binary");
    let canon = std::fs::canonicalize(&me).expect("the test binary resolves");
    assert_eq!(resolved_exe(me.clone()), canon);
    assert_eq!(own_exe().expect("this image"), canon);
    let gone = me.with_file_name("aterm (deleted)");
    let expect = if cfg!(target_os = "linux") {
        std::path::PathBuf::from("/proc/self/exe")
    } else {
        gone.clone()
    };
    assert_eq!(resolved_exe(gone), expect);
}
