// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A hang detector, never a latency budget: every wait on it ends the
/// moment the thing it waits for happens.
const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);

/// The wake pipe: both ends close-on-exec (they must not survive a seamless
/// re-exec) and non-blocking, so a redundant [`wake`] against a FULL pipe
/// returns instead of parking the event loop. A blocking write end hangs the
/// saturation loop below; a [`wake`] that retried `EAGAIN` hangs at the end.
#[test]
fn a_wake_pipe_is_cloexec_and_a_wake_into_a_full_one_never_blocks() {
    let (rd, wr) = make_wake_pipe().expect("wake pipe");
    // SAFETY: read-only fcntl queries on two live fds owned by this test.
    unsafe {
        for fd in [rd, wr] {
            assert_ne!(libc::fcntl(fd, libc::F_GETFD) & libc::FD_CLOEXEC, 0);
            assert_ne!(libc::fcntl(fd, libc::F_GETFL) & libc::O_NONBLOCK, 0);
        }
    }
    let fill = [0x55u8; 4096];
    // SAFETY: bounded writes to this test's live non-blocking pipe end.
    while unsafe { libc::write(wr, fill.as_ptr().cast(), fill.len()) >= 0 } {}
    assert_eq!(
        io::Error::last_os_error().kind(),
        io::ErrorKind::WouldBlock,
        "pipe saturation must report EAGAIN"
    );
    wake(wr);
    close_fd(rd);
    close_fd(wr);
}

/// The session ends with its shell: with the slave held by a leftover job, so the
/// master never reads data or EOF, the exit watch answers `Exited` once the shell
/// exits, with or without a wake pipe; while it lives the reader parks, and a wake
/// still wins.
#[test]
fn read_or_wake_answers_exited_when_the_shell_exits_while_the_slave_is_held() {
    use std::os::fd::AsRawFd as _;
    use std::sync::mpsc;
    // The master a job holds open: never data, never EOF.
    let mut m = [0i32; 2];
    // SAFETY: valid 2-int out-array for pipe(2).
    assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
    let (master_rd, master_wr) = (m[0], m[1]);
    // The shell: it exits when its stdin closes.
    let mut shell = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "read _"])
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn the shell");
    let exit = aterm_uds::exitwatch::exit_fd(shell.id()).expect("a live child is watchable");
    let mut buf = [0u8; 16];

    // Alive: the wakeless poll times out, and a wake wins over the watch.
    assert!(matches!(
        read_or_wake(master_rd, &mut buf, -1, exit.as_raw_fd(), -1),
        ReadOutcome::Idle
    ));
    let (wake_rd, wake_wr) = make_wake_pipe().expect("wake pipe");
    wake(wake_wr);
    assert!(matches!(
        read_or_wake(master_rd, &mut buf, wake_rd, exit.as_raw_fd(), -1),
        ReadOutcome::Wake
    ));
    close_fd(wake_wr);
    close_fd(wake_rd);

    let (wake_rd, wake_wr) = make_wake_pipe().expect("wake pipe");
    let (tx, rx) = mpsc::channel();
    let watch = exit.as_raw_fd();
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        let parked = matches!(
            read_or_wake(master_rd, &mut buf, wake_rd, watch, -1),
            ReadOutcome::Exited
        );
        let wakeless = matches!(
            read_or_wake(master_rd, &mut buf, -1, watch, -1),
            ReadOutcome::Exited
        );
        tx.send((parked, wakeless)).unwrap();
    });
    drop(shell.stdin.take());
    // A hang detector: the shell's exit is the event.
    let answered = rx.recv_timeout(HANG_DETECTOR);
    if answered.is_err() {
        // Free the parked reader so the test can end.
        wake(wake_wr);
    }
    reader.join().unwrap();
    let _ = shell.wait();
    close_fd(wake_wr);
    close_fd(wake_rd);
    // SAFETY: closing the two pipe fds this test owns.
    unsafe {
        libc::close(master_rd);
        libc::close(master_wr);
    }
    assert_eq!(
        answered.ok(),
        Some((true, true)),
        "the shell exited while its slave was held: the reader must say so"
    );
}

/// An interactive-sized burst (under one outq) is delivered on the first quiet read,
/// with no park (`at_gap` never fires, no bridge poll is counted), and gathers exactly
/// the bytes written.
#[test]
fn drain_more_nonblocking_delivers_interactive_burst_on_first_quiet_read() {
    let mut m = [0i32; 2];
    // SAFETY: valid 2-int out-array for pipe(2).
    assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
    let (rd, wr) = (m[0], m[1]);
    set_nonblocking(rd, true).expect("nonblock read end");
    // SAFETY: bounded write to this test's live pipe end.
    assert_eq!(unsafe { libc::write(wr, b"echo".as_ptr().cast(), 4) }, 4);
    let mut buf = [0u8; 65_536];
    let n = read(rd, &mut buf[..1]); // stand-in for the read_or_wake first chunk
    assert_eq!(n, 1);
    let polls_before = super::BRIDGE_POLLS.with(std::cell::Cell::get);
    let mut gaps = 0u32;
    let filled = drain_more_nonblocking(rd, &mut buf, 1, -1, -1, None, never_hot, &mut |_, _| {
        gaps += 1;
        false
    });
    let polls = super::BRIDGE_POLLS.with(std::cell::Cell::get) - polls_before;
    close_fd(rd);
    close_fd(wr);
    assert_eq!(filled, 4, "must gather the whole burst");
    assert_eq!(&buf[..4], b"echo");
    assert_eq!(
        (gaps, polls),
        (0, 0),
        "interactive drain must return on the first quiet read, not bridge \
         the gap: (dry gaps parked at, bridge polls taken)"
    );
}

/// The host's "a human is waiting" hint pinned off: the flood-shaped gather.
fn never_hot() -> bool {
    false
}

/// The hint pinned on: a keystroke is pending for the whole gather.
fn always_hot() -> bool {
    true
}

/// P05 — a CONTINUOUS stream (a refill waiting at every dry gap, for longer
/// than any budget) is cut by the SHORTER hot budget while a keystroke is
/// pending, and by the flood budget otherwise. Red before the fix: both arms
/// ran to 3 ms. The hot arm bridges every gap with the µs idle wait, never
/// the 1 ms bridge poll, EVEN WHILE THE PARSER IS BUSY, and each refill that
/// lands in that wait joins the same batch (else the gather ends at its first
/// gap and no budget ends it).
///
/// The stream comes from the park seam, not a thread: each gather writes one
/// byte from `at_gap`, immediately before it parks, so the park returns
/// `Refill` at every gap and the ONLY way either arm can end is the budget
/// check. Which budget ended it is COUNTED ([`BUDGET_ENDED`]), not timed; a
/// gather preempted past its budget still ends on that budget, so one gather
/// per arm is exact at any load. The budgets are compared with each other,
/// not with literals, so retuning either keeps this green.
#[test]
fn drain_more_nonblocking_hot_caps_a_continuous_stream_at_the_hot_budget() {
    use std::sync::atomic::AtomicUsize;
    const BUF: usize = 65_536;
    let mut ended = Vec::new();
    for (arm, interactive_pending, hot) in [
        ("hot", always_hot as fn() -> bool, true),
        ("cold", never_hot as fn() -> bool, false),
    ] {
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(rd, true).expect("nonblock read end");
        let chunk = [0xC3u8; 2048];
        // SAFETY: bounded write to this test's live pipe end.
        assert_eq!(
            unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
            chunk.len() as isize
        );
        let busy = AtomicUsize::new(1); // a parser mid-batch: the flood shape
        let mut buf = [0u8; BUF];
        let n = read(rd, &mut buf[..1024]);
        assert!(n > 0);
        let mut gaps = 0u32;
        let polls_before = super::BRIDGE_POLLS.with(std::cell::Cell::get);
        super::BUDGET_ENDED.with(|b| b.set(None));
        let filled = drain_more_nonblocking_with_idle_wait_after_gap(
            rd,
            &mut buf,
            n as usize,
            -1,
            -1,
            Some(&busy),
            interactive_pending,
            IDLE_POLL_DEFAULT_US,
            |_, _| {
                gaps += 1;
                // SAFETY: a one-byte write to this test's live pipe end; the
                // gather drains it, so the 64 KiB pipe never fills.
                assert_eq!(unsafe { libc::write(wr, b"x".as_ptr().cast(), 1) }, 1);
                false
            },
        );
        let polls = super::BRIDGE_POLLS.with(std::cell::Cell::get) - polls_before;
        let budget = super::BUDGET_ENDED.with(std::cell::Cell::take);
        close_fd(wr);
        close_fd(rd);
        assert!(
            filled < BUF,
            "{arm}: ended on a full buffer, which proves nothing"
        );
        // Hot bridges with the µs idle wait, cold with one bridge poll per gap.
        let expected = if hot { 0 } else { gaps };
        assert_eq!(
            polls, expected,
            "{arm}: {gaps} gaps took {polls} bridge polls"
        );
        ended.push(budget.unwrap_or_else(|| {
            panic!("{arm}: the gather must end on its own budget ({gaps} gaps)")
        }));
    }
    assert!(
        ended[0] < ended[1],
        "a pending keystroke must cut the batch on a SHORTER budget than a \
         flood's: (hot, cold) = {ended:?}"
    );
}

/// A saturated burst (at least one outq) is gathered into one batch, and EOF mid-drain
/// just ends the top-up.
#[test]
fn drain_more_nonblocking_gathers_saturated_burst_and_survives_eof() {
    let mut m = [0i32; 2];
    // SAFETY: valid 2-int out-array for pipe(2).
    assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
    let (rd, wr) = (m[0], m[1]);
    set_nonblocking(rd, true).expect("nonblock read end");
    let payload = [0xA5u8; 8192];
    // SAFETY: bounded write to this test's live pipe end (8 KiB fits the
    // default pipe buffer).
    assert_eq!(
        unsafe { libc::write(wr, payload.as_ptr().cast(), payload.len()) },
        payload.len() as isize
    );
    close_fd(wr); // EOF after the burst: the drain must keep what it read
    let mut buf = [0u8; 65_536];
    let n = read(rd, &mut buf[..1024]);
    assert!(n > 0);
    let filled = drain_more_nonblocking(
        rd,
        &mut buf,
        n as usize,
        -1,
        -1,
        None,
        never_hot,
        &mut |_, _| false,
    );
    assert_eq!(
        filled,
        payload.len(),
        "must gather the full saturated burst"
    );
    assert!(buf[..filled].iter().all(|&b| b == 0xA5));
    close_fd(rd);
}

