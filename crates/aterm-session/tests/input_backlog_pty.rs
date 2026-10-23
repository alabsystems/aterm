// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `SinkWriter::input_backlog` against a real pty: the one place the kernel's own count meets the ledger's dates.
#![cfg(target_os = "macos")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use aterm_session::input_backlog::InputBacklog;
use aterm_session::sink::{BulkMeter, Discard, ImmediateWrite, Seam, SinkWriter};
use aterm_spec::wait::MUST_HAPPEN;

/// A pty pair with the slave raw (`VMIN = 1`, `VTIME = 0`, a key-at-a-time
fn pty_pair(raw: bool) -> (i32, i32) {
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: `libc::termios` is plain integer fields and a byte array; all zeros is a valid value, overwritten by `tcgetattr` below.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `slave` is this test's live pty slave; `t` is a valid out-param.
    assert_eq!(unsafe { libc::tcgetattr(slave, &mut t) }, 0);
    if raw {
        // SAFETY: `t` is a live, initialised termios on this stack.
        unsafe { libc::cfmakeraw(&mut t) };
        t.c_cc[libc::VMIN] = 1;
        t.c_cc[libc::VTIME] = 0;
    } else {
        t.c_lflag |= libc::ICANON;
        t.c_lflag &= !libc::ECHO;
    }
    // SAFETY: `slave` is live; `t` is the termios just derived from its own.
    assert_eq!(unsafe { libc::tcsetattr(slave, libc::TCSANOW, &t) }, 0);
    (master, slave)
}

/// Read exactly `n` bytes from the slave — the program reading its input.
fn read_exactly(fd: i32, n: usize) -> Vec<u8> {
    let mut out = vec![0u8; n];
    let mut got = 0;
    while got < n {
        // SAFETY: a bounded read into the unfilled tail of `out`.
        let r = unsafe { libc::read(fd, out[got..].as_mut_ptr().cast(), n - got) };
        assert!(r > 0, "read({fd}) returned {r} with {got}/{n} bytes");
        got += r as usize;
    }
    out
}

fn close_pair(master: i32, slave: i32) {
    // SAFETY: both fds are this test's and still open; no sink outlives them.
    unsafe {
        libc::close(slave);
        libc::close(master);
    }
}

