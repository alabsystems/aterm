// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE INPUT/EGRESS CONTRACT OF 2026-09-25, driven through the GUI's real
//! seams: the per-session ordered writer ([`super::paste_order`]) admits
//! against one bounded budget and refuses what it cannot hold, a paste or a
//! report never taking the keys' reserve; a key typed into a full input queue
//! is refused and said once, never parked and never falsely delivered; a
//! session's teardown unblocks every waiter;
//! terminal replies never land inside a paste's bracketed envelope; a key
//! that waited behind a paste arms the echo clock at its own write; closing a
//! session releases everything waiting on it; a mouse or focus report waits
//! behind what was queued before it; and the Windows arm (every key and every
//! report through the writer) runs here on unix.
//!
//! Every fixture wedges a REAL fd — a socketpair or pipe filled until the
//! kernel refuses more — so "the program stopped reading" is a kernel fact,
//! not a mock.

use super::paste_order;
use crate::input::{Delivery, Egress, InputEvent, InputOutcome, PasteFraming, Source};
use crate::{App, WindowId};
use aterm_session::sink::{BulkMeter, BulkState, SinkWriter};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Fill `fd` (already `O_NONBLOCK`) until the kernel takes no more.
fn fill(fd: i32) -> usize {
    let mut filled = 0usize;
    loop {
        match aterm_pty::write_some(fd, &[b'.'; 4096]) {
            Ok(n) if n > 0 => filled += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            other => panic!("unexpected fill result: {other:?}"),
        }
    }
    assert!(filled > 0, "the fd took bytes before it filled");
    filled
}

/// A sink over one end of a socketpair in the production master's shape
/// (`O_NONBLOCK`, declared), filled solid: a program that stopped reading.
fn wedged_socket() -> (Arc<SinkWriter>, UnixStream, usize) {
    let (reader, writer) = UnixStream::pair().expect("socketpair");
    reader
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    writer.set_nonblocking(true).expect("nonblocking master");
    let filled = fill(writer.as_raw_fd());
    let owned: std::os::fd::OwnedFd = writer.into();
    let sink = Arc::new(SinkWriter::new_owned(owned));
    sink.note_master_nonblocking(true);
    (sink, reader, filled)
}

/// Poll `done` until it holds or `limit` passes.
fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    done()
}

/// Enqueue `ev` for `sink` on the ordered writer, borrowing the terminal,
/// mode mirror and echo tracker of `app`'s front session (the writer takes
/// them as handles; the bytes go to `sink`).
fn enqueue(
    app: &App,
    sink: &Arc<SinkWriter>,
    ev: InputEvent,
    hot_key: Option<u64>,
    meter: Option<Arc<BulkMeter>>,
) -> Result<(), paste_order::Rejected> {
    let mirror = app.front_terminal_mirror(WindowId(0)).expect("terminal");
    let session = app.pool.get(mirror.session).expect("session");
    paste_order::enqueue_metered(
        &mirror.term,
        &session.ctx.modes,
        sink,
        &session.ctx.output_echo,
        ev,
        None,
        hot_key.map(|id| (id, None)),
        meter,
    )
}

/// A left press at cell (row 2, col 3), in the shape the App's mouse arm
/// dispatches — `ESC[<0;4;3M` under SGR tracking.
fn left_press() -> InputEvent {
    InputEvent::MouseButton {
        button: aterm_types::mouse::MouseButton::Left,
        pressed: true,
        row: 2,
        col: 3,
        mods: 0,
        click_count: 1,
        side: aterm_core::selection::SelectionSide::Left,
        block: false,
        suppress_copy_on_select: false,
        px_off: crate::input::PixelOffset::CELL_ORIGIN,
    }
}

/// Read the `O_NONBLOCK` read end `fd` until `want` bytes arrived, or ten
/// seconds passed (then whatever did).
fn read_until(fd: i32, want: usize) -> Vec<u8> {
    let mut got = Vec::with_capacity(want);
    wait_until(Duration::from_secs(10), || {
        let mut buf = [0u8; 65536];
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            got.extend_from_slice(&buf[..n as usize]);
        }
        got.len() >= want
    });
    got
}

fn close_pipe(pipe: [i32; 2]) {
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}