/// Both parks a dry gap can take (the busy-parser bridge poll and the idle-parser µs
/// wait): a wake byte pending when the gather parks delivers what it has without
/// consuming the byte ([`read_or_wake`] owns the wake protocol). The idle wait is armed
/// past the hang detector, so only the wake can end it in time; the test budget
/// ([`BUDGET_FOR_TEST`]) ensures each park is reached (`at_gap` fires once, right
/// before it).
#[test]
fn drain_more_nonblocking_parks_yield_to_wake_and_leave_the_byte() {
    use std::sync::atomic::AtomicUsize;
    /// Past [`HANG_DETECTOR`]: a wait that ignored the wake ends only after it.
    const IDLE_WAIT_US: u32 = 90_000_000;
    super::BUDGET_FOR_TEST.with(|b| b.set(Some(HANG_DETECTOR)));
    for (park, in_flight) in [("bridge poll", 1usize), ("idle wait", 0)] {
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(rd, true).expect("nonblock read end");
        let payload = [0x9Cu8; 4096];
        // SAFETY: bounded write to this test's live pipe end.
        assert_eq!(
            unsafe { libc::write(wr, payload.as_ptr().cast(), payload.len()) },
            payload.len() as isize
        );
        // Non-blocking on both ends: a CONSUMED wake byte must fail the
        // assert below, not hang the read of it.
        let (wake_rd, wake_wr) = make_wake_pipe().expect("wake pipe");
        wake(wake_wr); // teardown byte queued BEFORE the gather parks
        let in_flight = AtomicUsize::new(in_flight);
        let mut buf = [0u8; 65_536];
        let n = read(rd, &mut buf[..1024]);
        assert!(n > 0);
        let mut gaps = 0u32;
        let t0 = std::time::Instant::now();
        let filled = drain_more_nonblocking_with_idle_wait_after_gap(
            rd,
            &mut buf,
            n as usize,
            wake_rd,
            -1,
            Some(&in_flight),
            never_hot,
            IDLE_WAIT_US,
            |_, _| {
                gaps += 1;
                false
            },
        );
        let took = t0.elapsed();
        let mut b = [0u8; 4];
        // SAFETY: bounded read from this test's live wake pipe end.
        let wn = unsafe { libc::read(wake_rd, b.as_mut_ptr().cast(), 4) };
        for fd in [rd, wr, wake_rd, wake_wr] {
            close_fd(fd);
        }
        assert_eq!(filled, payload.len(), "{park}: keeps the drained burst");
        assert_eq!(gaps, 1, "{park}: the gather reaches its park");
        assert_eq!(wn, 1, "{park}: the park must NOT consume the wake byte");
        assert!(
            took < HANG_DETECTOR,
            "{park}: a queued wake must end the park, not its timeout: {took:?}"
        );
    }
    super::BUDGET_FOR_TEST.with(|b| b.set(None));
}

/// The host fence: `at_gap` returning `true` delivers the batch without parking, and a
/// refill queued at that gap stays for the next read (aterm-gui cuts at a foreground
/// group change, and the refill belongs to the new group). Control: a fence that
/// answers `false` bridges the refill into the batch, the busy-parser continuation; at
/// idle knob 0 it also pins the idle cutoff's sign. Both run under the test budget
/// ([`BUDGET_FOR_TEST`]), so the fence is always asked.
#[test]
fn drain_more_nonblocking_fence_delivers_before_a_park() {
    use std::sync::atomic::AtomicUsize;
    super::BUDGET_FOR_TEST.with(|b| b.set(Some(std::time::Duration::from_secs(60))));
    let chunk = [0xC3u8; 2048];
    // (filled, gaps seen by the fence, bytes a follow-up read finds).
    let run = |cut: bool| -> (usize, u32, isize) {
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(rd, true).expect("nonblock read end");
        // SAFETY: bounded write to this test's live pipe end.
        assert_eq!(
            unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
            chunk.len() as isize
        );
        let busy = AtomicUsize::new(1); // parser busy ⇒ the bridge-poll park
        let mut buf = [0u8; 65_536];
        let n = read(rd, &mut buf[..1024]);
        assert!(n > 0);
        let mut gaps = 0u32;
        let mut seen_at = Vec::new();
        let filled = drain_more_nonblocking_with_idle_wait_after_gap(
            rd,
            &mut buf,
            n as usize,
            -1,
            -1,
            Some(&busy),
            never_hot,
            0,
            |f, _| {
                gaps += 1;
                seen_at.push(f);
                if gaps == 1 {
                    // SAFETY: bounded write to this test's live pipe end.
                    assert_eq!(
                        unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
                        chunk.len() as isize
                    );
                    return cut;
                }
                false
            },
        );
        assert_eq!(
            seen_at[0],
            chunk.len(),
            "the fence sees every byte read so far"
        );
        let mut rest = [0u8; 4096];
        let after = read(rd, &mut rest);
        close_fd(wr);
        close_fd(rd);
        (filled, gaps, after)
    };

    let (filled, gaps, after) = run(true);
    assert_eq!(filled, chunk.len(), "the fence cut the batch at the gap");
    assert_eq!(gaps, 1, "a fence that answered true is not asked again");
    assert_eq!(after, chunk.len() as isize, "the refill is still unread");

    let (filled, gaps, after) = run(false);
    assert_eq!(
        filled,
        2 * chunk.len(),
        "a false fence bridges the refill, as before"
    );
    assert!(gaps >= 1);
    assert!(after < 0, "nothing left behind (EAGAIN)");
    super::BUDGET_FOR_TEST.with(|b| b.set(None));
}

/// The process watch (card engine §3.3) is the fourth descriptor of the reader's poll and
/// joins both parks: readable, it is heard BEFORE the master, so the host drains it before
/// any byte read after it. `read_or_wake` answers `Proc` with data waiting; a park it ends
/// asks the fence again at the same offset (`at_gap(f, true)`), and the gather reads on
/// unless the fence delivers. Both parks: the bridge poll (parser busy) and the idle wait
/// (parser idle, armed past the hang detector so only a descriptor can end it).
#[test]
fn the_process_watch_is_heard_before_the_master_in_every_poll() {
    use std::sync::atomic::AtomicUsize;
    let pipe = || {
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        set_nonblocking(m[0], true).expect("nonblock read end");
        (m[0], m[1])
    };
    let put = |fd: i32, bytes: &[u8]| {
        // SAFETY: bounded write to this test's live pipe end.
        assert_eq!(
            unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) },
            bytes.len() as isize
        );
    };
    let take = |fd: i32| {
        let mut sink = [0u8; 64];
        read(fd, &mut sink)
    };

    // read_or_wake: the master and the watch both readable answers Proc, then Data.
    let (master, master_wr) = pipe();
    let (watch, watch_wr) = pipe();
    let (wake_rd, wake_wr) = make_wake_pipe().expect("wake pipe");
    put(master_wr, b"bytes");
    put(watch_wr, b"!");
    let mut buf = [0u8; 64];
    for wake in [wake_rd, -1] {
        assert!(matches!(
            read_or_wake(master, &mut buf, wake, -1, watch),
            ReadOutcome::Proc
        ));
    }
    assert_eq!(take(watch), 1);
    assert!(matches!(
        read_or_wake(master, &mut buf, wake_rd, -1, watch),
        ReadOutcome::Data(5)
    ));
    for fd in [master, master_wr, watch, watch_wr, wake_rd, wake_wr] {
        close_fd(fd);
    }

    super::BUDGET_FOR_TEST.with(|b| b.set(Some(HANG_DETECTOR)));
    let chunk = [0xC3u8; 2048];
    // (filled, the fence's calls, bytes a follow-up read of the master finds).
    let run = |park: &str, deliver_at_watch: bool| -> (usize, Vec<(usize, bool)>, isize) {
        let (rd, wr) = pipe();
        let (watch, watch_wr) = pipe();
        put(wr, &chunk);
        let busy = AtomicUsize::new(usize::from(park == "bridge"));
        let idle_wait_us = if park == "bridge" { 0 } else { u32::MAX };
        let mut buf = [0u8; 65_536];
        let n = read(rd, &mut buf[..1024]);
        assert!(n > 0);
        let mut calls = Vec::new();
        let filled = drain_more_nonblocking_with_idle_wait_after_gap(
            rd,
            &mut buf,
            n as usize,
            -1,
            watch,
            Some(&busy),
            never_hot,
            idle_wait_us,
            |f, proc| {
                calls.push((f, proc));
                if proc {
                    assert_eq!(take(watch), 1, "{park}: the host drains the watch");
                    return deliver_at_watch;
                }
                if calls.len() > 1 {
                    return true; // the next dry gap: nothing more is coming
                }
                // A refill and an event, both before the park.
                put(wr, &chunk);
                put(watch_wr, b"!");
                false
            },
        );
        let mut rest = [0u8; 4096];
        let after = read(rd, &mut rest);
        for fd in [rd, wr, watch, watch_wr] {
            close_fd(fd);
        }
        (filled, calls, after)
    };
    for park in ["bridge", "idle"] {
        let (filled, calls, after) = run(park, true);
        assert_eq!(
            calls,
            [(chunk.len(), false), (chunk.len(), true)],
            "{park}: the watch is heard at the park's offset, before the refill"
        );
        assert_eq!(
            filled,
            chunk.len(),
            "{park}: the fence delivered at the watch"
        );
        assert_eq!(
            after,
            chunk.len() as isize,
            "{park}: the refill is still unread"
        );

        let (filled, calls, _) = run(park, false);
        assert_eq!(
            calls,
            [
                (chunk.len(), false),
                (chunk.len(), true),
                (2 * chunk.len(), false)
            ]
        );
        assert_eq!(filled, 2 * chunk.len(), "{park}: the gather reads on");
    }
    super::BUDGET_FOR_TEST.with(|b| b.set(None));
}

/// R2x CLOSED: with a process watch, no batch ends past an `exec` the watch could hear.
/// * A batch under one outq ends at a poll of the watch and the master that stands in for
///   its dry `read`: a readable watch is heard there, at the batch's offset, before the
///   master's next bytes are read; a quiet one ends the batch with no system call of its
///   own.
/// * A batch that ends unparked past its last poll (its budget, a full buffer) asks the
///   watch once more (one `poll(2)`) and hears it at its end.
/// * A park whose wait runs out quiet proves the batch: no check of its own.
///
/// The checks are counted ([`super::WATCH_CHECKS`]), not timed.
#[test]
fn no_batch_ends_past_an_exec_the_watch_could_hear() {
    let pipe = || {
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        set_nonblocking(m[0], true).expect("nonblock read end");
        (m[0], m[1])
    };
    let put = |fd: i32, bytes: &[u8]| {
        // SAFETY: bounded write to this test's live pipe end.
        assert_eq!(
            unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) },
            bytes.len() as isize
        );
    };
    let checks = || super::WATCH_CHECKS.with(std::cell::Cell::get);
    // (filled, the fence's calls, the batch-end checks it made).
    let drain = |rd: i32, watch: i32, buf: &mut [u8], first: usize, budget| {
        super::BUDGET_FOR_TEST.with(|b| b.set(Some(budget)));
        let n = read(rd, &mut buf[..first]);
        assert!(n > 0);
        let before = checks();
        let mut calls = Vec::new();
        let busy = std::sync::atomic::AtomicUsize::new(1);
        let filled = drain_more_nonblocking_with_idle_wait_after_gap(
            rd,
            buf,
            n as usize,
            -1,
            watch,
            Some(&busy),
            never_hot,
            0,
            |f, heard| {
                calls.push((f, heard));
                if heard {
                    let mut sink = [0u8; 8];
                    assert_eq!(read(watch, &mut sink), 1, "the host drains the watch");
                }
                heard
            },
        );
        super::BUDGET_FOR_TEST.with(|b| b.set(None));
        (filled, calls, checks() - before)
    };

    // An interactive batch: the watch is heard in place of the dry read, before the
    // master's next bytes; quiet, the batch ends with no check of its own.
    let (rd, wr) = pipe();
    let (watch, watch_wr) = pipe();
    let mut buf = [0u8; 4096];
    put(wr, b"old");
    put(watch_wr, b"!");
    put(wr, b"new");
    let (filled, calls, own) = drain(rd, watch, &mut buf, 3, HANG_DETECTOR);
    assert_eq!(
        (filled, calls, own),
        (3, vec![(3, true)], 0),
        "heard at the small batch's end, before its next bytes"
    );
    let (filled, calls, own) = drain(rd, watch, &mut buf, 3, HANG_DETECTOR);
    assert_eq!(
        (filled, calls, own),
        (3, vec![], 0),
        "quiet: the batch ends at the poll"
    );
    for fd in [rd, wr, watch, watch_wr] {
        close_fd(fd);
    }

    // A batch its budget ends unparked, and one its buffer ends: asked once, heard at
    // the end.
    for (bytes, cap, budget) in [
        (2048, 4096, std::time::Duration::ZERO),
        (4096, 2048, HANG_DETECTOR),
    ] {
        let (rd, wr) = pipe();
        let (watch, watch_wr) = pipe();
        put(wr, &vec![b'x'; bytes]);
        put(watch_wr, b"!");
        let mut buf = vec![0u8; cap];
        let (filled, calls, own) = drain(rd, watch, &mut buf, 1024, budget);
        let end = bytes.min(cap);
        assert_eq!(
            (filled, calls, own),
            (end, vec![(end, true)], 1),
            "{bytes} bytes into {cap}: heard at the batch's end"
        );
        for fd in [rd, wr, watch, watch_wr] {
            close_fd(fd);
        }
    }

    // A parked batch whose wait runs out quiet is proven by the park: no check.
    let (rd, wr) = pipe();
    let (watch, watch_wr) = pipe();
    put(wr, &[b'y'; 2048]);
    let mut buf = [0u8; 4096];
    let (filled, calls, own) = drain(rd, watch, &mut buf, 1024, HANG_DETECTOR);
    assert_eq!(
        (filled, own),
        (2048, 0),
        "the park's quiet proves the batch"
    );
    assert_eq!(calls, [(2048, false)], "one park, its sample only");
    for fd in [rd, wr, watch, watch_wr] {
        close_fd(fd);
    }
}