/// Make `master`'s file description non-blocking and tell the sink — the
fn nonblocking_sink(master: i32) -> SinkWriter {
    // SAFETY: `master` is this test's live fd; F_GETFL/F_SETFL take ints.
    let flags = unsafe { libc::fcntl(master, libc::F_GETFL) };
    assert!(flags >= 0);
    // SAFETY: as above.
    assert_eq!(
        unsafe { libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let sink = SinkWriter::new(master);
    sink.note_master_nonblocking(true);
    sink
}

/// Probe until `done` holds (the drainer runs on its own thread, so the
fn settle(sink: &SinkWriter, done: impl Fn(&InputBacklog) -> bool) -> InputBacklog {
    let mut last = None;
    let hit = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(10), || {
        let b = sink.input_backlog().expect("a pty master is measured");
        last = Some(b);
        done(&b).then_some(b)
    });
    hit.unwrap_or_else(|| panic!("never settled: {:?}", last.expect("probed")))
}

/// Wait until a parked paste's first bytes are queued and dated. The ledger stamps a
fn first_write_dated(sink: &SinkWriter) -> InputBacklog {
    settle(sink, |b| b.queued > 0 && !b.wait.is_zero())
}

/// RAW UNREAD INPUT IS COUNTED AND DATED — the incident's shape. `abc` goes in
#[test]
fn raw_unread_input_is_counted_and_dated() {
    let (master, slave) = pty_pair(true);
    let sink = SinkWriter::new(master);
    let empty = sink.input_backlog().expect("a pty master is measured");
    assert_eq!((empty.queued, empty.wait), (0, Duration::ZERO));
    assert!(!empty.canonical);
    assert_eq!(empty.spilled, Some(0));
    assert_eq!(empty.output_backlog, Some(0));

    let before_write = Instant::now();
    assert_eq!(sink.write_frame(b"abc").expect("write"), 3);
    let fresh = sink.input_backlog().expect("measured");
    assert_eq!(fresh.queued, 3);
    assert!(fresh.wait <= before_write.elapsed(), "{fresh:?}");

    assert_eq!(read_exactly(slave, 1), b"a");
    let partial = sink.input_backlog().expect("measured");
    assert_eq!(partial.queued, 2, "the program read one byte");

    assert_eq!(sink.write_frame(b"\x1b[B").expect("write"), 3);
    let behind = sink.input_backlog().expect("measured");
    assert_eq!(behind.queued, 5);

    assert_eq!(read_exactly(slave, 5), b"bc\x1b[B");
    let drained = sink.input_backlog().expect("measured");
    assert_eq!((drained.queued, drained.wait), (0, Duration::ZERO));
    drop(sink);
    close_pair(master, slave);
}

/// Canonical mode counts complete lines only: a partial `ab` is invisible to FIONREAD
#[test]
fn canonical_partial_line_reads_zero() {
    let (master, slave) = pty_pair(false);
    let sink = SinkWriter::new(master);
    assert_eq!(sink.write_frame(b"ab").expect("write"), 2);
    let b = sink.input_backlog().expect("measured");
    assert_eq!((b.queued, b.wait), (0, Duration::ZERO));
    assert!(b.canonical);
    drop(sink);
    close_pair(master, slave);
}

/// A WRITER PARKED ON A FULL QUEUE DOES NOT BLIND THE PROBE. A paste into a
#[test]
fn a_writer_parked_on_a_full_queue_does_not_blind_the_probe() {
    let (master, slave) = pty_pair(true);
    let sink = Arc::new(nonblocking_sink(master));

    const PASTE: usize = 64 * 1024;
    let paster = {
        let sink = Arc::clone(&sink);
        thread::spawn(move || sink.write_frame(&[b'p'; PASTE]).expect("paste"))
    };
    let _ = first_write_dated(&sink);
    assert!(
        !paster.is_finished(),
        "the paste must be parked on a full queue"
    );

    let parked = sink
        .input_backlog()
        .expect("measured while a writer is parked");
    assert!(parked.queued > 0, "{parked:?}");
    assert!(
        !parked.wait.is_zero(),
        "the parked paste's chunk is not in flight, so the queue keeps its date: {parked:?}"
    );

    assert_eq!(sink.write_frame_nonparking(b"z").expect("spill"), 1);
    let spilled = sink.input_backlog().expect("measured");
    assert!(!spilled.wait.is_zero(), "{spilled:?}");
    assert!(
        matches!(spilled.spilled, Some(1) | None),
        "the spilled key is reported (or its mutex was busy): {spilled:?}"
    );

    let got = read_exactly(slave, PASTE + 1);
    assert!(got[..PASTE].iter().all(|&b| b == b'p'));
    assert_eq!(got[PASTE], b'z', "the spilled key arrives after the paste");
    assert_eq!(paster.join().expect("paster"), PASTE);
    assert!(sink.wait_egress_drained_to_kernel());
    let drained = sink.input_backlog().expect("measured");
    assert_eq!(drained.queued, 0, "{drained:?}");
    drop(sink);
    close_pair(master, slave);
}

/// A drainer parked partway through a chunk counts each unread byte once. A
#[test]
fn a_drainer_parked_mid_chunk_counts_each_unread_byte_once() {
    const FRAME: usize = 4000;
    const READ: usize = 500;
    let (master, slave) = pty_pair(true);
    let sink = nonblocking_sink(master);

    let mut first_full = None;
    for episode in 0..2 {
        assert_eq!(
            sink.write_frame_nonparking(&[b'p'; FRAME]).expect("spill"),
            FRAME
        );
        let full = settle(&sink, |b| b.queued > 0 && b.spilled.is_some());
        assert_eq!(full.unread(), FRAME, "episode {episode}: {full:?}");
        assert!(
            full.spilled.is_some_and(|s| s > READ),
            "the tail spilled and the drainer is parked on it: {full:?}"
        );
        match first_full {
            None => first_full = Some(full),
            Some(first) => assert_eq!(
                (full.queued, full.spilled),
                (first.queued, first.spilled),
                "a second episode reads like the first"
            ),
        }

        assert_eq!(read_exactly(slave, READ), vec![b'p'; READ]);
        let refilled = settle(&sink, |b| b.queued == full.queued && b.spilled.is_some());
        assert_eq!(
            refilled.unread(),
            FRAME - READ,
            "episode {episode}: the bytes the drainer wrote from its chunk are counted \
             once, in the kernel's queue: {refilled:?}"
        );

        assert_eq!(read_exactly(slave, FRAME - READ), vec![b'p'; FRAME - READ]);
        assert!(sink.wait_egress_drained_to_kernel());
        let drained = settle(&sink, |b| b.spilled.is_some());
        assert_eq!(
            (drained.queued, drained.spilled),
            (0, Some(0)),
            "{drained:?}"
        );
    }
    drop(sink);
    close_pair(master, slave);
}

/// A METERED PASTE IS DATED BY ITS OWN WRITE. A paste write loop
#[test]
fn a_metered_paste_is_dated_by_its_own_write() {
    const AGE: Duration = Duration::from_millis(100);
    const PASTE: usize = 64 * 1024;
    let (master, slave) = pty_pair(true);
    let sink = Arc::new(nonblocking_sink(master));
    sink.backdate_input_for_testing(AGE);

    let meter = Arc::new(BulkMeter::new());
    let spawned_at = Instant::now();
    let paster = {
        let (sink, meter) = (Arc::clone(&sink), Arc::clone(&meter));
        thread::spawn(move || {
            sink.write_frame_metered_with_receipt(&[b'p'; PASTE], &meter)
                .expect("paste")
                .accepted()
        })
    };
    let _ = first_write_dated(&sink);
    assert!(
        !paster.is_finished(),
        "the paste must be parked on a full queue"
    );
    let parked = sink.input_backlog().expect("measured while parked");
    let since_spawn = spawned_at.elapsed();
    assert!(parked.queued > 0, "{parked:?}");
    assert!(
        parked.wait <= since_spawn,
        "dated by the metered write, not by the sink's birth: {parked:?}, \
         {since_spawn:?} since the paster was spawned"
    );

    let got = read_exactly(slave, PASTE);
    assert!(got.iter().all(|&b| b == b'p'));
    assert_eq!(paster.join().expect("paster"), PASTE);
    drop(sink);
    close_pair(master, slave);
}

/// Wait up to a minute for the slave to have input, then read what is
fn read_within(fd: i32, max: usize) -> Vec<u8> {
    let mut p = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `p` is a live pollfd on this stack; nfds == 1.
    let ready = unsafe { libc::poll(&mut p, 1, 60_000) };
    assert_eq!(ready, 1, "nothing reached the program within 60 s");
    let mut out = vec![0u8; max];
    // SAFETY: a bounded read into `out`, which is `max` bytes long.
    let n = unsafe { libc::read(fd, out.as_mut_ptr().cast(), max) };
    assert!(n > 0, "read({fd}) returned {n}");
    out.truncate(n as usize);
    out
}

/// A discard drops the spill with the queue, and the sink still reaches the program.
#[test]
fn a_discard_drops_a_parked_spill_and_the_next_key_still_arrives() {
    let (master, slave) = pty_pair(true);
    let sink = nonblocking_sink(master);
    let (parks_tx, parks) = mpsc::channel();
    sink.seam_for_testing(move |seam| {
        if seam == Seam::DrainPark {
            let _ = parks_tx.send(());
        }
    });
    let mut paste = vec![b'#'; 1100];
    paste.extend_from_slice(b"\recho SPILLED\r");
    assert_eq!(paste.len(), 1114);
    assert_eq!(sink.write_frame_nonparking(&paste).expect("spill"), 1114);
    let parked = settle(&sink, |b| b.spilled.is_some() && b.unread() == 1114);
    assert_eq!(parked.queued, 1022, "the raw queue is full: {parked:?}");
    parks
        .recv_timeout(MUST_HAPPEN)
        .expect("the drainer parked on the full queue");

    assert_eq!(sink.discard_unread_input(Discard::Restart), Some(1114));
    let empty = settle(&sink, |b| b.spilled.is_some());
    assert_eq!(empty.unread(), 0, "{empty:?}");

    assert_eq!(sink.write_frame_nonparking(b"k").expect("key"), 1);
    assert_eq!(
        read_within(slave, 64),
        b"k",
        "the key after the discard is all the program reads"
    );
    assert!(sink.wait_egress_drained_to_kernel());
    drop(sink);
    close_pair(master, slave);
}

/// A discard ends a writer parked mid-frame: a blocking `write_frame` that filled the
#[test]
fn a_discard_ends_a_writer_parked_mid_frame() {
    const PASTE: usize = 64 * 1024;
    let (master, slave) = pty_pair(true);
    let sink = Arc::new(nonblocking_sink(master));
    let paster = {
        let sink = Arc::clone(&sink);
        thread::spawn(move || sink.write_frame(&[b'p'; PASTE]).expect("paste"))
    };
    let parked = settle(&sink, |b| b.queued == 1022);
    assert!(
        !paster.is_finished(),
        "parked on the full queue: {parked:?}"
    );

    assert_eq!(sink.discard_unread_input(Discard::Restart), Some(1022));
    let finished = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(10), || {
        paster.is_finished().then_some(())
    });
    assert!(
        finished.is_some(),
        "the parked writer never looked again: it sleeps on the flushed queue"
    );
    let accepted = paster.join().expect("paster");
    assert_eq!(accepted, 1022, "the rest of the frame was not written");

    assert_eq!(sink.write_frame(b"k").expect("key"), 1);
    assert_eq!(read_within(slave, 64), b"k");
    drop(sink);
    close_pair(master, slave);
}

