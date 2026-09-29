// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `SinkWriter::input_backlog` against a REAL pty — the reading built for the
//! 2026-09-24 incident, where a frozen Claude Code left the owner's Enter
//! unread in its slave's input queue for hours and a supervisor's
//! screen-fenced key queued behind it.
//!
//! The pure halves (the ledger truth table, the word and refusal tables) are
//! `aterm_session::input_backlog`'s unit tests; the socketpair fixtures every
//! other sink test drives are not ttys and read `None`. This file is the one
//! place the kernel's own count meets the ledger's dates: a raw slave that
//! does not read, bytes written through the sink, and the probe's
//! `queued`/`wait` checked against what the test did and when.
//!
//! macOS-only: the queue probes answer `None` everywhere else (on Linux a
//! master's `FIONREAD` counts output — aterm-pty's `input_queue_len` says
//! why). The pairs come from `openpty`, whose slave is inheritable; that is
//! harmless here because this test binary spawns no child that could inherit
//! it (aterm-pty's own tests, which do spawn, use its close-on-exec opener).
#![cfg(target_os = "macos")]

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use aterm_session::input_backlog::InputBacklog;
use aterm_session::sink::{BulkMeter, Discard, SinkWriter};

/// A pty pair with the slave raw (`VMIN = 1`, `VTIME = 0`, a key-at-a-time
/// reader like every agent TUI) or canonical without echo.
fn pty_pair(raw: bool) -> (i32, i32) {
    // One `openpty` at a time. The harness runs these tests on parallel
    // threads, and unserialised, `openpty` failed here with `-1` about once
    // in thirty runs of this file (2026-09-25, once it had eight tests), and
    // under three concurrent runs of the binary six times in forty-five.
    // Serialised, forty-five runs at that concurrency failed none. The cause
    // inside libc was not traced; only the in-process race is measured.
    static OPENPTY: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = OPENPTY.lock().unwrap_or_else(|p| p.into_inner());
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: both out-params are live `c_int`s on this stack; the name,
    // termios and winsize pointers may be null.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty");
    // SAFETY: `libc::termios` is plain integer fields and a byte array; all
    // zeros is a valid value, overwritten by `tcgetattr` below.
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
/// production shape (spawn.rs `note_master_nonblocking`).
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
/// kernel's count moves under the test), panicking with the last reading
/// after five seconds.
fn settle(sink: &SinkWriter, done: impl Fn(&InputBacklog) -> bool) -> InputBacklog {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let b = sink.input_backlog().expect("a pty master is measured");
        if done(&b) {
            return b;
        }
        assert!(Instant::now() < deadline, "never settled: {b:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

/// Wait until a parked paste's first bytes are in the queue AND DATED. The
/// ledger stamps a write after `write(2)` returns (`ledger_end`), and until it
/// has, the probe reads the queued bytes as in flight (`wait` zero). A nonzero
/// `wait` therefore means the stamp is older than this call's return, so an
/// age bound slept from here counts from an event this thread SAW — never
/// from the paster's `spawn`, which a loaded scheduler can start later than a
/// `sleep` beside it overshoots.
fn first_write_dated(sink: &SinkWriter) -> InputBacklog {
    settle(sink, |b| b.queued > 0 && !b.wait.is_zero())
}

/// RAW UNREAD INPUT IS COUNTED AND DATED — the incident's shape. `abc` goes in
/// and reads `queued == 3`; 300 ms later the oldest byte has waited at least
/// that long; the program reading ONE byte leaves 2, still dated by the
/// original write; a NEW write behind them (a driver's key) does not make the
/// old bytes young; and reading everything clears the reading to zero.
#[test]
fn raw_unread_input_is_counted_and_dated() {
    let (master, slave) = pty_pair(true);
    let sink = SinkWriter::new(master);
    let empty = sink.input_backlog().expect("a pty master is measured");
    assert_eq!((empty.queued, empty.wait), (0, Duration::ZERO));
    assert!(!empty.canonical);
    assert_eq!(empty.spilled, Some(0));
    assert_eq!(empty.output_backlog, Some(0));

    assert_eq!(sink.write_frame(b"abc").expect("write"), 3);
    let fresh = sink.input_backlog().expect("measured");
    assert_eq!(fresh.queued, 3);
    assert!(fresh.wait < Duration::from_millis(300), "{fresh:?}");

    thread::sleep(Duration::from_millis(300));
    let aged = sink.input_backlog().expect("measured");
    assert_eq!(aged.queued, 3);
    assert!(aged.wait >= Duration::from_millis(300), "{aged:?}");

    assert_eq!(read_exactly(slave, 1), b"a");
    let partial = sink.input_backlog().expect("measured");
    assert_eq!(partial.queued, 2, "the program read one byte");
    assert!(
        partial.wait >= Duration::from_millis(300),
        "the unread `bc` are still the old write's: {partial:?}"
    );

    assert_eq!(sink.write_frame(b"\x1b[B").expect("write"), 3);
    let behind = sink.input_backlog().expect("measured");
    assert_eq!(behind.queued, 5);
    assert!(
        behind.wait >= Duration::from_millis(300),
        "a key queued behind stale bytes does not make them young: {behind:?}"
    );

    assert_eq!(read_exactly(slave, 5), b"bc\x1b[B");
    let drained = sink.input_backlog().expect("measured");
    assert_eq!((drained.queued, drained.wait), (0, Duration::ZERO));
    drop(sink);
    close_pair(master, slave);
}

/// CANONICAL MODE COUNTS COMPLETE LINES ONLY: a partial `ab` at a cooked
/// prompt is invisible to FIONREAD, so it reads 0 — and `canonical` says so,
/// which is what keeps a shell's type-ahead from ever reading as a stall.
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
/// program that has stopped reading fills the raw queue, and the sink's
/// blocking write then waits in `poll(POLLOUT)` HOLDING the fd lock for as
/// long as the program stays frozen. The probe takes neither that lock nor
/// the park's chunk as "in flight" (the sink parks between ledger brackets),
/// so it still dates the queue — here by the first bytes the kernel took,
/// more than a second ago — while the writer is parked, and a keystroke that
/// spills behind it is reported too.
#[test]
fn a_writer_parked_on_a_full_queue_does_not_blind_the_probe() {
    let (master, slave) = pty_pair(true);
    let sink = Arc::new(nonblocking_sink(master));

    const PASTE: usize = 64 * 1024;
    let paster = {
        let sink = Arc::clone(&sink);
        thread::spawn(move || sink.write_frame(&[b'p'; PASTE]).expect("paste"))
    };
    // The second is counted from the first DATED bytes, not from the spawn.
    // A probe blinded by the park (the regression) does not date them: `settle`
    // fails on its `wait: 0ns` reading, or, should one poll land between the
    // two brackets, the reading after the sleep is blind and fails below.
    let _ = first_write_dated(&sink);
    thread::sleep(Duration::from_millis(1200));
    assert!(
        !paster.is_finished(),
        "the paste must be parked on a full queue"
    );

    let parked = sink
        .input_backlog()
        .expect("measured while a writer is parked");
    assert!(parked.queued > 0, "{parked:?}");
    assert!(
        parked.wait >= Duration::from_secs(1),
        "the parked paste's chunk is not in flight, so the queue keeps its date: {parked:?}"
    );

    // A keystroke now cannot reach the kernel: it spills behind the paste.
    assert_eq!(sink.write_frame_nonparking(b"z").expect("spill"), 1);
    let spilled = sink.input_backlog().expect("measured");
    assert!(spilled.wait >= Duration::from_secs(1), "{spilled:?}");
    assert!(
        matches!(spilled.spilled, Some(1) | None),
        "the spilled key is reported (or its mutex was busy): {spilled:?}"
    );

    // The program wakes and reads everything, in order.
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

/// A DRAINER PARKED PARTWAY THROUGH A CHUNK COUNTS EACH UNREAD BYTE ONCE. A
/// 4000-byte keystroke frame into a raw reader that has stopped reading
/// fills the queue with its head; its tail spills, and the drainer takes
/// the whole tail as ONE chunk and parks on the full queue. The program then
/// reads 500 bytes, the drainer hands the kernel about 500 more from the
/// middle of that chunk, and parks again. Those bytes are now in the queue
/// FIONREAD counts, so the spill must stop counting them: `unread()` is
/// exactly 4000 - 500. Before the spill tracked how far into its chunk the
/// drainer had got, this read `queued: 1022, spilled: Some(2978)` — 4000 —
/// the 500 counted twice (reviewer finding on S2, 2026-09-24; the S6 status
/// field `input_bytes` is this sum). A second episode on the same sink reads
/// the first one's figure again, so the mark is cleared with its chunk.
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
        let drained = sink.input_backlog().expect("measured");
        assert_eq!(
            (drained.queued, drained.spilled),
            (0, Some(0)),
            "{drained:?}"
        );
    }
    drop(sink);
    close_pair(master, slave);
}

/// A METERED PASTE IS DATED BY ITS OWN WRITE. origin/main's large-paste
/// meter (`SinkWriter::write_frame_metered_with_receipt`) arrived with a
/// write loop of its own that called `aterm_pty::write_some_blocking`
/// directly; merged over this branch on 2026-09-25 it wrote around the
/// ledger. Bytes the ledger never saw read as older than the sink itself
/// (`InputLedger::oldest_unread_at` dates them from its birth), so a paste
/// into a frozen program was dated by whenever the SESSION started, not by
/// the paste. The sink is aged first so the two dates are far apart: the
/// parked paste must read at least the time it has been parked, and less
/// than the sink's age.
#[test]
fn a_metered_paste_is_dated_by_its_own_write() {
    const AGE: Duration = Duration::from_millis(1500);
    const PARKED: Duration = Duration::from_millis(400);
    const PASTE: usize = 64 * 1024;
    let (master, slave) = pty_pair(true);
    let sink = Arc::new(nonblocking_sink(master));
    thread::sleep(AGE);

    let meter = Arc::new(BulkMeter::new());
    let paster = {
        let (sink, meter) = (Arc::clone(&sink), Arc::clone(&meter));
        thread::spawn(move || {
            sink.write_frame_metered_with_receipt(&[b'p'; PASTE], &meter)
                .expect("paste")
                .accepted()
        })
    };
    // PARKED is counted from the paste's first DATED bytes. Slept from the
    // spawn it had no margin: `wait` read PARKED + the sleep's overshoot -
    // the paster's start latency, under PARKED whenever the new thread
    // started later than the sleep overshot. A write around the ledger (the
    // regression) is dated from the sink's birth, so this returns at once and
    // the `wait < AGE` bound below still fails on it.
    let _ = first_write_dated(&sink);
    thread::sleep(PARKED);
    assert!(
        !paster.is_finished(),
        "the paste must be parked on a full queue"
    );
    let parked = sink.input_backlog().expect("measured while parked");
    assert!(parked.queued > 0, "{parked:?}");
    assert!(
        parked.wait >= PARKED && parked.wait < AGE,
        "dated by the metered write, not by the sink's birth: {parked:?}"
    );

    let got = read_exactly(slave, PASTE);
    assert!(got.iter().all(|&b| b == b'p'));
    assert_eq!(paster.join().expect("paster"), PASTE);
    drop(sink);
    close_pair(master, slave);
}

/// Wait up to two seconds for the slave to have input, then read what is
/// there — so a wedged sink fails the test instead of hanging it.
fn read_within(fd: i32, max: usize) -> Vec<u8> {
    let mut p = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `p` is a live pollfd on this stack; nfds == 1.
    let ready = unsafe { libc::poll(&mut p, 1, 2000) };
    assert_eq!(ready, 1, "nothing reached the program within 2 s");
    let mut out = vec![0u8; max];
    // SAFETY: a bounded read into `out`, which is `max` bytes long.
    let n = unsafe { libc::read(fd, out.as_mut_ptr().cast(), max) };
    assert!(n > 0, "read({fd}) returned {n}");
    out.truncate(n as usize);
    out
}

/// A DISCARD DROPS THE SPILL WITH THE QUEUE, AND THE SINK STILL REACHES THE
/// PROGRAM (whole-branch review, second round, 2026-09-25). The shape of a
/// paste into a frozen raw program: 1118 bytes, of which the queue takes
/// 1022 and the sink spills 96, with the drainer parked in `poll(POLLOUT)`
/// holding the fd lock. The first cut of the restart remedy flushed only
/// the kernel queue. A flush does not wake a parked poll on Darwin, and
/// the emptied queue gives nobody a reason to read, so the drainer slept
/// for good and a key written after it queued behind the 96 forever.
/// `discard_unread_input` drops both (1118), and the next key is the next
/// thing the program reads, alone.
#[test]
fn a_discard_drops_a_parked_spill_and_the_next_key_still_arrives() {
    let (master, slave) = pty_pair(true);
    let sink = nonblocking_sink(master);
    let mut paste = vec![b'#'; 1100];
    paste.extend_from_slice(b"\recho SPILLED\r");
    assert_eq!(paste.len(), 1114);
    assert_eq!(sink.write_frame_nonparking(&paste).expect("spill"), 1114);
    let parked = settle(&sink, |b| b.spilled.is_some() && b.unread() == 1114);
    assert_eq!(parked.queued, 1022, "the raw queue is full: {parked:?}");
    // The spill is counted as soon as it is committed; give the drainer
    // thread the moment it needs to peek its chunk and park on the queue.
    thread::sleep(Duration::from_millis(200));

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

/// A DISCARD ENDS A WRITER PARKED MID-FRAME. A blocking `write_frame` (a
/// paste on its own thread) fills the queue and parks HOLDING the fd lock.
/// After the discard it must neither sleep for good on the flushed queue
/// nor hand the kernel the rest of its frame, which was meant for the
/// program the discard was for. It returns short, and the next key reaches
/// the program alone.
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
    let deadline = Instant::now() + Duration::from_secs(2);
    while !paster.is_finished() {
        assert!(
            Instant::now() < deadline,
            "the parked writer never looked again: it sleeps on the flushed queue"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let accepted = paster.join().expect("paster");
    assert_eq!(accepted, 1022, "the rest of the frame was not written");

    assert_eq!(sink.write_frame(b"k").expect("key"), 1);
    assert_eq!(read_within(slave, 64), b"k");
    drop(sink);
    close_pair(master, slave);
}

/// Off a tty there is no queue to flush, so nothing is discarded — the
/// spill included — and the answer says so.
#[test]
fn a_discard_off_a_tty_drops_nothing() {
    let (a, b) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    use std::os::fd::AsRawFd;
    let sink = SinkWriter::new(a.as_raw_fd());
    assert_eq!(sink.discard_unread_input(Discard::Restart), None);
    drop(sink);
    drop((a, b));
}

/// A FLUSH IS NOT A RESTART (robustness review of the manual reset,
/// 2026-09-26). `reset flush` and the drop before `signal term` empty the
/// same queue and both move the discard epoch — a frame in flight belongs to
/// the input that went, whichever drop took it. Only the restart's drop moves
/// `restart_discards`, the count aterm-gui's input watch starts a published
/// stall's restart grace on. Before the split a `reset flush` on a stalled
/// program read as its restart signal, and five seconds later the program
/// was named as having survived it, remedy `signal kill`, though no signal
/// was ever sent. Off a tty nothing is dropped, and neither count moves.
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
    // The read evidence counts from the LATEST drop, whichever it was.
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

/// A READ AFTER A DISCARD IS TOLD FROM THE DISCARD (whole-branch review,
/// fourth round, 2026-09-25). The discard's empty queue is what a reader
/// leaves too, so aterm-gui holds a stall through it; this is the evidence
/// that lets the hold go when the program reads again. Nothing written since
/// the discard is no read. A byte written and still queued is no read. The
/// program reading it is one, and a byte queued behind that read does not
/// undo it. The next discard starts over. In canonical mode FIONREAD cannot
/// see a partial line, so a byte typed there never reads as read: that is
/// the negative control for the count the answer rests on.
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

/// A CBREAK PROGRAM'S CONTROL KEY IS NOT A READ (review of the restart hold's
/// read evidence, 2026-09-25). With ICANON off but ISIG, IEXTEN and IXON on
/// — Python's `tty.setcbreak`, ncurses `cbreak()` — the line discipline
/// consumes `^C`, `^O` and `^S` as they are written: the ledger counts the
/// byte and the queue never holds it. The held gate admits a lone `^C` on
/// purpose (it is a signal, the remedy), so the queue's shrink was read as
/// the program reading and released a program still frozen. Nothing reads the
/// slave here, the queue is seen EMPTY after each key — the evidence the old
/// answer rested on — and the answer stays `false`. The raw twin in
/// [`a_read_after_a_discard_is_told_from_the_discard`] is the positive
/// control: there a real read answers `true`.
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