/// The blocking fallback ([`drain_more_watched`]) hears the watch in each per-chunk poll,
/// at that chunk's offset and before its bytes; a batch that ends with no poll after its
/// last bytes (here, handed in full) asks the watch once more and hears it at its end.
#[test]
fn the_blocking_fallback_hears_the_watch_at_each_chunk() {
    let pipe = || {
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        (m[0], m[1])
    };
    let put = |fd: i32, bytes: &[u8]| {
        // SAFETY: bounded write to this test's live pipe end.
        assert_eq!(
            unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) },
            bytes.len() as isize
        );
    };
    let (rd, wr) = pipe();
    let (watch, watch_wr) = pipe();
    let hear = |heard: &mut Vec<usize>, f: usize| {
        heard.push(f);
        let mut sink = [0u8; 8];
        assert_eq!(read(watch, &mut sink), 1, "the host drains the watch");
        true
    };
    put(wr, b"old");
    let mut buf = [0u8; 4096];
    assert_eq!(read(rd, &mut buf), 3);
    put(watch_wr, b"!");
    put(wr, b"new");
    let mut heard = Vec::new();
    let filled = drain_more_watched(rd, &mut buf, 3, watch, &mut |f| hear(&mut heard, f));
    assert_eq!((filled, heard), (3, vec![3]), "heard before the next bytes");

    let mut full = [0u8; 3];
    assert_eq!(read(rd, &mut full), 3);
    put(watch_wr, b"!");
    let mut heard = Vec::new();
    let filled = drain_more_watched(rd, &mut full, 3, watch, &mut |f| hear(&mut heard, f));
    assert_eq!(
        (filled, heard),
        (3, vec![3]),
        "a full batch is asked at its end"
    );
    for fd in [rd, wr, watch, watch_wr] {
        close_fd(fd);
    }
}

/// Parser-idle hysteresis: an armed idle wait parks an idle parser's dry gap (the µs
/// idle wait, never the 1 ms bridge poll) until the writer's refill, written from
/// another thread while the gather is parked, which joins the same batch. The wait's
/// length is the only difference from the shipped default. The writer is released by
/// `at_gap` and waits 20 ms, so a gap that parked on the 1 ms bridge poll has expired
/// before the refill lands. The buffer holds exactly two chunks, so capacity, not EOF,
/// ends the batch. Counted, not timed: the idle wait is the minute hang detector, and
/// the gather runs under the test budget ([`BUDGET_FOR_TEST`]), so a preempted gather
/// still reaches the gap.
#[test]
fn drain_more_nonblocking_idle_wait_bridges_refill_into_same_batch() {
    use std::sync::atomic::AtomicUsize;
    /// The hang detector, as the idle wait: only a park blind to the refill waits it out.
    const IDLE_WAIT_US: u32 = 60_000_000;
    const CHUNK_LEN: usize = 2048;
    super::BUDGET_FOR_TEST.with(|b| b.set(Some(std::time::Duration::from_secs(60))));
    let mut m = [0i32; 2];
    // SAFETY: valid 2-int out-array for pipe(2).
    assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
    let (rd, wr) = (m[0], m[1]);
    set_nonblocking(rd, true).expect("nonblock read end");
    let chunk = [0xB4u8; CHUNK_LEN];
    // SAFETY: bounded write to this test's live pipe end.
    assert_eq!(
        unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
        chunk.len() as isize
    );
    let (release, released) = std::sync::mpsc::channel::<()>();
    let writer = std::thread::spawn(move || {
        // A gather that never reached its park drops `release` unsent.
        if released.recv().is_ok() {
            std::thread::sleep(std::time::Duration::from_millis(20));
            // SAFETY: bounded write to the pipe end this thread owns.
            unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) };
        }
        close_fd(wr);
    });
    let idle = AtomicUsize::new(0);
    let mut buf = [0u8; 2 * CHUNK_LEN];
    let n = read(rd, &mut buf[..1024]);
    assert!(n > 0);
    let mut gaps = 0u32;
    let filled = drain_more_nonblocking_with_idle_wait_after_gap(
        rd,
        &mut buf,
        n as usize,
        -1,
        -1,
        Some(&idle),
        never_hot,
        IDLE_WAIT_US,
        |_, _| {
            gaps += 1;
            if gaps == 1 {
                release.send(()).expect("the writer waits for its release");
            }
            false
        },
    );
    drop(release);
    writer.join().unwrap();
    close_fd(rd);
    super::BUDGET_FOR_TEST.with(|b| b.set(None));
    assert_eq!(
        (gaps, filled),
        (1, 2 * CHUNK_LEN),
        "an armed idle wait must park once and gather the refill into the same \
         batch: (dry gaps parked at, bytes gathered)"
    );
}

/// A signal handler that runs during the armed idle wait does not end it: the park
/// resumes for the time it has left, and the refill still joins the batch (Rosetta runs
/// one on every thread it translates, `idle_refill_wait`). A no-op `SIGURG` handler is
/// installed (a signal nothing else in this process sends), and once the gather reaches
/// its gap a second thread signals the gathering thread until the park counts one
/// ([`IDLE_WAIT_INTERRUPTS`]), then writes the refill. Deterministic: the park is the
/// minute hang detector, so the gather is parked until the refill, which comes only
/// after a signal landed inside the park. NEGATIVE CONTROL: a park that delivers on
/// `EINTR`, as it did before 2026-10-17, gathers one chunk, not two.
#[test]
fn a_signal_does_not_end_an_armed_idle_wait() {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};
    /// The hang detector, as the idle wait: only a park blind to the refill waits it out.
    const IDLE_WAIT_US: u32 = 60_000_000;
    const CHUNK_LEN: usize = 2048;
    extern "C" fn noop(_: libc::c_int) {}
    // Not among aterm-libc's declarations; both libcs export it.
    unsafe extern "C" {
        fn pthread_self() -> libc::pthread_t;
    }
    struct SignalHandler(libc::sigaction);
    impl Drop for SignalHandler {
        fn drop(&mut self) {
            // SAFETY: restore the action saved by this test, after its signaller joins.
            let restored = unsafe { libc::sigaction(libc::SIGURG, &self.0, std::ptr::null_mut()) };
            if !std::thread::panicking() {
                assert_eq!(restored, 0, "restore the SIGURG handler");
            }
        }
    }
    // SAFETY: initialize sigaction's plain fields and save the previous action;
    // the no-op handler has no non-signal-safe work and SA_RESTART stays off.
    let _handler = unsafe {
        let mut act: libc::sigaction = std::mem::zeroed();
        let mut previous: libc::sigaction = std::mem::zeroed();
        act.sa_sigaction = noop as *const () as usize;
        assert_eq!(libc::sigemptyset(std::ptr::addr_of_mut!(act.sa_mask)), 0);
        assert_eq!(
            libc::sigaction(libc::SIGURG, &act, &mut previous),
            0,
            "install the SIGURG handler"
        );
        SignalHandler(previous)
    };
    struct GatherState {
        budget: Option<Duration>,
        ended: Option<Duration>,
        interrupts: Option<Arc<AtomicUsize>>,
        _thread: std::marker::PhantomData<std::rc::Rc<()>>,
    }
    impl Drop for GatherState {
        fn drop(&mut self) {
            super::BUDGET_FOR_TEST.with(|b| b.set(self.budget));
            super::BUDGET_ENDED.with(|b| b.set(self.ended));
            super::IDLE_WAIT_INTERRUPTS.with(|c| *c.borrow_mut() = self.interrupts.take());
        }
    }
    // Create the thread-affine restorer before changing any TLS slot.
    let _state = GatherState {
        budget: super::BUDGET_FOR_TEST.with(std::cell::Cell::get),
        ended: super::BUDGET_ENDED.with(std::cell::Cell::get),
        interrupts: super::IDLE_WAIT_INTERRUPTS.with(|c| c.borrow().clone()),
        _thread: std::marker::PhantomData,
    };
    super::BUDGET_FOR_TEST.with(|b| b.set(Some(Duration::from_secs(60))));
    let interrupts = Arc::new(AtomicUsize::new(0));
    super::IDLE_WAIT_INTERRUPTS.with(|c| *c.borrow_mut() = Some(Arc::clone(&interrupts)));
    let (rd, wr) = make_wake_pipe().expect("owned CLOEXEC nonblocking pipe");
    // SAFETY: both fresh descriptors are transferred exactly once to their owners.
    let (rd, wr) = unsafe { (OwnedFd::from_raw_fd(rd), OwnedFd::from_raw_fd(wr)) };
    let chunk = [0xB5u8; CHUNK_LEN];
    // SAFETY: bounded write to this test's live nonblocking pipe end.
    assert_eq!(
        unsafe { libc::write(wr.as_raw_fd(), chunk.as_ptr().cast(), chunk.len()) },
        chunk.len() as isize
    );
    // SAFETY: names this thread, which the signaller guard keeps alive until join.
    let gatherer = unsafe { pthread_self() };
    struct Signaller {
        done: Arc<AtomicBool>,
        release: Option<std::sync::mpsc::Sender<()>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Signaller {
        fn stop(&mut self) -> std::thread::Result<()> {
            self.done.store(true, Ordering::Relaxed);
            drop(self.release.take());
            self.thread.take().map_or(Ok(()), |thread| thread.join())
        }
    }
    impl Drop for Signaller {
        fn drop(&mut self) {
            // Also releases a worker that has not reached its first recv on unwind.
            let _ = self.stop();
        }
    }
    let done = Arc::new(AtomicBool::new(false));
    let (release, released) = std::sync::mpsc::channel::<()>();
    let (counted, finished) = (Arc::clone(&interrupts), Arc::clone(&done));
    let thread = std::thread::spawn(move || {
        if released.recv().is_ok() {
            let hang = Instant::now() + Duration::from_secs(60);
            while counted.load(Ordering::Relaxed) == 0 && !finished.load(Ordering::Relaxed) {
                assert!(Instant::now() < hang, "no signal landed inside the park");
                // SAFETY: the gathering thread joins this worker even during unwind.
                unsafe { libc::pthread_kill(gatherer, libc::SIGURG) };
                std::thread::sleep(Duration::from_millis(1));
            }
            if !finished.load(Ordering::Relaxed) {
                // SAFETY: the moved owner keeps this fresh nonblocking pipe end live.
                assert_eq!(
                    unsafe { libc::write(wr.as_raw_fd(), chunk.as_ptr().cast(), chunk.len()) },
                    chunk.len() as isize,
                    "write the refill"
                );
            }
        }
    });
    let mut signaller = Signaller {
        done,
        release: Some(release),
        thread: Some(thread),
    };
    let idle = AtomicUsize::new(0);
    let mut buf = [0u8; 2 * CHUNK_LEN];
    let n = read(rd.as_raw_fd(), &mut buf[..1024]);
    assert!(n > 0);
    let mut gaps = 0u32;
    let filled = drain_more_nonblocking_with_idle_wait_after_gap(
        rd.as_raw_fd(),
        &mut buf,
        n as usize,
        -1,
        -1,
        Some(&idle),
        never_hot,
        IDLE_WAIT_US,
        |_, _| {
            gaps += 1;
            if gaps == 1 {
                signaller
                    .release
                    .as_ref()
                    .expect("release sender remains owned")
                    .send(())
                    .expect("the signaller waits for its release");
            }
            false
        },
    );
    signaller.stop().expect("join the signaller");
    assert!(
        interrupts.load(Ordering::Relaxed) >= 1,
        "NON-VACUITY: a signal interrupted the park"
    );
    assert_eq!(
        (gaps, filled),
        (1, 2 * CHUNK_LEN),
        "a signal must not end the armed idle wait: (dry gaps parked at, bytes gathered)"
    );
}