/// A flush is not a restart: `reset flush` and the drop before `signal term` empty the
#[test]
fn a_flush_moves_the_discard_epoch_and_not_the_restart_count() {
    let (master, slave) = pty_pair(true);
    let sink = nonblocking_sink(master);
    assert_eq!((sink.discards(), sink.restart_discards()), (0, 0));
    assert_eq!(sink.write_frame_nonparking(b"ab").expect("write"), 2);
    let _ = settle(&sink, |b| b.queued == 2);
    assert_eq!(sink.discard_unread_input(Discard::Flush), Some(2));
    assert_eq!(
        (sink.discards(), sink.restart_discards()),
        (1, 0),
        "a flush is a discard, not a restart"
    );
    assert_eq!(sink.discard_unread_input(Discard::Restart), Some(0));
    assert_eq!((sink.discards(), sink.restart_discards()), (2, 1));
    assert_eq!(sink.discard_unread_input(Discard::Flush), Some(0));
    assert_eq!(sink.write_frame_nonparking(b"x").expect("write"), 1);
    let _ = settle(&sink, |b| b.queued == 1);
    assert_eq!(read_exactly(slave, 1), b"x");
    assert!(sink.read_since_discard(), "read after the flush");
    assert_eq!((sink.discards(), sink.restart_discards()), (3, 1));
    drop(sink);
    close_pair(master, slave);

    let (a, b) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    use std::os::fd::AsRawFd;
    let sink = SinkWriter::new(a.as_raw_fd());
    assert_eq!(sink.discard_unread_input(Discard::Restart), None);
    assert_eq!((sink.discards(), sink.restart_discards()), (0, 0));
    drop(sink);
    drop((a, b));
}