/// STALL THE PROGRAM, FILL THE SPILL, TYPE A KEY (docs/AUDIT-typing-to-pixels
/// P0, decided 2026-09-25). The key is REFUSED: dispatch returns at once (no
/// park on the event loop), the outcome is a failure (no false success), the
/// refusal is counted and said ONCE — one "Couldn't send your input" row for the
/// session however many keys are refused, never a bell per key — and once the
/// program reads, every byte accepted before the key arrives in order with the
/// refused keys nowhere among it — and a key typed then goes through. RED
/// before: the key parked the UI thread in `write_frame_after_reserve` until
/// the program read.
///
/// A watchdog reads the pipe out after 20 s, so a regression FAILS the
/// elapsed-time assertion instead of hanging the suite.
#[test]
fn a_key_into_a_full_input_queue_is_refused_and_said_once_never_parked() {
    let (mut app, sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
    aterm_pty::set_nonblocking(pipe[1], true).expect("nonblocking master");
    sink.note_master_nonblocking(true);
    let filled = fill(pipe[1]);
    // Earlier input filled the spill to its cap (the program has not read
    // any of it).
    let mut accepted: Vec<Vec<u8>> = Vec::new();
    for i in 0usize.. {
        // Upper case: nothing accepted may look like the refused `k`.
        let frame = vec![b'A' + (i % 26) as u8; 4097];
        match sink.write_frame_interactive_with_receipt(&frame) {
            Ok(receipt) => {
                assert_eq!(receipt.accepted(), frame.len());
                accepted.push(frame);
            }
            Err(error) => {
                assert!(error.is_refused(), "only the cap refuses: {error}");
                break;
            }
        }
        assert!(i < 1024, "the cap never refused");
    }

    let done = Arc::new(AtomicBool::new(false));
    let watchdog = {
        let done = Arc::clone(&done);
        let reader = pipe[0];
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !done.load(Ordering::Acquire) {
                if Instant::now() >= deadline {
                    // The dispatch parked: unwedge it so the test can fail.
                    let mut buf = [0u8; 65536];
                    while unsafe { libc::read(reader, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };
    let refused_before = crate::metrics::input_refused();
    let session = app
        .front_terminal_mirror(WindowId(0))
        .expect("terminal")
        .session;
    let said = |app: &App| {
        let key = format!("session.input-full.{session}");
        app.messages
            .live_rows()
            .filter(|m| m.msg.key.as_deref() == Some(key.as_str()))
            .count()
    };
    assert_eq!(said(&app), 0, "negative control: nothing said yet");
    let t0 = Instant::now();
    let outcome = app.input(WindowId(0), InputEvent::Text("k".into()), Source::Human);
    let second = app.input(WindowId(0), InputEvent::Text("k".into()), Source::Human);
    let took = t0.elapsed();
    done.store(true, Ordering::Release);
    watchdog.join().expect("watchdog");
    assert!(
        took < Duration::from_secs(2),
        "the key parked the event loop for {took:?}"
    );
    assert_eq!(
        outcome,
        InputOutcome::WriteFailed,
        "a refused key is never reported delivered"
    );
    assert_eq!(second, InputOutcome::WriteFailed);
    assert!(
        crate::metrics::input_refused() >= refused_before + 2,
        "each refusal is counted"
    );
    assert_eq!(said(&app), 1, "and said once for the session");

    // The program reads: the fill, then every accepted frame in order; no `k`.
    let expect = filled + accepted.iter().map(Vec::len).sum::<usize>();
    let mut got = Vec::with_capacity(expect);
    assert!(
        wait_until(Duration::from_secs(10), || {
            let mut buf = [0u8; 65536];
            let n = unsafe { libc::read(pipe[0], buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 {
                got.extend_from_slice(&buf[..n as usize]);
            }
            got.len() >= expect
        }),
        "everything accepted reaches the program"
    );
    assert_eq!(got.len(), expect, "and nothing else");
    let mut at = filled;
    for frame in &accepted {
        assert_eq!(&got[at..at + frame.len()], frame.as_slice(), "in order");
        at += frame.len();
    }
    assert!(!got.contains(&b'k'), "the refused key was never delivered");

    // The spill has drained: the retyped key goes through.
    assert!(wait_until(Duration::from_secs(5), || sink.egress_drained_to_kernel()));
    let outcome = app.input(WindowId(0), InputEvent::Text("z".into()), Source::Human);
    assert_eq!(outcome, InputOutcome::Ok);
    let mut z = Vec::new();
    assert!(wait_until(Duration::from_secs(5), || {
        let mut buf = [0u8; 16];
        let n = unsafe { libc::read(pipe[0], buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            z.extend_from_slice(&buf[..n as usize]);
        }
        !z.is_empty()
    }));
    assert_eq!(z, b"z");
    drop(app);
    drop(sink);
    close_pipe(pipe);
}

/// REPEATED MAXIMUM PASTES PLATEAU, AND TEARDOWN UNBLOCKS EVERY WAITER. Into
/// a program that stopped reading, the first maximum paste is written until
/// it parks, and every maximum paste after it is REFUSED at submission while
/// it holds its room — the admission never holds more than a paste may use,
/// however many are pasted. RED before: the FIFO was an unbounded channel and
/// each queued paste kept its 16 MiB alive in it.
///
/// Then the session closes (the sink's sever, `Session::drop`): the parked
/// writer abandons its frame, its meter settles, its slot retires and every
/// admitted byte comes back. Negative control: before the sever the paste is
/// still parked.
#[test]
fn repeated_maximum_pastes_plateau_and_teardown_unblocks_every_waiter() {
    let (app, _app_sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
    let (sink, _reader, _filled) = wedged_socket();
    let paste = || {
        InputEvent::Paste(
            "p".repeat(16 * 1024 * 1024),
            PasteFraming::Gesture { bracketed: true },
        )
    };
    let (_, _, paste_bytes, _) = paste_order::limits_for_test();
    let meters: Vec<Arc<BulkMeter>> = vec![Arc::new(BulkMeter::new())];
    enqueue(&app, &sink, paste(), None, Some(meters[0].clone())).expect("the first paste");
    let mut refused = 0usize;
    for _ in 0..6 {
        match enqueue(&app, &sink, paste(), None, None) {
            Err(paste_order::Rejected::Full) => refused += 1,
            other => panic!("a second maximum paste must be refused: {other:?}"),
        }
        assert!(
            paste_order::admitted_for_test(&sink).1 <= u64::from(paste_bytes),
            "the admission stays within what a paste may use"
        );
    }
    assert_eq!(refused, 6);
    assert!(
        wait_until(Duration::from_secs(5), || meters[0].progress().state
            == BulkState::Writing),
        "the first paste is being written"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        sink.ordered_egress_count(),
        1,
        "before the sever the paste still waits"
    );
    assert_eq!(meters[0].progress().state, BulkState::Writing);

    sink.sever_input();
    assert!(
        wait_until(Duration::from_secs(5), || sink.ordered_egress_count() == 0),
        "the sever unblocked the writer"
    );
    for meter in &meters {
        let state = meter.progress().state;
        assert!(
            matches!(state, BulkState::Stopped | BulkState::Failed),
            "a torn-down paste settles, never stays Writing/Queued: {state:?}"
        );
    }
    assert_eq!(
        paste_order::admitted_for_test(&sink),
        (0, 0),
        "every admitted byte came back"
    );
    drop(app);
    close_pipe(pipe);
}

/// TERMINAL REPLIES STAY WHOLLY OUTSIDE A PASTE'S ENVELOPE, before and
/// during the paste (contract 1: one envelope; a reply waits behind the
/// paste, never inside it). A reply answered before the paste reaches the
/// program first; one answered while the paste is parked on a slow reader,
/// and a key typed then, both arrive after the closing bracket; the envelope
/// is one contiguous run. The reply goes through the real reply writer, the
/// paste and the key through the real ordered writer.
#[test]
fn replies_stay_outside_the_paste_envelope_before_and_during_it() {
    let (app, _app_sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
    let (reader, writer) = UnixStream::pair().expect("socketpair");
    reader
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    writer.set_nonblocking(true).expect("nonblocking master");
    let owned: std::os::fd::OwnedFd = writer.into();
    let sink = Arc::new(SinkWriter::new_owned(owned));
    sink.note_master_nonblocking(true);
    let (replies, reply_writer) = crate::spawn::spawn_reply_writer_for_test(sink.clone());

    assert!(replies.submit(Arc::from(&b"\x1b[?62c"[..])), "reply before");
    let mut first = [0u8; 6];
    let mut reader = reader;
    reader.read_exact(&mut first).expect("the early reply");
    assert_eq!(&first, b"\x1b[?62c");

    let body = vec![b'p'; 1 << 20];
    enqueue(
        &app,
        &sink,
        InputEvent::Paste(
            String::from_utf8(body.clone()).expect("ascii"),
            PasteFraming::Gesture { bracketed: true },
        ),
        None,
        None,
    )
    .expect("the paste");
    // The program reads the paste's head, then answers nothing for a while:
    // the paste parks on the full socket with the envelope open.
    let mut head = vec![0u8; 64 * 1024];
    reader.read_exact(&mut head).expect("the paste's head");
    assert!(head.starts_with(b"\x1b[200~"));
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        replies.submit(Arc::from(&b"\x1b[12;40R"[..])),
        "reply during"
    );
    enqueue(&app, &sink, InputEvent::Text("K".into()), None, None).expect("a key during");

    let mut rest = Vec::new();
    let mut chunk = [0u8; 65536];
    while !(rest.ends_with(b"K") && rest.windows(8).any(|w| w == b"\x1b[12;40R")) {
        let n = reader.read(&mut chunk).expect("the rest");
        assert!(n > 0, "peer open");
        rest.extend_from_slice(&chunk[..n]);
    }
    let mut stream = head;
    stream.extend_from_slice(&rest);
    let close = stream
        .windows(6)
        .position(|w| w == b"\x1b[201~")
        .expect("the envelope closes");
    let envelope = &stream[6..close];
    assert_eq!(
        envelope,
        body.as_slice(),
        "the envelope is one contiguous run"
    );
    let after = &stream[close + 6..];
    assert!(
        after == b"\x1b[12;40RK" || after == b"K\x1b[12;40R",
        "the reply and the key land after the envelope: {:?}",
        String::from_utf8_lossy(after)
    );
    drop(replies);
    drop(sink);
    assert!(
        wait_until(Duration::from_secs(5), || reply_writer.is_finished()),
        "the reply writer ends with its lane and sink"
    );
    drop(app);
    close_pipe(pipe);
}

/// THE WINDOWS ARM, ON UNIX: ConPTY has no non-blocking write, so there every
/// key goes through the per-session ordered writer and the UI thread only
/// enqueues. Routed that way here, a key with no paste in flight is still
/// deferred to the writer (it wrote nothing inline) and reaches the program;
/// and once the writer admits nothing more, the next key is refused
/// (`queue_full`) — not queued, not written, and never written around the
/// FIFO.
#[test]
fn the_conpty_route_sends_every_key_through_the_ordered_writer() {
    let (app, sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
    let mirror = app.front_terminal_mirror(WindowId(0)).expect("terminal");
    let session = app.pool.get(mirror.session).expect("session");
    let route = |ev: &InputEvent| {
        paste_order::ordered_or_inline_routed(
            true,
            &mirror.term,
            &session.ctx.modes,
            &sink,
            &session.ctx.output_echo,
            ev,
            None,
            None,
        )
    };
    let (receipt, inline) = route(&InputEvent::Text("w".into()));
    assert!(
        !inline,
        "the key was handed to the writer, not written here"
    );
    assert!(!receipt.input_queue_full());
    let mut got = Vec::new();
    assert!(wait_until(Duration::from_secs(5), || {
        let mut buf = [0u8; 8];
        let n = unsafe { libc::read(pipe[0], buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            got.extend_from_slice(&buf[..n as usize]);
        }
        !got.is_empty()
    }));
    assert_eq!(got, b"w");

    let hold = paste_order::reject_admission_for_test(&sink, false);
    let (receipt, inline) = route(&InputEvent::Text("x".into()));
    assert!(!inline);
    assert!(receipt.input_queue_full(), "a full queue refuses the key");
    assert_eq!(
        crate::app_input::egress_to_outcome(receipt.egress),
        InputOutcome::WriteFailed
    );
    drop(hold);
    std::thread::sleep(Duration::from_millis(50));
    let mut buf = [0u8; 8];
    assert!(
        unsafe { libc::read(pipe[0], buf.as_mut_ptr().cast(), buf.len()) } <= 0,
        "the refused key was never written"
    );
    drop(mirror);
    drop(app);
    drop(sink);
    close_pipe(pipe);
}

/// A KEY THAT WAITED BEHIND A PASTE ARMS THE ECHO CLOCK AT ITS OWN WRITE
/// (typing audit P1). The key is dispatched while the session's FIFO is
/// occupied, so the UI thread's bracket closes before the key is written;
/// the writer arms the clock once the key's receipt carries an accepted
/// order. The bracket, closed only after that write moved the sink's epoch,
/// arms nothing: exactly one arm, no coalesced double. RED before: the
/// writer never armed (`arms == 0` if the bracket closed first), and a
/// bracket that closed after the write armed for bytes it did not write.
#[test]
fn a_key_deferred_behind_a_paste_arms_the_echo_clock_at_its_write() {
    let _echo = crate::echo_rtt::fresh_for_test();
    let (mut app, sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
    let _arms = crate::echo_rtt::deferred_arms_for_test(&sink);
    let wid = WindowId(0);
    let pin = paste_order::pin_ordering_for_test(&sink);
    let probe = app.echo_probe_open(wid, None);
    let key = InputEvent::Key {
        key: aterm_types::keyboard::Key::Character('q'),
        mods: aterm_types::keyboard::Modifiers::empty(),
        base_layout: None,
        event_type: aterm_types::keyboard::KeyEventType::Press,
    };
    assert_eq!(app.input(wid, key, Source::Human), InputOutcome::Ok);
    // The writer takes the key and writes it (the pin holds only its slot).
    let mut got = Vec::new();
    assert!(wait_until(Duration::from_secs(5), || {
        let mut buf = [0u8; 8];
        let n = unsafe { libc::read(pipe[0], buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            got.extend_from_slice(&buf[..n as usize]);
        }
        !got.is_empty()
    }));
    assert_eq!(got, b"q");
    assert!(wait_until(Duration::from_secs(5), || sink
        .ordered_egress_count()
        == 1));
    app.echo_probe_close(probe);
    let echo = crate::echo_rtt::snapshot();
    assert_eq!(echo.arms, 1, "the writer armed the deferred key: {echo:?}");
    assert_eq!(
        echo.coalesced, 0,
        "the bracket did not arm for a write it did not make: {echo:?}"
    );
    drop(pin);
    drop(app);
    drop(sink);
    close_pipe(pipe);
}

/// CLOSING THE TAB IS THE TEARDOWN: `Session::drop` severs the session's
/// input, so a paste the ordered writer has parked on a program that stopped
/// reading — and a key queued behind it — are released the moment the
/// session goes, not when (if ever) the program reads again. RED before: the
/// parked writer held its sink clone (and with it the master and the queued
/// jobs) for as long as the program stayed frozen; a child that outlived its
/// hang-up in another process group kept it forever.
#[test]
fn closing_the_session_releases_its_parked_writer_and_queued_input() {
    let (mut app, sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
    aterm_pty::set_nonblocking(pipe[1], true).expect("nonblocking master");
    sink.note_master_nonblocking(true);
    fill(pipe[1]);
    let wid = WindowId(0);
    let session = app.front_terminal_mirror(wid).expect("terminal").session;
    // A paste too large to spill whole in one frame: parked on the direct lane.
    let outcome = app.input(
        wid,
        InputEvent::Paste(
            "p".repeat(256 * 1024),
            PasteFraming::Gesture { bracketed: true },
        ),
        Source::Human,
    );
    assert_eq!(outcome, InputOutcome::Ok);
    assert_eq!(
        app.input(wid, InputEvent::Text("k".into()), Source::Human),
        InputOutcome::Ok,
        "the key queues behind the paste"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        sink.ordered_egress_count(),
        2,
        "negative control: while the session lives, both wait on the program"
    );
    assert!(!sink.is_severed());

    // Close the session: the last view detaches and the Session drops.
    while app.pool.get(session).is_some() {
        let _ = app.pool.detach(session);
    }
    assert!(sink.is_severed(), "Session::drop severed its input");
    assert!(
        wait_until(Duration::from_secs(5), || sink.ordered_egress_count() == 0),
        "the parked paste and the queued key were released"
    );
    assert_eq!(paste_order::admitted_for_test(&sink), (0, 0));
    drop(app);
    drop(sink);
    close_pipe(pipe);
}

/// A REPLY FLOOD PLATEAUS TOO. A program that floods queries it never reads
/// the answers to (large colour or clipboard replies included) cannot grow
/// the reply lane past its byte budget: once the writer is parked on the
/// wedged tty and the budget is held, every further reply is dropped at
/// submission (and counted), never queued. RED before: the lane was bounded
/// only by count — 1024 replies of any size.
#[test]
fn a_reply_flood_into_a_stalled_program_plateaus_at_the_reply_budget() {
    let (sink, _reader, _filled) = wedged_socket();
    let (replies, _writer) = crate::spawn::spawn_reply_writer_for_test(sink.clone());
    let reply: Arc<[u8]> = Arc::from(vec![b'r'; 64 * 1024]);
    let budget = aterm_session::sink::REPLY_BUDGET_BYTES;
    let mut queued = 0usize;
    let mut dropped = 0usize;
    let counted_before = crate::metrics::reply_dropped();
    for _ in 0..256 {
        if replies.submit(Arc::clone(&reply)) {
            queued += 1;
        } else {
            dropped += 1;
        }
        assert!(
            sink.reply_reserved() <= budget,
            "the reply lane stays within its budget"
        );
    }
    assert!(
        dropped > 0,
        "the flood was refused once the budget was held"
    );
    assert!(queued * reply.len() <= budget + reply.len());
    assert!(
        crate::metrics::reply_dropped() >= counted_before + dropped as u64,
        "every dropped reply is counted (`reply_dropped` under `metrics percentiles`)"
    );
    // Teardown releases what the parked writer and the queue still hold.
    sink.sever_input();
    assert!(
        wait_until(Duration::from_secs(5), || sink.reply_reserved() == 0),
        "every held reply byte came back"
    );
    drop(replies);
}

/// THE WINDOWS ARM FOR REPORTS, ON UNIX (2026-09-26): a mouse, wheel or focus
/// report written inline could park the UI thread on ConPTY as surely as a
/// key, so there it takes the keys' route. Routed that way here: with
/// tracking off the seam decides locally and nothing is queued or written;
/// with tracking on, a press and a focus report are ENCODED on the calling
/// thread (the bytes an inline write would have sent) and handed to the
/// ordered writer, which writes them in order; and once the writer admits
/// nothing more, the next report is refused (`queue_full`), never written.
#[test]
fn the_conpty_route_sends_every_report_through_the_ordered_writer() {
    let (app, sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
    let mirror = app.front_terminal_mirror(WindowId(0)).expect("terminal");
    let session = app.pool.get(mirror.session).expect("session");
    let route = |ev: &InputEvent| {
        paste_order::report_ordered_or_inline_routed(
            true,
            &mirror.term,
            &session.ctx.modes,
            &sink,
            ev,
        )
    };

    let receipt = route(&left_press());
    assert!(
        matches!(receipt.egress, Egress::TrackingOff { .. }),
        "tracking off: the local gesture runs"
    );
    assert!(!receipt.is_queued(), "and nothing was queued");
    assert_eq!(sink.ordered_egress_count(), 0);
    assert_eq!(paste_order::admitted_for_test(&sink), (0, 0));

    crate::term_lock(&mirror.term).process(b"\x1b[?1000h\x1b[?1006h\x1b[?1004h");
    let (_, inline_bytes) =
        crate::input::seam_encode(&mirror.term, &session.ctx.modes, &sink, &left_press());
    assert_eq!(
        inline_bytes, b"\x1b[<0;4;3M",
        "the SGR press an inline write sends"
    );
    for ev in [left_press(), InputEvent::Focus(true)] {
        let receipt = route(&ev);
        assert!(receipt.is_queued(), "handed to the writer: {ev:?}");
        assert_eq!(receipt.accepted_order(), None, "and not written here");
        assert_eq!(receipt.egress, Egress::Reported(Delivery::Full));
    }
    let got = read_until(pipe[0], inline_bytes.len() + 3);
    assert_eq!(
        got,
        [inline_bytes.as_slice(), b"\x1b[I"].concat(),
        "in order"
    );

    let hold = paste_order::reject_admission_for_test(&sink, false);
    let receipt = route(&left_press());
    assert!(
        receipt.input_queue_full(),
        "a full queue refuses the report"
    );
    assert!(!receipt.is_queued());
    assert_eq!(
        crate::app_input::egress_to_outcome(receipt.egress),
        InputOutcome::WriteFailed
    );
    drop(hold);
    std::thread::sleep(Duration::from_millis(50));
    let mut buf = [0u8; 16];
    assert!(
        unsafe { libc::read(pipe[0], buf.as_mut_ptr().cast(), buf.len()) } <= 0,
        "the refused report was never written"
    );
    drop(mirror);
    drop(app);
    drop(sink);
    close_pipe(pipe);
}

/// A REPORT WAITS BEHIND WHAT WAS QUEUED BEFORE IT (2026-09-26), as a key
/// does. Paste A is parked on a program that stopped reading and paste B
/// waits behind it on the ordered writer; a click made now reaches the
/// program after B, not inside the run of pastes it followed. RED before: the
/// click was written inline, lost the fd lock to the parked paste and went to
/// the spill, which drains ahead of the writer's next job — the program read
/// A, the click, then B.
#[test]
fn a_mouse_report_waits_behind_the_pastes_queued_before_it() {
    let (mut app, sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
    aterm_pty::set_nonblocking(pipe[1], true).expect("nonblocking master");
    sink.note_master_nonblocking(true);
    let filled = fill(pipe[1]);
    let wid = WindowId(0);
    {
        let mirror = app.front_terminal_mirror(wid).expect("terminal");
        crate::term_lock(&mirror.term).process(b"\x1b[?1000h\x1b[?1006h");
    }
    let a = "a".repeat(256 * 1024);
    let b = "b".repeat(4096);
    let unbracketed = PasteFraming::Gesture { bracketed: false };
    let meter = Arc::new(BulkMeter::new());
    enqueue(
        &app,
        &sink,
        InputEvent::Paste(a.clone(), unbracketed),
        None,
        Some(meter.clone()),
    )
    .expect("paste A");
    enqueue(
        &app,
        &sink,
        InputEvent::Paste(b.clone(), unbracketed),
        None,
        None,
    )
    .expect("paste B");
    assert!(
        wait_until(Duration::from_secs(5), || meter.progress().state
            == BulkState::Writing),
        "paste A is being written"
    );
    // A parks on the full pipe, holding the fd lock; B waits in the FIFO.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(sink.ordered_egress_count(), 2, "A and B both wait");

    assert_eq!(
        app.input(wid, left_press(), Source::Human),
        InputOutcome::Ok
    );

    let report = b"\x1b[<0;4;3M";
    let want = filled + a.len() + b.len() + report.len();
    let got = read_until(pipe[0], want);
    assert_eq!(got.len(), want, "everything reaches the program, once");
    let tail = &got[filled..];
    assert_eq!(&tail[..a.len()], a.as_bytes(), "paste A first, whole");
    assert_eq!(
        &tail[a.len()..a.len() + b.len()],
        b.as_bytes(),
        "then paste B: the click did not overtake it"
    );
    assert_eq!(&tail[a.len() + b.len()..], report, "the click last");
    drop(app);
    drop(sink);
    close_pipe(pipe);
}

/// Set `app` up so its next [`App::recompute_focus_reports`] sends `ESC[I` to
/// the front session: the window front and focused, DEC 1004 on, and the
/// account saying nobody holds the keyboard yet (the delta a tab switch or a
/// pane focus makes).
fn arm_focus_in(app: &mut App) {
    let wid = WindowId(0);
    app.frontmost_window = Some(wid);
    app.windows.get_mut(&wid).expect("window").focused = true;
    let mirror = app.front_terminal_mirror(wid).expect("terminal");
    crate::term_lock(&mirror.term).process(b"\x1b[?1004h");
    app.focus_reported = None;
}

/// A TAB SWITCH'S FOCUS REPORT WAITS BEHIND THE PASTES QUEUED BEFORE IT
/// (2026-09-26 review), as a click does. The focus delta every tab switch,
/// pane focus, split, close, detach and migrate converges on
/// (`recompute_focus_reports`) is sent while paste A is parked on a program
/// that stopped reading and paste B waits behind it: the program reads A, B,
/// then `ESC[I`. RED before: that path still wrote inline — the report lost
/// the fd lock to the parked paste, went to the spill, and drained ahead of
/// B, so the program read A, the report, then B.
#[test]
fn a_tab_switch_focus_report_waits_behind_the_pastes_queued_before_it() {
    let (mut app, sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
    aterm_pty::set_nonblocking(pipe[1], true).expect("nonblocking master");
    sink.note_master_nonblocking(true);
    let filled = fill(pipe[1]);
    arm_focus_in(&mut app);
    let a = "a".repeat(256 * 1024);
    let b = "b".repeat(4096);
    let unbracketed = PasteFraming::Gesture { bracketed: false };
    let meter = Arc::new(BulkMeter::new());
    enqueue(
        &app,
        &sink,
        InputEvent::Paste(a.clone(), unbracketed),
        None,
        Some(meter.clone()),
    )
    .expect("paste A");
    enqueue(
        &app,
        &sink,
        InputEvent::Paste(b.clone(), unbracketed),
        None,
        None,
    )
    .expect("paste B");
    assert!(
        wait_until(Duration::from_secs(5), || meter.progress().state
            == BulkState::Writing),
        "paste A is being written"
    );
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(sink.ordered_egress_count(), 2, "A and B both wait");

    app.recompute_focus_reports();

    let report = b"\x1b[I";
    let want = filled + a.len() + b.len() + report.len();
    let got = read_until(pipe[0], want);
    assert_eq!(got.len(), want, "everything reaches the program, once");
    let tail = &got[filled..];
    assert_eq!(&tail[..a.len()], a.as_bytes(), "paste A first, whole");
    assert_eq!(
        &tail[a.len()..a.len() + b.len()],
        b.as_bytes(),
        "then paste B: the focus report did not overtake it"
    );
    assert_eq!(&tail[a.len() + b.len()..], report, "the report last");
    drop(app);
    drop(sink);
    close_pipe(pipe);
}

/// THE WINDOWS ARM OF A TAB SWITCH'S FOCUS REPORT, ON UNIX: on ConPTY the
/// focus delta goes through the ordered writer like every other report, so
/// the UI thread never parks on a frozen program's input pipe. Routed that
/// way (`force_conpty_route_for_test`) with the writer admitting nothing, the
/// report is REFUSED — which only a queued report can be — and nothing is
/// written. Negative control: on the unix route with nothing queued, the
/// same delta is written inline, whatever the writer would admit. RED
/// before: the path wrote inline on every platform, so the forced route
/// still wrote `ESC[I`.
#[test]
fn a_tab_switch_focus_report_takes_the_conpty_route() {
    let (mut app, sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
    arm_focus_in(&mut app);
    let hold = paste_order::reject_admission_for_test(&sink, false);
    {
        let _route = paste_order::force_conpty_route_for_test();
        app.recompute_focus_reports();
    }
    std::thread::sleep(Duration::from_millis(50));
    let mut buf = [0u8; 16];
    assert!(
        unsafe { libc::read(pipe[0], buf.as_mut_ptr().cast(), buf.len()) } <= 0,
        "the focus report was queued (and refused), never written inline"
    );
    assert_eq!(sink.ordered_egress_count(), 0, "nothing was queued");

    arm_focus_in(&mut app);
    app.recompute_focus_reports();
    assert_eq!(
        read_until(pipe[0], 3),
        b"\x1b[I",
        "the unix route writes it"
    );
    drop(hold);
    drop(app);
    drop(sink);
    close_pipe(pipe);
}

/// A MOTION FLOOD BEHIND A PASTE NEVER REFUSES A KEY (2026-09-26 review).
/// Under any-motion tracking (DEC 1003) a pointer moving over a program that
/// has stopped reading queues one report per motion event behind the parked
/// paste. They are admitted as a paste is, never into the keys' reserve: the
/// flood is refused once it has used the room a paste may use, and a key
/// typed then is still queued. RED before: reports were admitted as keys, so
/// the flood took the keys' room too and the key was refused.
#[test]
fn a_motion_flood_behind_a_paste_never_refuses_a_key() {
    let (app, sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
    aterm_pty::set_nonblocking(pipe[1], true).expect("nonblocking master");
    sink.note_master_nonblocking(true);
    fill(pipe[1]);
    let mirror = app.front_terminal_mirror(WindowId(0)).expect("terminal");
    let session = app.pool.get(mirror.session).expect("session");
    crate::term_lock(&mirror.term).process(b"\x1b[?1003h\x1b[?1006h");
    let meter = Arc::new(BulkMeter::new());
    enqueue(
        &app,
        &sink,
        InputEvent::Paste(
            "a".repeat(256 * 1024),
            PasteFraming::Gesture { bracketed: false },
        ),
        None,
        Some(meter.clone()),
    )
    .expect("the paste");
    assert!(
        wait_until(Duration::from_secs(5), || meter.progress().state
            == BulkState::Writing),
        "the paste is being written"
    );

    let mut queued = 0usize;
    let mut refused = false;
    for i in 0..200_000u32 {
        let motion = InputEvent::MouseMove {
            buttons: 3,
            row: (i % 20) as u16,
            col: (i % 80) as u16,
            mods: 0,
            side: aterm_core::selection::SelectionSide::Left,
            px_off: crate::input::PixelOffset::CELL_ORIGIN,
        };
        let receipt =
            paste_order::report_ordered_or_inline(&mirror.term, &session.ctx.modes, &sink, &motion);
        if receipt.input_queue_full() {
            refused = true;
            break;
        }
        assert!(receipt.is_queued(), "motion {i} waits behind the paste");
        queued += 1;
    }
    assert!(refused, "the motion flood is bounded");
    assert!(queued > 0);
    let (_, jobs, _, paste_jobs) = paste_order::limits_for_test();
    assert_eq!(
        paste_order::admitted_for_test(&sink).0,
        u64::from(paste_jobs),
        "the flood stopped at the room a paste may use"
    );
    assert!(paste_jobs < jobs, "the keys' reserve is whole");

    let (receipt, inline) = paste_order::ordered_or_inline(
        &mirror.term,
        &session.ctx.modes,
        &sink,
        &session.ctx.output_echo,
        &InputEvent::Text("k".into()),
        None,
        None,
    );
    assert!(!inline, "the key waits behind the paste");
    assert!(
        !receipt.input_queue_full(),
        "the key is queued, not refused"
    );
    assert!(receipt.is_queued());
    sink.sever_input();
    drop(mirror);
    drop(app);
    drop(sink);
    close_pipe(pipe);
}

/// AN OS APPEARANCE CHANGE NEVER PARKS THE UI THREAD (2026-09-26 review).
/// A program with DEC 2031 on has stopped reading, and earlier input filled
/// the spill to its cap; the OS flips to light mode. The colour-scheme
/// report takes the reports' route: `apply_os_color_scheme` returns at once,
/// the report (advisory) is refused rather than queued without bound, and
/// once the program reads, everything accepted before it arrives in order.
/// RED before: the report went through the BLOCKING `write_frame`, which
/// waited at the spill cap until the program read — the event loop, and
/// every window, parked for as long as the program stayed frozen.
///
/// A watchdog reads the pipe out after 20 s, so a regression FAILS the
/// elapsed-time assertion instead of hanging the suite.
#[test]
fn an_os_appearance_change_into_a_stalled_program_never_parks_the_ui_thread() {
    let (mut app, sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
    aterm_pty::set_nonblocking(pipe[1], true).expect("nonblocking master");
    sink.note_master_nonblocking(true);
    let filled = fill(pipe[1]);
    {
        let mirror = app.front_terminal_mirror(WindowId(0)).expect("terminal");
        crate::term_lock(&mirror.term).process(b"\x1b[?2031h");
    }
    let mut accepted: Vec<Vec<u8>> = Vec::new();
    for i in 0usize.. {
        let frame = vec![b'A' + (i % 26) as u8; 4097];
        match sink.write_frame_interactive_with_receipt(&frame) {
            Ok(receipt) => {
                assert_eq!(receipt.accepted(), frame.len());
                accepted.push(frame);
            }
            Err(error) => {
                assert!(error.is_refused(), "only the cap refuses: {error}");
                break;
            }
        }
        assert!(i < 1024, "the cap never refused");
    }

    let done = Arc::new(AtomicBool::new(false));
    let watchdog = {
        let done = Arc::clone(&done);
        let reader = pipe[0];
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !done.load(Ordering::Acquire) {
                if Instant::now() >= deadline {
                    let mut buf = [0u8; 65536];
                    while unsafe { libc::read(reader, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };
    let t0 = Instant::now();
    app.apply_os_color_scheme(WindowId(0), aterm_types::Appearance::Light);
    let took = t0.elapsed();
    done.store(true, Ordering::Release);
    watchdog.join().expect("watchdog");
    assert!(
        took < Duration::from_secs(2),
        "the appearance change parked the event loop for {took:?}"
    );

    let expect = filled + accepted.iter().map(Vec::len).sum::<usize>();
    let got = read_until(pipe[0], expect);
    assert_eq!(got.len(), expect, "everything accepted, and nothing else");
    let mut at = filled;
    for frame in &accepted {
        assert_eq!(&got[at..at + frame.len()], frame.as_slice(), "in order");
        at += frame.len();
    }
    drop(app);
    drop(sink);
    close_pipe(pipe);
}