/// The errno an opener's error carries: its own, or its step payload's.
#[cfg(target_os = "macos")]
fn carried_errno(err: &io::Error) -> Option<libc::c_int> {
    err.raw_os_error().or_else(|| {
        err.get_ref()?
            .source()?
            .downcast_ref::<io::Error>()?
            .raw_os_error()
    })
}

/// The master open, against a scripted `posix_openpt` that fails as the kernel
/// does (`-1`, `errno` set — libsyscall's `cerror`) and then opens descriptor
/// 1000. XNU leaks a lost `/dev/ptmx` allocation race as errno -6
/// (`EREDRIVEOPEN`, seen as `open_pty_pair: Os { code: -6 }` under load): it
/// is redriven, and reported with its errno only once every redrive is lost.
/// A real limit is reported at once with its errno and kind intact, and
/// `EINVAL` (a kernel rejecting `O_CLOEXEC` there) is retried once without
/// that flag and nothing else.
#[cfg(target_os = "macos")]
#[test]
fn master_open_redrives_a_lost_race_and_reports_a_real_failure_at_once() {
    const OPENED: libc::c_int = 1000;
    let play = |errnos: &[libc::c_int]| {
        let mut flags_seen = Vec::new();
        let got = open_ptmx(|flags| {
            flags_seen.push(flags);
            let Some(&errno) = errnos.get(flags_seen.len() - 1) else {
                return OPENED;
            };
            // SAFETY: `__error` returns this thread's live `errno` slot.
            unsafe { *libc::__error() = errno };
            -1
        });
        (got, flags_seen)
    };
    let (got, seen) = play(&[XNU_EREDRIVEOPEN; 3]);
    assert_eq!((got.ok(), seen.len()), (Some(OPENED), 4), "3 lost races");
    let lost = [XNU_EREDRIVEOPEN; PTMX_REDRIVES as usize + 1];
    let (got, seen) = play(&lost);
    let err = got.expect_err("every race lost");
    assert_eq!(
        (carried_errno(&err), seen.len()),
        (Some(XNU_EREDRIVEOPEN), lost.len()),
        "every race lost: {err:?}"
    );
    for errno in [libc::ENXIO, libc::EMFILE, libc::ENFILE, libc::EAGAIN] {
        let (got, seen) = play(&[errno]);
        let err = got.expect_err("a real limit");
        assert_eq!(
            (carried_errno(&err), err.kind(), seen.len()),
            (Some(errno), io::Error::from_raw_os_error(errno).kind(), 1),
            "errno {errno}: reported at once, intact: {err:?}"
        );
    }
    let (got, seen) = play(&[libc::EINVAL]);
    let first = libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC;
    assert_eq!(got.ok(), Some(OPENED), "EINVAL");
    assert_eq!(seen, [first, first & !libc::O_CLOEXEC], "EINVAL: {seen:x?}");
}

/// `ptsname_r`'s error comes from its return value, never a stale `errno` (musl does
/// not set it); a -1 still reads `errno`. Played with `errno` left stale (`EBADF`) by a
/// failed `close`, since Apple's `ptsname_r` only answers -1.
#[test]
fn ptsname_r_errors_come_from_its_return_value() {
    let after_stale = |rc| {
        // SAFETY: closing no descriptor; it only sets `errno` (EBADF).
        assert_eq!(unsafe { libc::close(-1) }, -1);
        ptsname_r_error(rc).raw_os_error()
    };
    assert_eq!(
        after_stale(libc::ERANGE),
        Some(libc::ERANGE),
        "the returned number"
    );
    assert_eq!(after_stale(-1), Some(libc::EBADF), "-1 reads errno");
}

// A confinement failure in the child fails closed: an Untrusted `Cap<Sandbox>` fails
// `apply`, the child exits before exec, and the parent returns an error.
#[test]
fn sandbox_apply_failure_in_child_fails_closed_no_unconfined_shell() {
    // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    // A spawn cap that passes the parent's gate, a sandbox cap that fails the child's.
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let weak_sandbox = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Untrusted);

    let result = spawn_shell(
        24,
        80,
        &spawn_cap,
        &weak_sandbox,
        &[],
        None, // shell_override
        None, // shell_args
        None,
        None,
        None,
        None,
    );
    let err = result
        .expect_err("a sandbox confinement failure must surface as an error, NOT a master fd");
    assert_eq!(
        err.kind(),
        io::ErrorKind::PermissionDenied,
        "child sandbox failure must be reported as PermissionDenied, got: {err}",
    );
}

// ---- PTY pair creation: close-on-exec from birth ----

/// Read `fd`'s descriptor flags, failing the test if the fd is not live.
fn fd_flags(fd: libc::c_int, what: &str) -> libc::c_int {
    // SAFETY: `F_GETFD` only reads the descriptor-flag word of `fd`.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(
        flags >= 0,
        "F_GETFD must succeed on {what} (fd {fd}): {}",
        io::Error::last_os_error()
    );
    flags
}

/// `poll(fd, POLLIN)` for at most `budget`: the revents once any arrive, else 0. The
/// shape of `handoff_masters_closed`'s poll; with `events: 0` macOS reports nothing.
fn poll_revents(fd: libc::c_int, budget: std::time::Duration) -> libc::c_short {
    let deadline = std::time::Instant::now() + budget;
    loop {
        let mut p = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: a single valid pollfd with a 20 ms timeout.
        let rc = unsafe { libc::poll(&mut p, 1, 20) };
        if rc > 0 {
            return p.revents;
        }
        if std::time::Instant::now() >= deadline {
            return 0;
        }
    }
}

/// The pixel arithmetic's edges: half a geometry is not a geometry (a zero
/// axis zeroes BOTH fields, so no consumer can divide by it), and the
/// kernel's `u16` fields report the cap, never a wrapped small number.
#[test]
fn winsize_pixels_zeroes_half_a_geometry_and_saturates() {
    assert_eq!(winsize_pixels(24, 80, Some((0, 17))), (0, 0));
    assert_eq!(winsize_pixels(24, 80, Some((9, 0))), (0, 0));
    assert_eq!(
        winsize_pixels(u16::MAX, u16::MAX, Some((9, 17))),
        (u16::MAX, u16::MAX)
    );
}

/// THE DEFECT, as a deterministic regression: while a session's pty pair and
/// an exec-status channel are open in this (multi-threaded) parent, an
/// UNRELATED spawn that hands its child every descriptor without
/// `FD_CLOEXEC` ([`aterm_uds::spawnfd::transparent`]; a std `Command` child
/// gets fds 0-2 alone under Trust's SP-1 std) inherits NO end of either. An
/// inherited slave lets a stranger forge output into the session's terminal;
/// an inherited status write end suppresses a spawn's EOF, which parked the
/// session-opening thread for the stranger's whole life.
///
/// Deterministic: everything is open across the whole spawn, so anything
/// inheritable WOULD be inherited. NON-VACUITY: a plain `dup` of the slave
/// (POSIX clears `FD_CLOEXEC` on it) must be inherited AND must write onto
/// this master — proof that the probe sees a leak, and that the slave the
/// opener handed back is this master's own peer.
#[test]
fn an_unrelated_exec_inherits_no_end_of_a_live_pty_or_status_channel() {
    use std::time::Duration;
    let (master, slave) = open_pty_pair().expect("pty pair");
    let (status_rd, status_wr, _carrier) =
        open_exec_status_channel().expect("the exec-status channel must open");
    // SAFETY: `slave` is live; `dup` returns a new fd onto the same pty.
    let leaked = unsafe { libc::dup(slave) };
    assert!(leaked >= 0, "dup(slave) failed");
    // Each number is read from the shell's own table ([`aterm_spec::host::sh_held`]),
    // never by a path lookup of `/dev/fd/<n>`, which on Darwin reads a held descriptor as
    // absent while anything else looks the number up. The probes run BEFORE any
    // redirection, so no descriptor the shell opens for one outlives it at a probed
    // number; then one `exec` (which keeps no saved copies) points its stdio at
    // `/dev/null`, the report file and `/dev/null`, so nothing it prints is pty traffic.
    // The marks are OPENS of `/dev/fd/<n>`, which that race does not touch (it is in
    // `stat`'s read-back of the type; 0 of 200 000 opens failed under contention that
    // failed 2% of `stat`s), never `>&<n>`: POSIX promises only fds 0-9 in a redirection,
    // and dash (Debian's /bin/sh) aborts the script on `>&10` ("Bad fd number").
    let probed: String = [master, slave, status_rd, status_wr, leaked]
        .iter()
        .map(|&fd| {
            let held = aterm_spec::host::sh_held(fd);
            format!("{held} && got=\"$got got:{fd}\"; ")
        })
        .collect();
    let script = format!(
        "got=; {probed}\
         exec </dev/null >\"$1\" 2>/dev/null; \
         {{ printf 'LEAKMARK' >/dev/fd/{leaked}; }} 2>/dev/null; \
         {{ printf 'SLAVEMARK' >/dev/fd/{slave}; }} 2>/dev/null; \
         printf '%s end' \"$got\""
    );
    let path =
        std::env::temp_dir().join(format!("aterm-pty-unrelated-exec-{}", std::process::id()));
    let status = aterm_uds::spawnfd::transparent("/bin/sh")
        .args(["-c", &script, "sh"])
        .arg(&path)
        .status()
        .expect("the unrelated helper must run");
    let report = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    // Parse fd numbers, not substrings: "got:3" is a substring of "got:33".
    let inherited: Vec<libc::c_int> = report
        .split_whitespace()
        .filter_map(|tok| tok.strip_prefix("got:")?.parse().ok())
        .collect();

    // A hang detector for the control's mark: the helper has exited, so it
    // is already queued, and the loop leaves the moment it is read.
    set_nonblocking(master, true).expect("nonblocking master");
    let mut seen = Vec::new();
    // `None` is not judged here: the LEAKMARK assert names it.
    let _ = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(5),
        || {
            let mut buf = [0u8; 256];
            let n = read(master, &mut buf);
            if n > 0 {
                seen.extend_from_slice(&buf[..n as usize]);
            }
            (seen.len() >= b"LEAKMARK".len()).then_some(())
        },
    );
    let seen = String::from_utf8_lossy(&seen).into_owned();
    for fd in [leaked, slave, master, status_rd, status_wr] {
        close_fd(fd);
    }

    assert!(
        status.success() && report.ends_with("end"),
        "the unrelated helper did not run to completion: {status}; report: {report:?}"
    );
    assert!(
        inherited.contains(&leaked) && seen.contains("LEAKMARK"),
        "control: an fd WITHOUT FD_CLOEXEC is inherited and writes onto this \
         master; report: {report:?}, master saw: {seen:?}"
    );
    for (end, fd) in [
        ("pty master", master),
        ("pty slave", slave),
        ("status read end", status_rd),
        ("status write end", status_wr),
    ] {
        assert!(
            !inherited.contains(&fd),
            "an unrelated exec'd process inherited the {end} (fd {fd}); report: {report:?}"
        );
    }
}