/// A read after a discard is told from the discard. The discard's empty queue is what
#[test]
fn a_read_after_a_discard_is_told_from_the_discard() {
    let (master, slave) = pty_pair(true);
    let sink = nonblocking_sink(master);
    assert!(!sink.read_since_discard(), "no discard yet");
    assert_eq!(sink.write_frame_nonparking(b"ab").expect("write"), 2);
    let _ = settle(&sink, |b| b.queued == 2);
    assert_eq!(sink.discard_unread_input(Discard::Restart), Some(2));
    assert!(
        !sink.read_since_discard(),
        "nothing written since the discard"
    );

    assert_eq!(sink.write_frame_nonparking(b"x").expect("write"), 1);
    let _ = settle(&sink, |b| b.queued == 1);
    assert!(!sink.read_since_discard(), "written, not read");
    assert_eq!(read_exactly(slave, 1), b"x");
    assert!(sink.read_since_discard(), "the program read it");
    assert_eq!(sink.write_frame_nonparking(b"y").expect("write"), 1);
    let _ = settle(&sink, |b| b.queued == 1);
    assert!(sink.read_since_discard(), "a byte behind the read keeps it");

    assert_eq!(sink.discard_unread_input(Discard::Restart), Some(1));
    assert!(!sink.read_since_discard(), "the next discard starts over");
    drop(sink);
    close_pair(master, slave);

    let (master, slave) = pty_pair(false);
    let sink = nonblocking_sink(master);
    assert_eq!(sink.discard_unread_input(Discard::Restart), Some(0));
    assert_eq!(sink.write_frame_nonparking(b"c").expect("write"), 1);
    let partial = settle(&sink, |b| b.canonical);
    assert_eq!(partial.queued, 0, "FIONREAD cannot see a partial line");
    assert!(!sink.read_since_discard(), "a partial line is not a read");
    drop(sink);
    close_pair(master, slave);

    let (a, b) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    use std::os::fd::AsRawFd;
    let sink = SinkWriter::new(a.as_raw_fd());
    assert!(!sink.read_since_discard(), "off a tty nothing is told");
    drop(sink);
    drop((a, b));
}