// ---- the exec-status channel: the window, and the bound behind it ----

/// Build the racy carrier (`pipe(2)` + two `fcntl(F_SETFD)`) by hand: the real seam
/// cannot reach the leak, and the bound test must re-create it.
fn racy_status_pipe() -> (libc::c_int, libc::c_int) {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `pipe` fills exactly the two-element array it is given.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
    // SAFETY: both fds are live; `F_SETFD` only sets a flag word.
    unsafe {
        libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(fds[1], libc::F_SETFD, libc::FD_CLOEXEC);
    }
    assert!(
        fd_set_nonblocking(fds[0]),
        "the read end must be non-blocking, as the real carrier makes it"
    );
    (fds[0], fds[1])
}

/// THE HANG, bounded: with a stranger holding a duplicate of the write end,
/// the status wait ENDS on its budget as `NoVerdict` (which the seam turns
/// into a fail-closed error) instead of parking for the stranger's life.
///
/// The leak is re-created ON PURPOSE (the fix makes it unreachable through
/// the real seam): `pipe(2)`'s unflagged window, as a non-close-on-exec
/// duplicate a real spawn inherits ([`aterm_uds::spawnfd::transparent`]).
/// Non-vacuous by construction: every copy this process holds is closed, so
/// a stranger that did not hold the write end would let EOF answer `Execed`.
#[test]
fn a_stranger_holding_the_write_end_cannot_block_the_status_wait_forever() {
    const BUDGET: std::time::Duration = std::time::Duration::from_millis(400);
    let (rd, wr) = racy_status_pipe();
    // SAFETY: `wr` is live; `dup` copies it without FD_CLOEXEC.
    let leaked = unsafe { libc::dup(wr) };
    assert!(leaked >= 0, "dup(status_wr) failed");
    let mut stranger = aterm_uds::spawnfd::transparent("/bin/sh")
        .arg("-c")
        .arg("exec sleep 600 </dev/null >/dev/null 2>&1")
        .spawn()
        .expect("the unrelated helper must spawn");
    // Drop every copy this process holds, including the child's stand-in.
    close_fd(leaked);
    close_fd(wr);
    let status = wait_for_exec_status(rd, BUDGET);
    let _ = stranger.kill();
    let _ = stranger.wait();
    close_fd(rd);
    assert!(
        matches!(status, ExecStatus::NoVerdict),
        "a suppressed EOF must expire as `NoVerdict`, got {status:?}"
    );
}

/// [`wait_readable_briefly`] WAKES when a fifo's last writer closes. On macOS
/// `poll(2)` and `kqueue` never report a fifo's EOF, only `select(2)` does; a
/// wait built on either sleeps out its whole slice, which leaves every
/// correctness test green and adds up to [`EXEC_STATUS_SLICE`] to every
/// successful spawn. The slice here is the minute hang detector, so "woke
/// inside the slice" is exact against that regression at any load.
#[cfg(target_os = "macos")]
#[test]
fn wait_readable_briefly_wakes_when_a_fifos_last_writer_closes() {
    let (rd, wr) = open_exec_status_fifo().expect("the exec-status fifo must open");
    let closer = std::thread::spawn(move || {
        // SAFETY: the only write end; this thread owns it now.
        unsafe { libc::close(wr) };
    });
    let waited = std::time::Instant::now();
    wait_readable_briefly(rd, HANG_DETECTOR);
    let took = waited.elapsed();
    closer.join().expect("closer thread");
    let verdict = read_exec_status_now(rd);
    // SAFETY: the read end this test owns.
    unsafe { libc::close(rd) };
    assert!(
        matches!(verdict, Some(ExecStatus::Execed)),
        "the last writer closed: {verdict:?}"
    );
    assert!(
        took < HANG_DETECTOR,
        "the wait slept out its slice ({took:?}): blind to fifo EOF"
    );
}

/// With `$TMPDIR` unset the status FIFO goes in `/tmp`, not where `std::env::temp_dir`
/// would put it (std asks `confstr`, which [`exec_status_fifo_dir`] avoids); a set one
/// is used as given, ending in one `/`.
#[cfg(target_os = "macos")]
#[test]
fn an_unset_tmpdir_puts_the_status_fifo_in_tmp() {
    let dir = |tmpdir: Option<&str>| {
        String::from_utf8(fifo_dir_in(tmpdir.map(Into::into))).expect("UTF-8 in, UTF-8 out")
    };
    assert_eq!(dir(None), "/tmp/");
    assert_eq!(dir(Some("/var/x")), "/var/x/");
    assert_eq!(dir(Some("/var/x/")), "/var/x/");
    assert_eq!(dir(Some("")), "/");
}

/// One real spawn through the seam, checked from both sides. The child is a
/// probe that reports, then parks:
///
/// * its stdio is the slave, inherited across `execve`: `login_tty`'s `dup2`s
///   clear the slave's close-on-exec flag on 0/1/2, without which the exec'd
///   program starts with no terminal and no byte reaches the master;
/// * that slave is its CONTROLLING terminal, and THIS session's, named
///   exactly (`ctty=y` alone would pass on the harness's inherited one);
/// * it sees the parent-applied winsize (not 24x80, so a kernel default
///   cannot match it), and the master answers the real pixel geometry from
///   the first `TIOCGWINSZ` — hard-coded 0 once, so chafa, timg, viu and
///   libsixel got nothing;
/// * it holds NO descriptor but its stdio: an unflagged pipe end parked at
///   100 or above (the state the GUI's Metal driver leaves its shader-cache
///   files in, measured leaking into a login shell) is closed in the child;
/// * it is a session leader, pid == pgid == sid, and the pty's foreground
///   group — what `hangup` (killpg), `record_adopted_shell` and the GUI's
///   `tcgetpgrp` depend on.
///
/// From the parent: the master is close-on-exec (else a later session's
/// shell inherits it, a cross-session injection channel) and carries IUTF8 +
/// B230400; the exec-status channel came from the ATOMIC carrier (a fallback
/// to `pipe(2)` + `fcntl` passes everything else here with the window open);
/// and the master reports hangup once the session dies, so the parent kept
/// no copy of the slave — which would block `handoff_masters_closed` forever.
#[test]
fn a_spawned_child_gets_its_own_ctty_our_winsize_and_no_other_descriptor() {
    spawn_probe_checks_the_child(None);
}

/// The same child, from a kit the pool opened ahead ([`fill_kits`]): the spawn
/// takes it and gives it this spawn's winsize, pixels included, before the
/// fork; the termios is the spawn's; the kit still waiting in the pool reaches
/// the child as no descriptor; and the spent channel end waits in the pool for
/// the next fill, which closes it and opens the replacement. NEGATIVE CONTROL:
/// a pool never filled is off, and the spawn takes nothing from it.
#[test]
fn a_spawn_takes_a_ready_kit_and_gives_it_its_winsize() {
    let pool: &'static std::sync::Mutex<KitPool> =
        Box::leak(Box::new(std::sync::Mutex::new(KitPool::new())));
    TEST_KITS.with(|kits| kits.set(Some(pool)));
    let off = pool.lock().unwrap().take();
    assert!(off.is_none(), "a pool never filled lends nothing");
    assert_eq!(fill_kits(pool, 2).expect("two kits open"), 2);
    assert_eq!(ready_spawn_kits(), 2, "the census reads the kits waiting");
    let waiting = {
        let kits = pool.lock().unwrap();
        let first = &kits.ready[0];
        vec![first.master, first.slave, first.status_rd, first.status_wr]
    };
    spawn_probe_checks_the_child(Some(&waiting));
    {
        let kits = pool.lock().unwrap();
        assert_eq!(kits.ready.len(), 1, "the spawn took one kit");
        assert_eq!(kits.spent.len(), 1, "its spent channel end waits");
    }
    assert_eq!(ready_spawn_kits(), 1, "a taken kit leaves the census");
    assert_eq!(fill_kits(pool, 2).expect("the refill"), 1);
    let mut kits = pool.lock().unwrap();
    assert!(kits.spent.is_empty(), "the fill closed the spent end");
    assert_eq!(kits.ready.len(), 2);
    // The leaked pool's kits close here, not at the test process's exit.
    kits.ready.clear();
    drop(kits);
    TEST_KITS.with(|kits| kits.set(None));
}

/// The spent-end hold is bounded: past [`SPENT_MAX`], and with the pool off,
/// the caller closes its own.
#[test]
fn a_full_or_off_pool_hands_a_spent_end_back() {
    let mut pool = KitPool::new();
    assert_eq!(pool.keep_spent(-1), Some(-1), "off: closed by the caller");
    pool.on = true;
    let held: Vec<libc::c_int> = (0..SPENT_MAX as libc::c_int).map(|n| -2 - n).collect();
    for fd in &held {
        assert_eq!(pool.keep_spent(*fd), None);
    }
    assert_eq!(
        pool.keep_spent(-100),
        Some(-100),
        "full: closed by the caller"
    );
    // The fakes are not descriptors: nothing for `Drop` to close.
    pool.spent.clear();
}

/// [`a_spawned_child_gets_its_own_ctty_our_winsize_and_no_other_descriptor`]'s
/// spawn and checks, with `absent` naming descriptors the child must not hold
/// besides the parked pipe copy.
fn spawn_probe_checks_the_child(absent: Option<&[libc::c_int]>) {
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let (pipe_rd, pipe_wr) = make_wake_pipe().expect("a pipe");
    // F_DUPFD takes the lowest FREE number from 100 up, so no sibling test's
    // descriptor is disturbed, and its copy carries NO close-on-exec flag.
    // SAFETY: `fcntl(F_DUPFD)` on a descriptor this test owns.
    let parked = unsafe { libc::fcntl(pipe_rd, libc::F_DUPFD, 100) };
    assert!(parked >= 100, "park a copy of the pipe: {parked}");
    assert_eq!(
        fd_flags(parked, "the parked copy") & libc::FD_CLOEXEC,
        0,
        "PRECONDITION: fd {parked} is inheritable in the parent"
    );
    // Read from the shell's own table ([`aterm_spec::host::sh_held`]), as the
    // unrelated-exec probe reads, never by `[ -e /dev/fd/N ]` (on Darwin a held number
    // reads absent while anything else looks it up, which would pass a leak) and never
    // by a `>&N` redirection: dash, /bin/sh on Debian and Ubuntu, reads only one digit
    // after `>&`, refuses the whole line as a syntax error and printed nothing here (the
    // Linux lane, 2026-10-16).
    let parked_held = aterm_spec::host::sh_held(parked);
    let others: String = absent
        .unwrap_or_default()
        .iter()
        .map(|&fd| {
            let held = aterm_spec::host::sh_held(fd);
            format!("{held} && p=\"$p+{fd}\"; ")
        })
        .collect();
    let script = format!(
        "s=n; [ -t 0 ] && [ -t 1 ] && [ -t 2 ] && s=y; \
         c=n; {{ : < /dev/tty; }} 2>/dev/null && c=y; \
         p=closed; {parked_held} && p=open; \
         {others}\
         printf 'PROBE stdio=%s ctty=%s size=[%s] tty=[%s] parked=%s\\n' \
           \"$s\" \"$c\" \"$(stty size)\" \"$(tty)\" \"$p\"; \
         exec sleep 30"
    );
    let exec: Vec<std::ffi::OsString> = vec!["/bin/sh".into(), "-c".into(), script.into()];
    let racy_before = RACY_EXEC_STATUS_CARRIERS.load(Ordering::Relaxed);
    let sh = spawn_shell_with_pid_cell_px(
        41,
        137,
        &spawn_cap,
        &sandbox_cap,
        &[],
        None, // shell_override
        None, // shell_args
        None, // argv_override
        Some(&exec),
        None, // cwd
        None, // sandbox_wrap
        aterm_sandbox::Limits::inherit(),
        Some((9, 17)),
    )
    .expect("the probe command must spawn");
    let racy_after = RACY_EXEC_STATUS_CARRIERS.load(Ordering::Relaxed);
    for fd in [parked, pipe_rd, pipe_wr] {
        close_fd(fd);
    }

    set_nonblocking(sh.master, true).expect("nonblocking master");
    let mut seen = Vec::new();
    // A hang detector for the probe's line; the child parks in `sleep` only
    // after printing it.
    let deadline = std::time::Instant::now() + HANG_DETECTOR;
    while std::time::Instant::now() < deadline && !seen.ends_with(b"\n") {
        let mut buf = [0u8; 512];
        let n = read(sh.master, &mut buf);
        if n > 0 {
            seen.extend_from_slice(&buf[..n as usize]);
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let seen = String::from_utf8_lossy(&seen).into_owned();
    let want_tty = pts_name(sh.master)
        .expect("the master must still resolve its pts name")
        .to_string_lossy()
        .into_owned();
    // SAFETY: read-only identity probes on a live child pid / our master fd.
    let identity = unsafe {
        (
            libc::getpgid(sh.pid),
            libc::getsid(sh.pid),
            libc::tcgetpgrp(sh.master),
        )
    };
    // SAFETY: `TIOCGWINSZ` and `tcgetattr` fill zeroed out-params from the
    // live master; both structs are plain integer fields.
    let (ws, termios) = unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        assert_eq!(libc::ioctl(sh.master, libc::TIOCGWINSZ, &mut ws), 0);
        let mut t: libc::termios = std::mem::zeroed();
        assert_eq!(libc::tcgetattr(sh.master, &mut t), 0);
        (ws, t)
    };
    let master_flags = fd_flags(sh.master, "the spawned master");

    hangup(sh.pid);
    reap(sh.pid);
    // A hang detector: the poll returns the moment the hangup is reported.
    let after_death = poll_revents(sh.master, HANG_DETECTOR);
    close_fd(sh.master);

    for (want, what) in [
        (
            "stdio=y",
            "fds 0/1/2 are ttys: dup2 cleared FD_CLOEXEC on the slave copies",
        ),
        (
            "ctty=y",
            "a controlling terminal: login_tty's setsid + TIOCSCTTY",
        ),
        (
            "size=[41 137]",
            "the parent-applied winsize is live before the child reads it",
        ),
        (
            "parked=closed",
            "the child holds no descriptor it was never given",
        ),
    ] {
        assert!(seen.contains(want), "{what}. Saw: {seen:?}");
    }
    assert!(
        !seen.contains("parked=closed+"),
        "the child holds a descriptor of a kit still in the pool. Saw: {seen:?}"
    );
    assert!(
        seen.contains(&format!("tty=[{want_tty}]")),
        "the child's controlling terminal must be THIS session's slave \
         ({want_tty}). Saw: {seen:?}"
    );
    assert_eq!(
        identity,
        (sh.pid, sh.pid, sh.pid),
        "(pgid, sid, tcgetpgrp) of a session leader in the pty's foreground"
    );
    assert_eq!(
        (ws.ws_row, ws.ws_col, ws.ws_xpixel, ws.ws_ypixel),
        (41, 137, 137 * 9, 41 * 17),
        "the child's FIRST winsize carries the real pixel geometry"
    );
    assert_ne!(
        master_flags & libc::FD_CLOEXEC,
        0,
        "the master must be close-on-exec so it never leaks into a later session's shell"
    );
    assert_ne!(
        termios.c_iflag & libc::IUTF8,
        0,
        "the spawned pty carries IUTF8"
    );
    // SAFETY: `cfgetospeed` only reads the termios.
    assert_eq!(unsafe { libc::cfgetospeed(&termios) }, libc::B230400);
    assert_eq!(
        racy_before, racy_after,
        "the spawn fell back to the RACY `pipe(2)`+fcntl status carrier"
    );
    assert_ne!(
        after_death & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL),
        0,
        "the master never reported hangup after the session died: the parent \
         still holds a copy of the pty SLAVE"
    );
}

// ---- spawn termios (kernel defaults + IUTF8/B230400 deltas) ----

#[test]
fn spawn_termios_deltas_are_exactly_iutf8_and_speed() {
    let d = kernel_default_termios().expect("probe");
    let t = build_spawn_termios(d);
    // The two documented deltas...
    assert_ne!(
        t.c_iflag & libc::IUTF8,
        0,
        "IUTF8 must be set for the UTF-8 child locale"
    );
    // SAFETY: cfget*speed only read the valid termios.
    unsafe {
        assert_eq!(libc::cfgetospeed(&t), libc::B230400);
        assert_eq!(libc::cfgetispeed(&t), libc::B230400);
    }
    // ...and nothing else: flag-for-flag identical to the kernel defaults.
    assert_eq!(t.c_iflag & !libc::IUTF8, d.c_iflag & !libc::IUTF8);
    assert_eq!(t.c_oflag, d.c_oflag);
    assert_eq!(t.c_lflag, d.c_lflag);
    assert_eq!(t.c_cc, d.c_cc);
    // macOS keeps speed in c_ispeed/c_ospeed, so c_cflag is untouched there (Linux
    // stores CBAUD bits in it).
    #[cfg(target_os = "macos")]
    assert_eq!(t.c_cflag, d.c_cflag);
}

/// The kernel's default signal characters — `^C`, `^\`, `^Z` — which a
/// fresh pty's slave carries (the spawn seam copies the kernel defaults).
#[cfg(target_os = "macos")]
const DEFAULT_SIGNALS: crate::TtySignals = crate::TtySignals {
    intr: Some(0x03),
    quit: Some(0x1c),
    susp: Some(0x1a),
};

/// Edit the termios of `slave`, this test's live pty slave, in place.
#[cfg(target_os = "macos")]
fn edit_termios(slave: i32, edit: impl FnOnce(&mut libc::termios)) {
    // SAFETY: all-zeros is a valid termios, overwritten by `tcgetattr`.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `slave` is this test's live pty slave; `t` is an out-param.
    assert_eq!(unsafe { libc::tcgetattr(slave, &mut t) }, 0);
    edit(&mut t);
    // SAFETY: `slave` is live; `t` is derived from its own termios.
    assert_eq!(unsafe { libc::tcsetattr(slave, libc::TCSANOW, &t) }, 0);
}

/// A pty pair whose slave the test itself makes raw (`cfmakeraw`, `VMIN = 1`,
/// `VTIME = 0`): no child, no race. From the close-on-exec opener, never
/// `openpty`, whose inheritable ends a sibling test's spawn would keep.
#[cfg(target_os = "macos")]
fn raw_pair() -> (i32, i32) {
    let (master, slave) = open_pty_pair().expect("pty pair");
    edit_termios(slave, |t| {
        // SAFETY: `t` is a live, initialised termios.
        unsafe { libc::cfmakeraw(t) };
        t.c_cc[libc::VMIN] = 1;
        t.c_cc[libc::VTIME] = 0;
    });
    (master, slave)
}

/// Write `bytes` into the pty (the master end), all of it, as a key would.
#[cfg(target_os = "macos")]
fn type_into(master: i32, bytes: &[u8]) {
    // SAFETY: a bounded write of a live slice to this test's live master.
    let wrote = unsafe { libc::write(master, bytes.as_ptr().cast(), bytes.len()) };
    assert_eq!(
        wrote,
        bytes.len() as isize,
        "a short write into a near-empty pty"
    );
}

/// Read exactly `n` bytes from `fd` (the program reading its input, or aterm
/// reading the program's output), returning them.
#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
fn close_pair(master: i32, slave: i32) {
    // SAFETY: both fds are this test's and still open.
    unsafe {
        libc::close(slave);
        libc::close(master);
    }
}

/// THE FLUSH ON THE MASTER: `flush_input_queue(master)` discards exactly the
/// slave's unread INPUT and reports its count — a `\r` and the `\x1b[B`
/// behind it, 4 bytes — and leaves the program's unread OUTPUT alone (a
/// `TCIOFLUSH` would drop what the program drew). The output count is the
/// hand-spelled `TIOCOUTQ`: 5 drawn bytes, then 0 once the master read them.
#[test]
#[cfg(target_os = "macos")]
fn flush_input_queue_discards_the_unread_input_and_nothing_else() {
    let (master, slave) = raw_pair();
    type_into(master, b"\r\x1b[B");
    type_into(slave, b"drawn");
    assert_eq!(flush_input_queue(master), Some(4), "the discarded count");
    assert_eq!(input_queue_len(master), Some(0), "the input is gone");
    assert_eq!(output_queue_len(master), Some(5), "the output is not");
    assert_eq!(read_exactly(master, 5), b"drawn");
    assert_eq!(output_queue_len(master), Some(0), "aterm read it");
    type_into(master, b"x");
    assert_eq!(read_exactly(slave, 1), b"x", "the queue still works");
    close_pair(master, slave);
}

/// The signal characters [`tty_echo`] reports are the ones the line
/// discipline acts on: none without ISIG (raw), the kernel defaults with it
/// (cbreak: readline between keys, `watch`, `htop`), a remapped `VINTR` as
/// remapped, and a disabled one (`_POSIX_VDISABLE`, 0xff) as `None` — never
/// as a byte the unread-input gate would let through as a signal.
#[test]
#[cfg(target_os = "macos")]
fn tty_echo_reads_the_signal_characters_isig_acts_on() {
    let (master, slave) = raw_pair();
    assert_eq!(tty_echo(master).map(|e| e.signals), Some(None), "raw");
    edit_termios(slave, |t| t.c_lflag |= libc::ISIG);
    let echo = tty_echo(master).expect("tcgetattr(master)");
    assert!(!echo.canonical);
    assert_eq!(echo.signals, Some(DEFAULT_SIGNALS), "cbreak");
    edit_termios(slave, |t| {
        t.c_cc[libc::VINTR] = 0x07;
        t.c_cc[libc::VQUIT] = 0xff;
    });
    let signals = tty_echo(master).and_then(|e| e.signals).expect("ISIG");
    assert_eq!(signals.intr, Some(0x07));
    assert_eq!(signals.quit, None, "_POSIX_VDISABLE signals nothing");
    assert!(signals.signals(0x07) && !signals.signals(0x03) && !signals.signals(0xff));
    close_pair(master, slave);
}