/// A cbreak program's control key is not a read. With ICANON off but ISIG, IEXTEN and
#[test]
fn a_control_key_a_cbreak_driver_eats_is_not_a_read() {
    let (master, slave) = pty_pair(true);
    // SAFETY: all-zeros is a valid termios, overwritten by `tcgetattr`.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `slave` is this test's live pty slave; `t` is an out-param.
    assert_eq!(unsafe { libc::tcgetattr(slave, &mut t) }, 0);
    t.c_lflag |= libc::ISIG | libc::IEXTEN;
    t.c_iflag |= libc::IXON;
    // SAFETY: `slave` is live; `t` is derived from its own termios.
    assert_eq!(unsafe { libc::tcsetattr(slave, libc::TCSANOW, &t) }, 0);
    let sink = nonblocking_sink(master);
    assert_eq!(sink.write_frame_nonparking(b"a").expect("write"), 1);
    let _ = settle(&sink, |b| b.queued == 1);
    assert_eq!(sink.discard_unread_input(Discard::Restart), Some(1));
    for key in [0x03_u8, 0x0f, 0x13, 0x11] {
        assert_eq!(sink.write_frame_nonparking(&[key]).expect("write"), 1);
        let seen = settle(&sink, |b| b.queued == 0);
        assert_eq!(seen.queued, 0, "{key:#04x}: the driver ate it");
        assert!(
            !sink.read_since_discard(),
            "{key:#04x} left the queue with nothing reading the slave"
        );
    }
    drop(sink);
    close_pair(master, slave);
}

/// What the program has been given, read off a NON-BLOCKING slave for at most
fn read_all_within(slave: i32, n: usize) -> Vec<u8> {
    let mut got = Vec::new();
    let done = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(5), || {
        let mut buf = [0_u8; 4096];
        // SAFETY: a bounded read into a live stack buffer from the live slave.
        let r = unsafe { libc::read(slave, buf.as_mut_ptr().cast(), buf.len()) };
        if let Ok(r) = usize::try_from(r) {
            got.extend_from_slice(&buf[..r]);
        }
        (got.len() >= n).then_some(())
    });
    assert!(done.is_some(), "only {} of {n} bytes came", got.len());
    got
}

/// Whether the non-blocking slave holds nothing more to read.
fn nothing_more(slave: i32) -> bool {
    let mut one = [0_u8; 1];
    // SAFETY: a one-byte read into a live stack buffer from the live slave.
    let r = unsafe { libc::read(slave, one.as_mut_ptr().cast(), 1) };
    r < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock
}

/// THE GUARDED WRITE ON A REAL PTY: a text longer than a raw slave's input
#[test]
fn a_guarded_text_longer_than_the_input_queue_is_delivered_whole_and_in_order() {
    let mut text = b"[aterm harness] save your work: ".to_vec();
    text.resize(1100, b'w');
    text.extend_from_slice(b" ATERM-SAVED-3f9a");
    text.truncate(1118);

    let (master, slave) = pty_pair(true);
    aterm_pty::set_nonblocking(slave, true).expect("nonblocking slave");
    let sink = nonblocking_sink(master);
    let (outcome, order) = sink.try_write_frame_whole_with_receipt(&text);
    assert_eq!(
        outcome,
        ImmediateWrite::Queued { accepted: 1022 },
        "the raw queue's room"
    );
    assert!(order.is_some(), "the accepted prefix carries its order");
    assert_eq!(
        sink.try_write_frame_whole_with_receipt(b"\r").0,
        ImmediateWrite::BusyZero
    );
    assert_eq!(
        read_all_within(slave, text.len()),
        text,
        "the whole text, in order"
    );
    let pressed = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(5), || {
        match sink.try_write_frame_whole_with_receipt(b"\r").0 {
            ImmediateWrite::Full => Some(()),
            ImmediateWrite::BusyZero => None,
            other => panic!("an Enter with room: {other:?}"),
        }
    });
    assert!(pressed.is_some(), "the Enter never went after the text");
    assert_eq!(read_all_within(slave, 1), b"\r");
    assert!(nothing_more(slave));
    drop(sink);
    close_pair(master, slave);

    let (master, slave) = pty_pair(true);
    aterm_pty::set_nonblocking(slave, true).expect("nonblocking slave");
    let sink = nonblocking_sink(master);
    assert_eq!(
        sink.try_write_frame_immediate_with_receipt(&text).0,
        ImmediateWrite::PartialInDoubt { accepted: 1022 }
    );
    assert_eq!(sink.spilled_bytes(), 0, "nothing queued for the rest");
    assert_eq!(read_all_within(slave, 1022), &text[..1022]);
    assert!(nothing_more(slave), "the rest never arrives");
    drop(sink);
    close_pair(master, slave);
}

/// A 1118-byte text: 1022 `H`s, all a raw slave's input queue takes at once,
fn head_and_tail() -> Vec<u8> {
    let mut text = vec![b'H'; 1022];
    text.extend_from_slice(&[b'T'; 96]);
    text
}

/// Press a guarded Enter until the sink takes it (a drainer that abandoned
fn the_next_key_alone(sink: &SinkWriter, slave: i32) -> Vec<u8> {
    let pressed = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(5), || {
        match sink.try_write_frame_whole_with_receipt(b"\r").0 {
            ImmediateWrite::Full => Some(()),
            ImmediateWrite::BusyZero => None,
            other => panic!("an Enter into an empty queue: {other:?}"),
        }
    });
    assert!(pressed.is_some(), "the Enter never went after the discard");
    let got = read_all_within(slave, 1);
    assert!(nothing_more(slave));
    got
}

/// A DISCARD NEVER SPLITS A GUARDED FRAME: the discard (`reset flush`, the
#[test]
fn a_discard_drops_a_guarded_frame_whole_or_lets_it_through_whole() {
    let text = Arc::new(head_and_tail());

    let (master, slave) = pty_pair(true);
    aterm_pty::set_nonblocking(slave, true).expect("nonblocking slave");
    let sink = Arc::new(nonblocking_sink(master));
    let raced = Arc::new(Mutex::new(Vec::new()));
    {
        let (weak, text, raced) = (Arc::downgrade(&sink), Arc::clone(&text), Arc::clone(&raced));
        sink.seam_for_testing(move |seam| {
            if seam == Seam::DiscardFlush
                && let Some(sink) = weak.upgrade()
            {
                let outcome = sink.try_write_frame_whole_with_receipt(&text).0;
                raced.lock().expect("raced").push(outcome);
            }
        });
    }
    let dropped = sink.discard_unread_input(Discard::Flush);
    assert_eq!(
        *raced.lock().expect("raced"),
        [ImmediateWrite::BusyZero],
        "a guarded write at the flush waits the discard out"
    );
    assert_eq!(dropped, Some(0), "an empty queue and spill");
    assert!(nothing_more(slave), "nothing of it was written");
    assert_eq!(
        sink.try_write_frame_whole_with_receipt(&text).0,
        ImmediateWrite::Queued { accepted: 1022 }
    );
    assert_eq!(read_all_within(slave, text.len()), *text, "whole, in order");
    drop(sink);
    close_pair(master, slave);

    let (master, slave) = pty_pair(true);
    aterm_pty::set_nonblocking(slave, true).expect("nonblocking slave");
    let sink = nonblocking_sink(master);
    assert_eq!(
        sink.try_write_frame_whole_with_receipt(&text).0,
        ImmediateWrite::Queued { accepted: 1022 }
    );
    assert_eq!(
        sink.discard_unread_input(Discard::Flush),
        Some(text.len()),
        "the head from the kernel's queue and the rest from the spill"
    );
    assert_eq!(
        the_next_key_alone(&sink, slave),
        b"\r",
        "no part of the text"
    );
    drop(sink);
    close_pair(master, slave);

    let (master, slave) = pty_pair(true);
    aterm_pty::set_nonblocking(slave, true).expect("nonblocking slave");
    let sink = nonblocking_sink(master);
    assert_eq!(
        sink.try_write_frame_whole_with_receipt(&text).0,
        ImmediateWrite::Queued { accepted: 1022 }
    );
    assert_eq!(aterm_pty::flush_input_queue(master), Some(1022));
    assert_eq!(
        read_all_within(slave, 96),
        [b'T'; 96],
        "the end of the text, with no beginning"
    );
    assert!(nothing_more(slave));
    drop(sink);
    close_pair(master, slave);
}