/// [`tty_passes_every_byte`] answers `true` only with ICANON, ISIG, IEXTEN,
/// IXON and IGNCR all clear (`cfmakeraw`): each one lets the line discipline
/// consume a written byte with nothing reading the slave (`^C`, `^S`, `^O`,
/// `\r`, a partial line), so a shrinking queue there is no read.
#[test]
#[cfg(target_os = "macos")]
fn tty_passes_every_byte_only_in_raw_mode() {
    let (master, slave) = raw_pair();
    assert_eq!(tty_passes_every_byte(master), Some(true), "cfmakeraw");
    close_pair(master, slave);
    for (name, lflag, iflag) in [
        ("ICANON", libc::ICANON, 0),
        ("ISIG", libc::ISIG, 0),
        ("IXON", 0, libc::IXON),
        ("IEXTEN", libc::IEXTEN, 0),
        ("IGNCR", 0, libc::IGNCR),
    ] {
        let (master, slave) = raw_pair();
        edit_termios(slave, |t| {
            t.c_lflag |= lflag;
            t.c_iflag |= iflag;
        });
        assert_eq!(tty_passes_every_byte(master), Some(false), "{name}");
        close_pair(master, slave);
    }
}

/// A [`dup_fd`] copy is close-on-exec, though POSIX `dup(2)` clears the flag
/// on every copy it makes: the spill drainer holds its duplicate of a master
/// for as long as a wedged shell keeps the queue spilling, and an inheritable
/// copy would hand every process that execs meanwhile a writable descriptor
/// onto that session's terminal.
#[test]
fn a_dup_fd_copy_is_close_on_exec() {
    let (rd, wr) = make_wake_pipe().expect("a pipe");
    let dup = dup_fd(rd).expect("dup_fd");
    let flags = fd_flags(std::os::fd::AsRawFd::as_raw_fd(&dup), "the dup_fd copy");
    drop(dup);
    close_fd(rd);
    close_fd(wr);
    assert_ne!(
        flags & libc::FD_CLOEXEC,
        0,
        "a dup_fd copy must be close-on-exec (F_DUPFD_CLOEXEC, not dup)"
    );
}

// ---- locale resolution: the child always runs under a UTF-8 LC_CTYPE ----

#[test]
fn is_utf8_locale_classifies_codeset() {
    // UTF-8 codesets in every spelling/case, with and without an @modifier.
    for ok in [
        "en_US.UTF-8",
        "en_US.UTF8",
        "en_US.utf-8",
        "en_US.utf8",
        "de_DE.UTF-8@euro",
    ] {
        assert!(is_utf8_locale(ok), "{ok} should be UTF-8");
    }
    // No codeset, or a non-UTF-8 one, is not UTF-8.
    for no in [
        "C",
        "POSIX",
        "en_US",
        "en_US.ISO8859-1",
        "",
        "fr_FR.ISO8859-15@euro",
    ] {
        assert!(!is_utf8_locale(no), "{no} should NOT be UTF-8");
    }
}

/// Every inherited-locale shape, through `resolve_spawn_locale` and `build_child_env`,
/// gives the child a UTF-8 `LC_CTYPE`, judged by an oracle independent of
/// `is_utf8_locale`.
#[test]
fn spawn_locale_conformance_child_always_utf8_ctype() {
    use std::ffi::OsString;

    #[derive(Clone, Copy)]
    enum Cls {
        Unset,
        Empty,
        NonUtf8,
        Utf8,
    }
    let val = |c: Cls| match c {
        Cls::Unset => None,
        Cls::Empty => Some(""),
        Cls::NonUtf8 => Some("C"),
        Cls::Utf8 => Some("en_US.UTF-8"),
    };
    // A codeset check independent of `is_utf8_locale`.
    let looks_utf8 = |s: &str| {
        let lo = s.to_ascii_lowercase();
        lo.ends_with(".utf-8") || lo.ends_with(".utf8")
    };
    // Effective LC_CTYPE of a child env (POSIX precedence, empty == unset).
    let child_ctype_utf8 = |env: &[(OsString, OsString)]| -> bool {
        let get = |k: &str| {
            env.iter()
                .find(|(ek, _)| ek.to_str() == Some(k))
                .map(|(_, v)| v.to_string_lossy().into_owned())
                .filter(|s| !s.is_empty())
        };
        match get("LC_ALL")
            .or_else(|| get("LC_CTYPE"))
            .or_else(|| get("LANG"))
        {
            None => false, // "C" default
            Some(loc) => looks_utf8(&loc),
        }
    };

    let classes = [Cls::Unset, Cls::Empty, Cls::NonUtf8, Cls::Utf8];
    for &a in &classes {
        for &c in &classes {
            for &l in &classes {
                let mut inherited: Vec<(OsString, OsString)> =
                    vec![(OsString::from("PATH"), OsString::from("/usr/bin"))];
                for (k, cl) in [("LC_ALL", a), ("LC_CTYPE", c), ("LANG", l)] {
                    if let Some(v) = val(cl) {
                        inherited.push((OsString::from(k), OsString::from(v)));
                    }
                }

                let overrides = resolve_spawn_locale(val(a), val(c), val(l));

                let ne = |cl: Cls| val(cl).filter(|s| !s.is_empty());
                let orig_utf8 = ne(a).or(ne(c)).or(ne(l)).map(looks_utf8).unwrap_or(false);

                // Overrides are empty iff the inherited locale was already UTF-8.
                assert_eq!(
                    overrides.is_empty(),
                    orig_utf8,
                    "overrides emptiness must track already-UTF-8 for (LC_ALL,LC_CTYPE,LANG)=({:?},{:?},{:?})",
                    val(a),
                    val(c),
                    val(l)
                );

                if matches!(a, Cls::NonUtf8) {
                    assert!(
                        overrides.iter().any(|(k, v)| k == "LC_ALL" && v.is_empty()),
                        "non-UTF-8 LC_ALL must be neutralized; got {overrides:?}"
                    );
                }

                // The child's effective LC_CTYPE is UTF-8.
                let child = build_child_env(inherited.into_iter(), &overrides);
                assert!(
                    child_ctype_utf8(&child),
                    "child LC_CTYPE NOT UTF-8 for inherited (LC_ALL,LC_CTYPE,LANG)=({:?},{:?},{:?}); env={:?}",
                    val(a),
                    val(c),
                    val(l),
                    child
                );
            }
        }
    }
}

// ---- fail-closed spawn: under-tier capability ----

// An Untrusted `Cap<Spawn>` is `PermissionDenied` by the parent's gate, before any fork.
#[test]
fn under_tier_spawn_cap_is_denied_before_forking() {
    // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let weak_spawn = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Untrusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);

    let result = spawn_shell(
        24,
        80,
        &weak_spawn,
        &sandbox_cap,
        &[],
        None, // shell_override
        None, // shell_args
        None,
        None,
        None,
        None,
    );
    let err = result.expect_err("an under-tier spawn cap must be denied, not spawn a shell");
    assert_eq!(
        err.kind(),
        io::ErrorKind::PermissionDenied,
        "under-tier spawn must be PermissionDenied, got: {err}",
    );
}

// ---- fail-closed spawn: a child that cannot exec ----

// A `-e` program that does not exist makes the child's `execve` fail: the parent reads
// b'E' and returns an error naming the program, never a master. (A bogus `$SHELL` would
// need an env mutation, a data race in the threaded harness.)
#[test]
fn bogus_exec_command_takes_child_exec_failure_path() {
    // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);

    let bogus = vec![std::ffi::OsString::from(
        "/nonexistent/aterm-pty-no-such-prog-xyz",
    )];
    let result = spawn_shell(
        24,
        80,
        &spawn_cap,
        &sandbox_cap,
        &[],
        None, // shell_override
        None, // shell_args
        None,
        Some(&bogus),
        None,
        None,
    );
    let err = result.expect_err("a child that cannot exec must surface an error, not a master fd");
    assert_eq!(
        err.kind(),
        io::ErrorKind::Other,
        "exec failure before exec must be reported as Other, got: {err}",
    );
}

/// A session's shell with no argv of its own is a login shell (`-zsh`) on macOS
/// and an interactive non-login one (`zsh`) on every other Unix: each platform's
/// terminal convention.
#[test]
fn the_default_shell_argv0_is_login_on_macos_only() {
    let argv0 = default_shell_argv0(std::ffi::OsStr::new("zsh"));
    if cfg!(target_os = "macos") {
        assert_eq!(argv0, "-zsh");
    } else {
        assert_eq!(argv0, "zsh");
    }
}

// ---- OS-sandbox wrap (sandbox_wrap) ----

// A missing wrapper is `NotFound`, so the spawn never forks an unsandboxed shell.
#[test]
fn build_sandbox_wrap_fails_closed_when_wrapper_missing() {
    let prog = CString::new("/bin/zsh").unwrap();
    let argv = vec![CString::new("-zsh").unwrap()];
    let err = build_sandbox_wrap(
        "/nonexistent/aterm-no-such-sandbox-exec",
        aterm_containment::BASE_PROFILE,
        &prog,
        &argv,
    )
    .expect_err("a missing wrapper must fail closed, not silently skip the sandbox");
    assert_eq!(
        err.kind(),
        io::ErrorKind::NotFound,
        "fail-closed kind: {err}"
    );
}

// The wrapped argv: sandbox-exec, -p, <profile>, <program path> (replacing the login
// argv[0]), then the original args after argv[0].
#[cfg(target_os = "macos")]
#[test]
fn build_sandbox_wrap_produces_correct_argv_shape() {
    let prog = CString::new("/bin/zsh").unwrap();
    let argv = vec![
        CString::new("-zsh").unwrap(),
        CString::new("--rcfile").unwrap(),
        CString::new("/tmp/rc").unwrap(),
    ];
    let (target, wrapped) = build_sandbox_wrap(
        SANDBOX_EXEC_PATH,
        aterm_containment::BASE_PROFILE,
        &prog,
        &argv,
    )
    .expect("wrapper present → build succeeds");
    assert_eq!(target.to_str().unwrap(), SANDBOX_EXEC_PATH);
    let got: Vec<&str> = wrapped.iter().map(|c| c.to_str().unwrap()).collect();
    assert_eq!(
        got,
        vec![
            "sandbox-exec",
            "-p",
            aterm_containment::BASE_PROFILE,
            "/bin/zsh", // argv[0] replaced by the program PATH
            "--rcfile", // real args carried through verbatim …
            "/tmp/rc",  // …
        ],
        "wrapped argv shape must be sandbox-exec -p <sbpl> <prog> <orig argv[1..]>",
    );
}

// The wrap is APPLIED by the real spawn, not just built: `-e nc` against a
// live loopback listener in this parent, spawned through `spawn_shell` with
// the `(deny network*)` profile, cannot connect. The control is the identical
// spawn without the wrap, which does (nc says so on its stderr, the pty) — so
// a wrap the seam silently dropped reads as the control does, and a broken
// probe (no nc, no listener) fails the control instead of passing.
#[cfg(target_os = "macos")]
#[test]
fn wrapped_spawn_enforces_network_deny_via_seatbelt() {
    // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    // `nc -z` is done at the handshake, which the listen backlog answers:
    // nothing need accept it.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("bound").port().to_string();
    let nc: Vec<std::ffi::OsString> = ["/usr/bin/nc", "-G", "1", "-w", "1", "-z", "127.0.0.1"]
        .into_iter()
        .chain([port.as_str()])
        .map(std::ffi::OsString::from)
        .collect();
    let run = |sandbox_wrap: Option<&str>| {
        let master = spawn_shell(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None, // argv_override
            Some(&nc),
            None, // cwd
            sandbox_wrap,
        )
        .expect("-e nc must spawn");
        // To EOF: nc exits within its one-second timeouts either way.
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        loop {
            let n = read(master, &mut buf);
            if n <= 0 {
                break;
            }
            out.extend_from_slice(&buf[..n as usize]);
        }
        close_fd(master);
        String::from_utf8_lossy(&out).into_owned()
    };
    let open = run(None);
    let denied = run(Some(aterm_containment::BASE_PROFILE));
    drop(listener);
    assert!(
        open.contains("succeeded"),
        "control: unwrapped nc connects: {open:?}"
    );
    assert!(
        !denied.contains("succeeded"),
        "DENY FAILED: wrapped nc connected under (deny network*): {denied:?}"
    );
}

/// The probe a `-e` command's arguments are read back with: each positional
/// argument's bytes in hex, one `[..]` line per argument, then `END`.
const ARGV_HEX: &str = "for a; do printf '['; printf %s \"$a\" | od -An -tx1 | tr -d ' \\n'; \
                        printf ']\\n'; done; echo END";

/// Run `exec` as a `-e` command through the real spawn seam and return what the
/// program saw of each positional argument, in hex ([`ARGV_HEX`]).
fn argv_bytes_seen(exec: &[std::ffi::OsString]) -> Vec<String> {
    // SAFETY: a test process; the trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let sh = spawn_shell_with_pid_cell_px(
        24,
        80,
        &spawn_cap,
        &sandbox_cap,
        &[],
        None, // shell_override
        None, // shell_args
        None, // argv_override
        Some(exec),
        None, // cwd
        None, // sandbox_wrap
        aterm_sandbox::Limits::inherit(),
        None, // cell_px
    )
    .expect("the probe command must spawn");
    set_nonblocking(sh.master, true).expect("nonblocking master");
    let mut seen = Vec::new();
    // A hang detector: the loop leaves the moment the probe's `END` is in.
    let deadline = std::time::Instant::now() + HANG_DETECTOR;
    while std::time::Instant::now() < deadline && !String::from_utf8_lossy(&seen).contains("END") {
        let mut buf = [0u8; 256];
        let n = read(sh.master, &mut buf);
        if n > 0 {
            seen.extend_from_slice(&buf[..n as usize]);
        } else {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    hangup(sh.pid);
    // SAFETY: closing the master this test owns.
    unsafe { libc::close(sh.master) };
    reap(sh.pid);
    String::from_utf8_lossy(&seen)
        .lines()
        .filter_map(|l| l.trim().strip_prefix('[')?.strip_suffix(']'))
        .map(str::to_string)
        .collect()
}

/// A `-e` argument reaches the program byte for byte: non-UTF-8 is not turned into
/// U+FFFD, and the argument after it stays in place.
#[test]
fn a_command_argument_reaches_the_program_byte_for_byte() {
    use std::os::unix::ffi::OsStringExt as _;
    let raw = std::ffi::OsString::from_vec(vec![0xff, b'a', 0x80]);
    let exec: Vec<std::ffi::OsString> = vec![
        "/bin/sh".into(),
        "-c".into(),
        ARGV_HEX.into(),
        "sh".into(),
        raw,
        "c".into(),
    ];
    assert_eq!(argv_bytes_seen(&exec), ["ff6180", "63"]);
}

/// A NUL byte in any argv source refuses the spawn (`InvalidInput`) instead
/// of running something else: `execve` reads a C string only up to its NUL,
/// so an argument holding one would be cut and every argument after it
/// shifted, and a configured shell holding one once ran another program
/// (`shell = "/bin/zsh\u0000x"` with `shell_args = ["-c", "echo $0"]` printed
/// `/bin/sh`). Every list a spawn takes its argv from is checked (`-e`, the
/// argv override, config `shell_args`), and every branch that runs the shell
/// (those two and the bare login shell). The control: an `-e` command does
/// not run the shell, so a NUL in the shell does not refuse it.
#[test]
fn a_nul_in_an_argument_or_the_shell_refuses_the_spawn() {
    use std::ffi::OsString;
    // SAFETY: a test process; the trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    fn list<T: From<&'static str>>(v: &[&'static str]) -> Vec<T> {
        v.iter().map(|s| T::from(*s)).collect()
    }
    const NUL_SHELL: &str = "/bin/sh\0x";
    let exec_nul: Vec<OsString> = list(&["/bin/sh", "-c", "exit 0", "a\0b", "c"]);
    let args_nul: Vec<String> = list(&["-c", "exit 0", "a\0b", "c"]);
    let over_nul: Vec<String> = list(&["sh", "-c", "exit 0", "a\0b", "c"]);
    let exec: Vec<OsString> = list(&["/bin/sh", "-c", "exit 0"]);
    let args: Vec<String> = list(&["-c", "exit 0"]);
    let over: Vec<String> = list(&["sh", "-c", "exit 0"]);
    let spawn = |shell: &str,
                 shell_args: Option<&[String]>,
                 argv_override: Option<&[String]>,
                 exec: Option<&[OsString]>| {
        spawn_shell_with_pid_cell_px(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            Some(shell),
            shell_args,
            argv_override,
            exec,
            None, // cwd
            None, // sandbox_wrap
            aterm_sandbox::Limits::inherit(),
            None, // cell_px
        )
    };
    let end = |sh: SpawnedShell| {
        hangup(sh.pid);
        close_fd(sh.master);
        reap(sh.pid);
    };
    for (what, got) in [
        ("-e", spawn("/bin/sh", None, None, Some(&exec_nul))),
        ("shell_args", spawn("/bin/sh", Some(&args_nul), None, None)),
        (
            "argv_override",
            spawn("/bin/sh", None, Some(&over_nul), None),
        ),
        (
            "shell, shell_args",
            spawn(NUL_SHELL, Some(&args), None, None),
        ),
        (
            "shell, argv_override",
            spawn(NUL_SHELL, None, Some(&over), None),
        ),
        ("shell, login", spawn(NUL_SHELL, None, None, None)),
    ] {
        match got {
            Ok(sh) => {
                end(sh);
                panic!("{what}: a NUL was cut and something else ran");
            }
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{what}: {e}"),
        }
    }
    end(spawn(NUL_SHELL, None, None, Some(&exec))
        .expect("an -e command does not run the shell, so it is not refused for it"));
}

/// THE SHELL'S BAND ([`SHELL_ROLE`], macOS). A spawned shell holds the UI darwin
/// role, so its main thread runs in the UI tier, above the default band (31) a role-less
/// app child is squashed to; a program the shell starts runs in the default band again;
/// and a program run directly (`-e`) keeps the default. Under a parent whose children
/// can hold no role (an aterm that is no app: a launchd job, ssh) nothing is raised and
/// the spawns still succeed. NEGATIVE CONTROLS: without step (1b) the shell reads role 0
/// at 31; a raise for every child, not only shells, gives the `-e` program role 2.
#[cfg(target_os = "macos")]
#[test]
fn a_spawned_shell_runs_in_the_ui_tier_and_nothing_it_starts_does() {
    use std::time::Duration;
    // SAFETY: the trusted-launcher contract trivially holds in a test.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let spawn = |shell_args: Option<&[String]>, exec: Option<&[std::ffi::OsString]>| {
        spawn_shell_with_pid_cell_px(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            Some("/bin/sh"),
            shell_args,
            None, // argv_override
            exec,
            None, // cwd
            None, // sandbox_wrap
            aterm_sandbox::Limits::inherit(),
            None, // cell_px
        )
        .expect("the spawn")
    };
    // The shell starts one program in the background and names it.
    let args = [
        "-c".to_owned(),
        "sleep 30 & echo \"CHILD=$!\"; wait".to_owned(),
    ];
    let shell = spawn(Some(&args), None);
    let program = spawn(None, Some(&["/bin/sleep".into(), "30".into()]));
    set_nonblocking(shell.master, true).expect("nonblocking master");
    let mut seen = String::new();
    let deadline = std::time::Instant::now() + HANG_DETECTOR;
    let child = loop {
        let mut buf = [0u8; 256];
        let n = read(shell.master, &mut buf);
        if n > 0 {
            seen.push_str(&String::from_utf8_lossy(&buf[..n as usize]));
        }
        let named = seen.split_once("CHILD=").and_then(|(_, rest)| {
            let end = rest.find(|c: char| !c.is_ascii_digit())?;
            rest[..end].parse::<i32>().ok()
        });
        if let Some(pid) = named {
            break pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no CHILD= line: {seen:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    // SAFETY: `getpriority` reads a role; no pointers.
    let role = |pid: i32| unsafe { libc::getpriority(PRIO_DARWIN_ROLE, pid as libc::id_t) };
    let seen_bands =
        [shell.pid, child, program.pid].map(|pid| (role(pid), main_thread_priority(pid)));
    // Whether a child of this process can hold a role at all: a fork child that asks.
    // SAFETY: the child calls only `setpriority` and `_exit`, both async-signal-safe.
    let held = unsafe {
        let pid = libc::fork();
        if pid == 0 {
            libc::_exit(i32::from(
                libc::setpriority(PRIO_DARWIN_ROLE, 0, SHELL_ROLE) != 0,
            ));
        }
        let mut status = 0;
        libc::waitpid(pid, &mut status, 0) == pid
            && libc::WIFEXITED(status)
            && libc::WEXITSTATUS(status) == 0
    };
    for sh in [shell, program] {
        hangup(sh.pid);
        close_fd(sh.master);
        reap(sh.pid);
    }
    let [
        (shell_role, shell_pri),
        (child_role, child_pri),
        (program_role, program_pri),
    ] = seen_bands;
    if held {
        assert_eq!(shell_role, SHELL_ROLE, "the shell holds the UI role");
        assert!(
            shell_pri > 31,
            "the shell's main thread runs above the default band: {shell_pri}"
        );
        assert_eq!(child_role, 0, "the shell's own program holds no role");
        assert!(
            child_pri <= 31,
            "the shell's program runs in the default band: {child_pri}"
        );
    } else {
        assert_eq!(shell_role, 0, "no role can be held here, so none is");
    }
    assert_eq!(program_role, 0, "an -e program holds no role");
    assert!(
        program_pri <= 31,
        "an -e program runs in the default band: {program_pri}"
    );
}

/// The base priority of `pid`'s first (main) thread, from `PROC_PIDTHREADINFO`.
#[cfg(target_os = "macos")]
fn main_thread_priority(pid: i32) -> i32 {
    /// `struct proc_threadinfo` (`<sys/proc_info.h>`); not in `libc`.
    #[repr(C)]
    struct ProcThreadInfo {
        user_time: u64,
        system_time: u64,
        cpu_usage: i32,
        policy: i32,
        run_state: i32,
        flags: i32,
        sleep_time: i32,
        curpri: i32,
        priority: i32,
        maxpriority: i32,
        name: [u8; 64],
    }
    const PROC_PIDTHREADINFO: libc::c_int = 5;
    const PROC_PIDLISTTHREADS: libc::c_int = 6;
    let mut threads = [0u64; 8];
    // SAFETY: `proc_pidinfo` writes at most the given size into the live buffers.
    unsafe {
        let n = libc::proc_pidinfo(
            pid,
            PROC_PIDLISTTHREADS,
            0,
            threads.as_mut_ptr().cast(),
            std::mem::size_of_val(&threads) as libc::c_int,
        );
        assert!(n >= 8, "pid {pid} lists its threads");
        let mut info: ProcThreadInfo = std::mem::zeroed();
        let got = libc::proc_pidinfo(
            pid,
            PROC_PIDTHREADINFO,
            threads[0],
            (&raw mut info).cast(),
            std::mem::size_of::<ProcThreadInfo>() as libc::c_int,
        );
        assert!(got > 0, "pid {pid}'s main thread answers");
        info.priority
    }
}