/// THE DRAINER NEVER SPLITS ONE EITHER: it looks at the discard epoch and
#[test]
fn a_discard_racing_the_drainers_write_drops_the_frame_whole() {
    let text = head_and_tail();
    let (master, slave) = pty_pair(true);
    aterm_pty::set_nonblocking(slave, true).expect("nonblocking slave");
    let sink = Arc::new(nonblocking_sink(master));
    let (at_tx, at_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go_rx = Mutex::new(go_rx);
    let stopped = AtomicBool::new(false);
    sink.seam_for_testing(move |seam| {
        if seam != Seam::DrainWrite || stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = at_tx.send(());
        go_rx
            .lock()
            .expect("go")
            .recv_timeout(MUST_HAPPEN)
            .expect("the test lets the drainer write");
    });
    assert_eq!(
        sink.try_write_frame_whole_with_receipt(&text).0,
        ImmediateWrite::Queued { accepted: 1022 }
    );
    at_rx
        .recv_timeout(MUST_HAPPEN)
        .expect("the drainer reached its write");
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let discarder = {
        let sink = Arc::clone(&sink);
        thread::spawn(move || {
            let dropped = sink.discard_unread_input(Discard::Flush);
            let _ = done_tx.send(());
            dropped
        })
    };
    let finished_early = done_rx.recv_timeout(Duration::from_millis(300)).is_ok();
    go_tx.send(()).expect("the drainer waits");
    let dropped = discarder.join().expect("the discard");
    assert!(
        !finished_early,
        "the discard ran between the drainer's look and its write"
    );
    assert_eq!(dropped, Some(text.len()), "head and rest");
    assert_eq!(
        the_next_key_alone(&sink, slave),
        b"\r",
        "no part of the text"
    );
    drop(sink);
    close_pair(master, slave);
}

/// A DISCARD BEHIND A GUARDED WRITE MARKS WHAT THE KERNEL TOOK. The guarded
#[test]
fn a_discard_behind_a_guarded_write_marks_what_the_kernel_took() {
    let text = head_and_tail();
    let (master, slave) = pty_pair(true);
    aterm_pty::set_nonblocking(slave, true).expect("nonblocking slave");
    let sink = Arc::new(nonblocking_sink(master));
    let dropped = Arc::new(Mutex::new(None));
    {
        let (weak, dropped) = (Arc::downgrade(&sink), Arc::clone(&dropped));
        let fired = AtomicBool::new(false);
        sink.install_spill_arranger(move || {
            let Some(sink) = weak.upgrade() else { return };
            if !fired.swap(true, Ordering::SeqCst) {
                *dropped.lock().expect("dropped") = sink.discard_unread_input(Discard::Flush);
            }
            sink.arrange_pending_drainer();
        });
    }
    assert_eq!(
        sink.try_write_frame_whole_with_receipt(&text).0,
        ImmediateWrite::Queued { accepted: 1022 }
    );
    assert_eq!(
        *dropped.lock().expect("dropped"),
        Some(text.len()),
        "the head from the kernel's queue and the rest from the spill"
    );
    assert!(!sink.read_since_discard(), "nothing written since");
    let pressed = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(5), || {
        match sink.try_write_frame_whole_with_receipt(b"x").0 {
            ImmediateWrite::Full => Some(()),
            ImmediateWrite::BusyZero => None,
            other => panic!("one byte into an empty queue: {other:?}"),
        }
    });
    assert!(pressed.is_some(), "the byte never went after the discard");
    let _ = settle(&sink, |b| b.queued == 1);
    assert!(!sink.read_since_discard(), "written, not read");
    assert_eq!(read_all_within(slave, 1), b"x");
    assert!(nothing_more(slave));
    assert!(sink.read_since_discard(), "the program read it");
    drop(sink);
    close_pair(master, slave);
}
