// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A hang detector, never a latency budget: every wait on it ends the
/// moment the thing it waits for happens.
const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);

#[test]
fn size_reads_back_what_resize_wrote() {
    let (master, slave) = open_pty_pair().expect("pty pair");
    resize(master, 61, 200);
    assert_eq!(size(master), Some((61, 200)));
    resize(master, 60, 200);
    assert_eq!(size(master), Some((60, 200)));
    assert_eq!(size(-1), None);
    close_fd(slave);
    close_fd(master);
}

#[test]
fn wake_pipe_is_cloexec_and_nonblocking_on_both_ends() {
    let (rd, wr) = make_wake_pipe().expect("wake pipe");
    // SAFETY: read-only fcntl queries on two live fds owned by this test.
    unsafe {
        for fd in [rd, wr] {
            assert_ne!(libc::fcntl(fd, libc::F_GETFD) & libc::FD_CLOEXEC, 0);
            assert_ne!(libc::fcntl(fd, libc::F_GETFL) & libc::O_NONBLOCK, 0);
        }
    }
    close_fd(rd);
    close_fd(wr);
}

#[test]
fn saturated_wake_pipe_never_blocks_the_caller() {
    let (rd, wr) = make_wake_pipe().expect("wake pipe");
    let fill = [0x55u8; 4096];
    loop {
        // SAFETY: bounded write to this test's live non-blocking pipe end.
        let wrote = unsafe { libc::write(wr, fill.as_ptr().cast(), fill.len()) };
        if wrote < 0 {
            assert_eq!(
                std::io::Error::last_os_error().kind(),
                std::io::ErrorKind::WouldBlock,
                "pipe saturation must report EAGAIN"
            );
            break;
        }
    }
    let started = std::time::Instant::now();
    wake(wr);
    assert!(
        // A genuine failure blocks FOREVER on a full pipe with no reader, so any
        // finite bound catches it. 20ms sits inside one scheduler quantum and was
        // measuring preemption, not the never-blocks property; a minute is the
        // hang detector the house rule names.
        started.elapsed() < std::time::Duration::from_secs(60),
        "a redundant wake against a full pipe must return promptly"
    );
    close_fd(rd);
    close_fd(wr);
}

/// MEM-L2: a reader parked in `read_or_wake` on a master that STAYS OPEN with no
/// data and never EOFs (the orphaned-child-holds-the-slave scenario) must still
/// unblock the instant the owner fires the wake pipe — otherwise `Session::drop`
/// could never reclaim the reader (and the Terminal + master fd it pins).
#[test]
fn read_or_wake_unblocks_on_wake_even_when_master_never_eofs() {
    use std::sync::mpsc;
    use std::time::Duration;
    // A pipe stands in for a master held open with no data: its write end lives for
    // the whole test, so the read end never sees data OR EOF.
    let mut m = [0i32; 2];
    // SAFETY: valid 2-int out-array for pipe(2).
    assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
    let (master_rd, master_wr) = (m[0], m[1]);
    let (wake_rd, wake_wr) = make_wake_pipe().expect("wake pipe");

    let (tx, rx) = mpsc::channel();
    // Publish entry so the negative below cannot pass merely because the thread
    // was never scheduled: a 200ms `recv_timeout` is well inside a scheduling
    // delay on a loaded box, and without this a `read_or_wake` that busy-returned
    // immediately would still look like "stayed parked".
    let entered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader_entered = entered.clone();
    let h = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        reader_entered.store(true, std::sync::atomic::Ordering::SeqCst);
        let out = read_or_wake(master_rd, &mut buf, wake_rd);
        tx.send(matches!(out, ReadOutcome::Wake)).unwrap();
        close_fd(wake_rd);
    });
    while !entered.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::yield_now();
    }

    // Neither fd is ready ⇒ the reader must stay parked (not busy-return).
    assert!(
        rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "reader must block while master has no data and the wake pipe is empty"
    );

    // Fire the wake: the reader must return `Wake` promptly even though `master_rd`
    // never saw data or EOF (`master_wr` is still open).
    wake(wake_wr);
    // A hang detector for a handoff that must happen: the reader thread
    // returns the moment it is scheduled after the wake byte lands.
    let got_wake = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("reader must unblock promptly once the wake pipe fires");
    assert!(got_wake, "the wake pipe must yield ReadOutcome::Wake");

    h.join().unwrap();
    close_fd(wake_wr);
    // SAFETY: closing the two pipe fds this test owns.
    unsafe {
        libc::close(master_rd);
        libc::close(master_wr);
    }
}

/// Direct-read drain (O_NONBLOCK gather): an interactive-sized burst (< one
/// outq) must be delivered on the FIRST quiet read — no spin bridge, no
/// parking — and gather exactly the bytes written.
///
/// Read without a clock. A drain that skipped the first-quiet-read
/// delivery parks ONE 1 ms bridge poll and is then capped by its 3 ms
/// `BATCH_BUDGET`, so no wall-clock bound can tell it apart. The park seam `at_gap` fires
/// immediately before every park and the bridge-poll census counts the
/// parks themselves: a first-quiet-read delivery reaches neither.
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
    let filled = drain_more_nonblocking(rd, &mut buf, 1, -1, None, never_hot, &mut |_| {
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

/// The host hint "a human is waiting on the UI thread", pinned OFF: the
/// flood-shaped gather every test below assumes unless it says otherwise.
fn never_hot() -> bool {
    false
}

/// …and pinned ON: a keystroke is pending for the whole gather.
fn always_hot() -> bool {
    true
}

/// P05 — with a keystroke pending the gather bridges a dry gap with the µs
/// idle wait EVEN WHILE THE PARSER IS BUSY (never the 1 ms bridge poll),
/// and a refill that lands inside that wait still joins the same batch —
/// a TUI's per-keystroke repaint burst keeps coalescing.
#[test]
fn drain_more_nonblocking_hot_bridges_a_busy_parser_gap_with_the_idle_wait() {
    use std::sync::atomic::AtomicUsize;
    const CHUNK: usize = 2048;
    /// One gather over a fresh pipe: (bytes gathered, parks taken, wall held).
    fn gather() -> (usize, u32, std::time::Duration) {
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(rd, true).expect("nonblock read end");
        let chunk = [0xC3u8; CHUNK];
        // SAFETY: bounded write to this test's live pipe end.
        assert_eq!(
            unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
            chunk.len() as isize
        );
        let busy = AtomicUsize::new(1);
        let mut buf = [0u8; 65_536];
        let n = read(rd, &mut buf[..1024]);
        assert!(n > 0);
        let mut parks = 0u32;
        let t0 = std::time::Instant::now();
        let filled = drain_more_nonblocking_with_idle_wait_after_gap(
            rd,
            &mut buf,
            n as usize,
            -1,
            Some(&busy),
            always_hot,
            // The knob at 0 would DELIVER on the idle branch before the seam; a
            // hot gather that fired the seam therefore provably took the
            // µs-wait branch and not the busy-parser bridge poll.
            IDLE_POLL_DEFAULT_US,
            |_| {
                parks += 1;
                if parks == 1 {
                    // SAFETY: bounded write to this test's live pipe end.
                    assert_eq!(
                        unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
                        chunk.len() as isize
                    );
                }
                false
            },
        );
        let held = t0.elapsed();
        close_fd(wr);
        close_fd(rd);
        (filled, parks, held)
    }
    // The byte and park counts are exact on every run. The HOLD is a
    // wall-clock reading of a 50 µs wait, and a parallel suite can stretch
    // it past the 1 ms bound (held 1.43 ms once beside the continuous-stream
    // test's writer thread); the BEST of nine gathers is the property — the
    // old code parked a busy-parser gap on the 1 ms bridge poll, and its
    // gather cannot beat that however many times it runs. Nine, not three:
    // under the workspace gate's load (loadavg 8-18) the best of three once
    // read 943 µs against the 1 ms bound — every one of three ~80 µs gathers
    // had been stretched tenfold by scheduling — and a gather costs ~100 µs,
    // so nine samples buy the margin for nothing.
    // The subject is the PARK, not the 1 ms hot budget: a gather preempted
    // for a millisecond before its first dry gap delivers on the budget and
    // never reaches the park (3 of 200 runs beside 72 spinners read
    // `filled` 2048 or `parks` 1). The budget is a clock that measures the
    // load here, so this thread's gathers run under one no preemption
    // reaches; the budget's own tests keep the shipped value.
    super::BUDGET_FOR_TEST.with(|b| b.set(Some(std::time::Duration::from_secs(60))));
    let mut best_held = std::time::Duration::MAX;
    for _ in 0..9 {
        let polls_before = super::BRIDGE_POLLS.with(std::cell::Cell::get);
        let (filled, parks, held) = gather();
        // THE PROPERTY, read without a clock: the dry gap was bridged by the
        // µs idle wait, never by the 1 ms bridge poll.
        assert_eq!(
            super::BRIDGE_POLLS.with(std::cell::Cell::get),
            polls_before,
            "a hot gather took the 1 ms bridge poll"
        );
        assert_eq!(
            filled,
            2 * CHUNK,
            "a refill inside the hot idle wait must join the same batch"
        );
        assert!(
            parks >= 2,
            "one park to bridge the refill, one to end the burst"
        );
        best_held = best_held.min(held);
    }
    // The hold is printed, not asserted: the park counter above is the
    // property, and a wall-clock reading of a ~100 µs gather is the load's.
    super::BUDGET_FOR_TEST.with(|b| b.set(None));
    eprintln!("P05 busy-parser hot gather: best of nine held {best_held:?}");
}

/// P05 — the hold a CONTINUOUS stream (a refill waiting at every dry gap,
/// for longer than any budget) imposes on an echo byte: the 1 ms hot budget
/// with a keystroke pending, the 3 ms flood budget otherwise. Red before the
/// fix: both arms held ≥ 3 ms.
///
/// THE STREAM COMES FROM THE PARK SEAM, NOT FROM A THREAD. A writer thread
/// yielding one byte per turn made "continuous" a claim about the scheduler:
/// descheduled for longer than the 1 ms bridge poll, it ended the cold
/// gather on "burst over" — correct behaviour for a stream that really
/// paused. So each gather writes one
/// byte from `at_gap`, immediately before it parks: the refill is in the
/// pipe whenever the gather looks, the park returns `Refill` on every gap,
/// and the ONLY way either arm can end is the budget check.
///
/// WHICH BUDGET ENDED IT IS COUNTED, NOT TIMED: a wall-clock ceiling reads
/// the machine's load. The gather records the budget whose
/// check ended it ([`BUDGET_ENDED`]), so each arm is judged by the budget
/// that cut it, on every run: code that bridges a hot gather to
/// the 3 ms `BATCH_BUDGET`, reads 3 ms in the hot arm, and a cold arm cut
/// at the hot budget reads 1 ms in the cold one. The hold keeps its one
/// exact bound, from below — the budget check that ended the gather read a
/// clock started after `t0` — and is printed, not judged from above.
#[test]
fn drain_more_nonblocking_hot_caps_a_continuous_stream_at_the_hot_budget() {
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;
    const BUF: usize = 65_536;
    /// The flood gather's `BATCH_BUDGET` (a const local to its body).
    const COLD_BUDGET: Duration = Duration::from_millis(3);
    /// [`HOT_BATCH_BUDGET`], pinned to its value as a LITERAL: the verdict
    /// below compares the budget that ended the gather against this, and a
    /// comparison against the constant itself holds for any value of it —
    /// a hot budget raised back to the 3 ms streaming batch passed.
    const HOT_BUDGET: Duration = Duration::from_millis(1);
    /// Gathers per arm until one bridges a gap (see `capped`), 10 ms apart,
    /// so one scheduling window cannot cover them all.
    const ATTEMPTS: usize = 15;
    /// One gather over a stream that never pauses.
    struct Gather {
        held: Duration,
        filled: usize,
        /// Dry gaps parked at.
        gaps: u32,
        /// 1 ms bridge polls taken.
        polls: u32,
        /// The budget whose check ended the gather, if one did.
        ended: Option<Duration>,
    }
    fn gather(interactive_pending: fn() -> bool) -> Gather {
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
        let t0 = std::time::Instant::now();
        let filled = drain_more_nonblocking_with_idle_wait_after_gap(
            rd,
            &mut buf,
            n as usize,
            -1,
            Some(&busy),
            interactive_pending,
            IDLE_POLL_DEFAULT_US,
            |_| {
                gaps += 1;
                // SAFETY: a one-byte write to this test's live pipe end; the
                // gather drains it, so the 64 KiB pipe never fills.
                assert_eq!(unsafe { libc::write(wr, b"x".as_ptr().cast(), 1) }, 1);
                false
            },
        );
        let held = t0.elapsed();
        let polls = super::BRIDGE_POLLS.with(std::cell::Cell::get) - polls_before;
        let ended = super::BUDGET_ENDED.with(std::cell::Cell::take);
        close_fd(wr);
        close_fd(rd);
        Gather {
            held,
            filled,
            gaps,
            polls,
            ended,
        }
    }
    /// Every gather's verdict, clockless but for the exact lower bound: it
    /// did not stop on a full buffer, each gap took one park of its arm's
    /// kind, and THIS ARM's budget ended it. With the writer open, a byte
    /// always queued before the park and the buffer not full, the gather's
    /// other exits are EOF, a hard error and a signal interrupting a park,
    /// none of which this test can produce.
    ///
    /// Answers whether the gather bridged a gap at all. The budget is
    /// checked at every dry gap BEFORE the park seam, so a gather preempted
    /// past its budget before its first gap ends on the budget having
    /// bridged nothing (beside the same spinners: "hot: the stream was never
    /// bridged"): a correct gather, but no sample of a bridged stream, so
    /// each arm runs until one gather bridges.
    fn capped(arm: &str, g: &Gather, hot: bool) -> bool {
        let budget = if hot { HOT_BUDGET } else { COLD_BUDGET };
        assert!(
            g.filled < BUF,
            "{arm}: ended on a full buffer, which proves nothing"
        );
        // Hot bridges with the µs idle wait, flood with the 1 ms bridge poll;
        // each gap is followed by exactly one park of its arm's kind.
        let expected = if hot { 0 } else { g.gaps };
        assert_eq!(
            g.polls, expected,
            "{arm}: {} gaps took {} bridge polls",
            g.gaps, g.polls
        );
        assert_eq!(
            g.ended,
            Some(budget),
            "{arm}: the gather must end on its own {budget:?} budget (held {:?}, \
             {} gaps)",
            g.held,
            g.gaps
        );
        assert!(
            g.held >= budget,
            "{arm}: a gather ended by its {budget:?} budget held only {:?}",
            g.held
        );
        g.gaps >= 1
    }
    let mut holds = [Duration::ZERO; 2];
    for (i, (arm, pending, hot)) in [
        ("hot", always_hot as fn() -> bool, true),
        ("cold", never_hot as fn() -> bool, false),
    ]
    .into_iter()
    .enumerate()
    {
        let mut bridged = None;
        for _ in 0..ATTEMPTS {
            let g = gather(pending);
            if capped(arm, &g, hot) {
                bridged = Some(g.held);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        holds[i] = bridged
            .unwrap_or_else(|| panic!("{arm}: the stream was never bridged in {ATTEMPTS} gathers"));
    }
    eprintln!(
        "P05 continuous-stream gather hold: hot={:?} cold={:?}",
        holds[0], holds[1]
    );
}

/// Direct-read drain: a saturated burst (>= one outq) is gathered fully into
/// one batch, and EOF mid-drain just stops the top-up (kept bytes intact).
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
    let filled = drain_more_nonblocking(rd, &mut buf, n as usize, -1, None, never_hot, &mut |_| {
        false
    });
    assert_eq!(
        filled,
        payload.len(),
        "must gather the full saturated burst"
    );
    assert!(buf[..filled].iter().all(|&b| b == 0xA5));
    close_fd(rd);
}

/// Bridge teardown contract: a wake byte pending during the bridge poll
/// makes the drain deliver what it has WITHOUT consuming the byte —
/// `read_or_wake` owns the wake protocol and must still observe it.
#[test]
fn drain_more_nonblocking_bridge_yields_to_wake_and_leaves_the_byte() {
    let mut m = [0i32; 2];
    // SAFETY: valid 2-int out-array for pipe(2).
    assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
    let (rd, wr) = (m[0], m[1]);
    set_nonblocking(rd, true).expect("nonblock read end");
    let payload = [0x5Au8; 4096];
    // SAFETY: bounded write to this test's live pipe end.
    assert_eq!(
        unsafe { libc::write(wr, payload.as_ptr().cast(), payload.len()) },
        payload.len() as isize
    );
    let mut wk = [0i32; 2];
    // SAFETY: valid 2-int out-array for pipe(2).
    assert_eq!(unsafe { libc::pipe(wk.as_mut_ptr()) }, 0, "wake pipe");
    let (wake_rd, wake_wr) = (wk[0], wk[1]);
    wake(wake_wr); // teardown byte queued BEFORE the drain bridges
    let mut buf = [0u8; 65_536];
    let n = read(rd, &mut buf[..1024]);
    assert!(n > 0);
    let filled = drain_more_nonblocking(
        rd,
        &mut buf,
        n as usize,
        wake_rd,
        None,
        never_hot,
        &mut |_| false,
    );
    assert_eq!(filled, payload.len(), "keeps the drained burst");
    let mut b = [0u8; 4];
    // SAFETY: bounded read from this test's live wake pipe end.
    let wn = unsafe { libc::read(wake_rd, b.as_mut_ptr().cast(), 4) };
    assert_eq!(wn, 1, "bridge must NOT consume the wake byte");
    close_fd(rd);
    close_fd(wr);
    close_fd(wake_rd);
    close_fd(wake_wr);
}

/// The host FENCE: `at_gap` returning
/// `true` at a dry gap delivers the batch WITHOUT parking, and a refill that
/// arrived at that gap stays queued for the next read — it must not be
/// gathered into the batch the fence just cut, because aterm-gui cuts at the
/// foreground-process-group change and the refill belongs to the NEW group.
/// Negative control in the same test: the identical schedule with a fence
/// that always answers `false` bridges the refill into the batch. That arm is also the refill
/// continuation — THE flood-throughput mechanism (181→226 MB/s): with the
/// parse stage BUSY a refill landing in the bridge park joins the SAME
/// batch. With the idle knob at 0 it pins the idle cutoff's sign too: an
/// inverted `== 0` sends this busy parser down the idle branch, which
/// delivers before the fence is ever asked.
///
/// Both runs gather under this thread's test budget ([`BUDGET_FOR_TEST`]):
/// the shipped 3 ms `BATCH_BUDGET` is a clock started on entry and checked
/// at every dry gap before the fence, so a gather preempted that long
/// delivered without ever asking it (beside 72 spinners with the test
/// clamped to background QoS: `seen_at` empty, "index out of bounds").
#[test]
fn drain_more_nonblocking_fence_delivers_before_a_park() {
    use std::sync::atomic::AtomicUsize;
    super::BUDGET_FOR_TEST.with(|b| b.set(Some(std::time::Duration::from_secs(60))));
    let chunk = [0xC3u8; 2048];
    // Returns (filled, gaps seen by the fence, bytes a follow-up read finds).
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
            Some(&busy),
            never_hot,
            0,
            |f| {
                gaps += 1;
                seen_at.push(f);
                if gaps == 1 {
                    // The new holder's first bytes, queued at the gap.
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

/// Parser-idle cutoff (the ghostty fps-fire class): with `in_flight == 0`
/// and the stream dry, the drain must deliver after the probe burst —
/// never park in the 1 ms bridge poll. Runs both the public fn (env unset
/// ⇒ the measured `IDLE_POLL_DEFAULT_US` = 50 µs wait) and the inner body
/// with an explicit 0 (the immediate-deliver revert path).
///
/// READ BY CENSUS, NOT BY A STOPWATCH: a clock measures the load, and
/// the property is which PARK the dry gap took, which is countable: the
/// bridge-poll census counts the 1 ms polls this thread takes, and `at_gap`
/// fires immediately before either park. One more clock sat in the
/// gather: its 3 ms `BATCH_BUDGET`, checked at every dry gap from entry,
/// lets a gather preempted that long deliver before it reaches the gap at
/// all — a pass that proves nothing. So this thread's gathers run under a
/// budget no preemption reaches ([`BUDGET_FOR_TEST`]); with the writer
/// still open and nothing more written, the only ways such a gather ends
/// are the idle cutoff and the bridge poll's quiet timeout, and the census
/// tells them apart. One attempt per path is exact; a regressed cutoff
/// parks on the bridge poll every time.
#[test]
fn drain_more_nonblocking_idle_parser_delivers_without_parking() {
    use std::sync::atomic::AtomicUsize;
    /// One `BRIDGE_POLL_MS` park (a const local to the gather's body), in µs.
    const BRIDGE_POLL_US: u32 = 1_000;
    const {
        assert!(
            IDLE_POLL_DEFAULT_US > 0,
            "the measured idle-refill default must stay armed"
        );
        assert!(
            IDLE_POLL_DEFAULT_US < BRIDGE_POLL_US,
            "the shipped idle wait must be shorter than one bridge poll"
        );
    }
    super::BUDGET_FOR_TEST.with(|b| b.set(Some(std::time::Duration::from_secs(60))));
    for explicit_zero in [false, true] {
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(rd, true).expect("nonblock read end");
        let chunk = [0x7Eu8; 4096];
        // SAFETY: bounded write to this test's live pipe end.
        assert_eq!(
            unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
            chunk.len() as isize
        );
        let idle = AtomicUsize::new(0);
        let mut buf = [0u8; 65_536];
        let n = read(rd, &mut buf[..1024]);
        assert!(n > 0);
        let polls_before = super::BRIDGE_POLLS.with(std::cell::Cell::get);
        super::IDLE_WAIT_TIMEOUT_US.with(|t| t.set(None));
        let mut gaps = 0u32;
        let filled = if explicit_zero {
            drain_more_nonblocking_with_idle_wait_after_gap(
                rd,
                &mut buf,
                n as usize,
                -1,
                Some(&idle),
                never_hot,
                0,
                |_| {
                    gaps += 1;
                    false
                },
            )
        } else {
            drain_more_nonblocking(
                rd,
                &mut buf,
                n as usize,
                -1,
                Some(&idle),
                never_hot,
                &mut |_| {
                    gaps += 1;
                    false
                },
            )
        };
        let polls = super::BRIDGE_POLLS.with(std::cell::Cell::get) - polls_before;
        let idle_timeout_us = super::IDLE_WAIT_TIMEOUT_US.with(std::cell::Cell::take);
        close_fd(rd);
        close_fd(wr);
        assert_eq!(filled, chunk.len(), "keeps the drained burst");
        // The knob at 0 delivers on the idle branch BEFORE the park seam;
        // an armed one (the public path folds in the CONFIGURED wait, env
        // knob or default, so an exported ATERM_PTY_IDLE_POLL_US cannot
        // fail this spuriously) fires the seam once, then its µs wait sees
        // no refill and delivers.
        let expected_gaps = u32::from(!explicit_zero && idle_poll_us() > 0);
        assert_eq!(
            (gaps, polls),
            (expected_gaps, 0),
            "an idle-parser dry gap must deliver within its idle wait, not \
             park the 1ms bridge poll: (dry gaps parked at, bridge polls \
             taken) (explicit_zero={explicit_zero}, configured idle wait {}µs)",
            idle_poll_us()
        );
        // …and the park it took must be the wait it was CONFIGURED for,
        // read off the timeout the select is handed, not off a clock: the
        // census above passes an idle wait of any length, and one rounded
        // up to poll(2)'s millisecond parks every idle gap for a full
        // bridge poll, which the census alone does not catch. The
        // configured value, not the default, so an exported
        // ATERM_PTY_IDLE_POLL_US cannot fail this spuriously; the const
        // check above holds the default below one bridge poll.
        let expected_timeout = (expected_gaps == 1).then(|| u64::from(idle_poll_us()));
        assert_eq!(
            idle_timeout_us, expected_timeout,
            "the idle wait must hand the kernel exactly its configured \
             timeout in µs (explicit_zero={explicit_zero})"
        );
    }
    super::BUDGET_FOR_TEST.with(|b| b.set(None));
}

/// Hysteresis knob (`ATERM_PTY_IDLE_POLL_US` > 0): an idle parser at a dry
/// gap WAITS for the writer's refill instead of delivering, so the refill
/// lands in the SAME batch — a refill written from another thread WHILE
/// the gather is parked, so the park itself wakes on arrival. The wait's
/// length is the only difference from the shipped default, and the test
/// above reads that off the timeout the select is handed.
///
/// The writer is released by the gather's own `at_gap` seam, not by a
/// sleep raced against the gather's start: once `at_gap` runs the gather is about to park, and the
/// writer then waits 20 ms — twenty busy-parser 1 ms bridge polls — so a
/// gap that parked on the wrong path has expired before the refill lands.
/// The buffer holds exactly two chunks, so capacity ends the batch without
/// depending on EOF (which a sibling test's fork can delay).
///
/// NO CLOCK DECIDES A VERDICT. The idle wait is the minute hang detector,
/// so only a park that never wakes on arrival sits it out. And the gather
/// runs under this thread's test budget ([`BUDGET_FOR_TEST`]) rather than
/// the 3 ms `BATCH_BUDGET`, so a preempted gather still reaches the gap:
/// one attempt, whose every count is exact.
#[test]
fn drain_more_nonblocking_idle_wait_bridges_refill_into_same_batch() {
    use std::sync::atomic::AtomicUsize;
    /// The house hang detector, as the idle wait: only a park that ignores
    /// the refill's arrival waits it out.
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
        // A gather that never reached its park drops `release` unsent:
        // nothing is written, and the pipe just closes.
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
        Some(&idle),
        never_hot,
        IDLE_WAIT_US,
        |_| {
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

/// Hysteresis + teardown: a wake byte pending when the idle wait parks
/// must deliver promptly WITHOUT consuming the byte — [`read_or_wake`]
/// owns the wake protocol (same contract as the busy-parser bridge poll).
///
/// The idle wait is armed LONGER than the minute hang detector, so a park
/// that ignored the wake fd could only end on its own timeout, past the
/// detector (a short timeout would let a drain that dropped the wake fd
/// pass). The park must also actually be REACHED: the gather's 3 ms
/// `BATCH_BUDGET` runs from entry, so a reader descheduled that long before
/// its first dry gap delivers without parking and proves nothing; the
/// gather runs under this thread's test budget ([`BUDGET_FOR_TEST`]), so it
/// always reaches the park: one attempt, `at_gap` firing once immediately before it.
#[test]
fn drain_more_nonblocking_idle_wait_yields_to_wake_and_leaves_the_byte() {
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;
    /// The house hang detector.
    const HANG: Duration = Duration::from_secs(60);
    /// Past [`HANG`]: a wait that ignored the wake ends only after it.
    const IDLE_WAIT_US: u32 = 90_000_000;
    super::BUDGET_FOR_TEST.with(|b| b.set(Some(HANG)));
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
    let mut wk = [0i32; 2];
    // SAFETY: valid 2-int out-array for pipe(2).
    assert_eq!(unsafe { libc::pipe(wk.as_mut_ptr()) }, 0, "wake pipe");
    let (wake_rd, wake_wr) = (wk[0], wk[1]);
    wake(wake_wr); // teardown byte queued BEFORE the idle wait parks
    let idle = AtomicUsize::new(0);
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
        Some(&idle),
        never_hot,
        IDLE_WAIT_US,
        |_| {
            gaps += 1;
            false
        },
    );
    let took = t0.elapsed();
    super::BUDGET_FOR_TEST.with(|b| b.set(None));
    assert_eq!(filled, payload.len(), "keeps the drained burst");
    let mut b = [0u8; 4];
    // Non-blocking: a CONSUMED wake byte must fail the assert below, not
    // hang this read forever.
    set_nonblocking(wake_rd, true).expect("nonblock wake read end");
    // SAFETY: bounded read from this test's live wake pipe end.
    let wn = unsafe { libc::read(wake_rd, b.as_mut_ptr().cast(), 4) };
    close_fd(rd);
    close_fd(wr);
    close_fd(wake_rd);
    close_fd(wake_wr);
    assert_eq!(wn, 1, "idle wait must NOT consume the wake byte");
    assert_eq!(
        gaps, 1,
        "the gather reaches its idle wait, and the wake ends it"
    );
    // The park was reached with the wake byte queued: only the wake can
    // have ended it before the detector.
    assert!(
        took < HANG,
        "a queued wake must deliver promptly, not park the full idle wait: {took:?}"
    );
}

/// Producer death mid-bridge: the write end closing while the drain is
/// parked in the bridge poll must deliver with what it has (via the POLLHUP
/// Deliver arm, or the read()==0 EOF break one syscall later — both
/// protocol-safe), never stall against a dead producer.
///
/// Read by census, not by a stopwatch. A drain that kept bridging a dead
/// producer is still capped by its 3 ms `BATCH_BUDGET`, so no wall-clock
/// bound can see it. The write end
/// is closed from `at_gap`, immediately before the park, and the drain must
/// end at that park: one gap, one bridge poll, nothing after. (Which arm
/// delivers is not observable: the 1 ms poll's quiet timeout delivers the
/// same batch after the same poll. What is observable is that nothing
/// follows it.) The gather runs on its own thread, so a drain that never
/// ends fails at the minute hang detector instead of hanging the suite. The
/// gather runs under its thread's test budget ([`BUDGET_FOR_TEST`]), so a
/// preempted one still reaches its first dry gap: one attempt, no retries.
#[test]
fn drain_more_nonblocking_delivers_promptly_on_writer_hup() {
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;
    /// The house hang detector.
    const HANG: Duration = Duration::from_secs(60);
    const CHUNK_LEN: usize = 2048;
    /// One gather whose producer dies at its first park: (bytes gathered,
    /// dry gaps parked at, bridge polls taken, wall held).
    fn gather() -> (usize, u32, u32, Duration) {
        super::BUDGET_FOR_TEST.with(|b| b.set(Some(HANG)));
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(rd, true).expect("nonblock read end");
        let chunk = [0x11u8; CHUNK_LEN];
        // SAFETY: bounded write to this test's live pipe end.
        assert_eq!(
            unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
            chunk.len() as isize
        );
        let busy = AtomicUsize::new(1); // busy parser: the bridge parks
        let mut buf = [0u8; 65_536];
        let n = read(rd, &mut buf[..1024]);
        assert!(n > 0);
        let polls_before = super::BRIDGE_POLLS.with(std::cell::Cell::get);
        let mut gaps = 0u32;
        let t0 = std::time::Instant::now();
        let filled = drain_more_nonblocking(
            rd,
            &mut buf,
            n as usize,
            -1,
            Some(&busy),
            never_hot,
            &mut |_| {
                gaps += 1;
                if gaps == 1 {
                    close_fd(wr); // the producer dies as the drain parks
                }
                false
            },
        );
        let took = t0.elapsed();
        super::BUDGET_FOR_TEST.with(|b| b.set(None));
        let polls = super::BRIDGE_POLLS.with(std::cell::Cell::get) - polls_before;
        if gaps == 0 {
            close_fd(wr);
        }
        close_fd(rd);
        (filled, gaps, polls, took)
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(gather());
    });
    let Ok((filled, gaps, polls, took)) = rx.recv_timeout(HANG) else {
        panic!("a drain against a dead producer did not end within {HANG:?}");
    };
    assert_eq!(filled, CHUNK_LEN, "keeps what was drained before HUP");
    assert_eq!(
        (gaps, polls),
        (1, 1),
        "a dead producer must end the batch at the park it died in, not bridge on: \
         (dry gaps parked at, bridge polls taken), held {took:?}"
    );
}

/// Blocking-emulated writes on an O_NONBLOCK description: a full queue must
/// PARK (in poll) until the reader drains, then complete. `write_some_blocking`
/// never surfaces `WouldBlock` (the legacy blocking-master contract every frame
/// writer keeps).
#[test]
fn blocking_writes_park_through_a_full_queue() {
    /// Fill a non-blocking pipe to EAGAIN, then `write` into it while a helper
    /// thread drains it; answers what `write` reported.
    fn through_a_full_pipe(write: fn(i32) -> usize) -> usize {
        use std::time::Duration;
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(wr, true).expect("nonblock write end");
        let fill = [0u8; 4096];
        loop {
            // SAFETY: bounded write to this test's live non-blocking pipe end.
            if unsafe { libc::write(wr, fill.as_ptr().cast(), fill.len()) } < 0 {
                assert_eq!(io::Error::last_os_error().kind(), io::ErrorKind::WouldBlock);
                break;
            }
        }
        // A sleep alone cannot make the writer park FIRST: if the main thread
        // is descheduled before its write, the drain lands first, the write never parks, and the park-through-EAGAIN property
        // under test simply does not run — a vacuous pass. Gate the drain on the
        // writer announcing itself, and keep a short sleep as slack for the gap
        // between the flag and the actual park.
        let about_to_write = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let drainer_gate = about_to_write.clone();
        let h = std::thread::spawn(move || {
            while !drainer_gate.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::yield_now();
            }
            std::thread::sleep(Duration::from_millis(50));
            let mut sink_buf = [0u8; 65_536];
            set_nonblocking(rd, true).expect("nonblock read end");
            while read(rd, &mut sink_buf) > 0 {}
            rd // keep rd open until after the writer completes
        });
        about_to_write.store(true, std::sync::atomic::Ordering::SeqCst);
        let wrote = write(wr);
        let rd = h.join().unwrap();
        close_fd(rd);
        close_fd(wr);
        wrote
    }
    assert!(
        through_a_full_pipe(|wr| {
            write_some_blocking(wr, b"parked-frame").expect("must not surface WouldBlock")
        }) > 0,
        "the retried write must land bytes after the drain"
    );
}

/// MEM-L2 fallback: when NO wake pipe exists (`wake_rd < 0`, the `make_wake_pipe`
/// failed-under-fd-exhaustion path), a reader on a master that stays open with no data
/// and never EOFs must NOT park forever — it must time out and return `Idle` so the
/// owning session's fd-free stop flag can still tear it down. A bare blocking `read`
/// here is exactly the silent MEM-L2 re-open this guards against.
#[test]
fn read_or_wake_wakeless_fallback_times_out_instead_of_parking() {
    use std::time::{Duration, Instant};
    // A pipe read end whose write end lives for the whole test stands in for an
    // orphan-held master: never readable, never EOF.
    let mut m = [0i32; 2];
    // SAFETY: valid 2-int out-array for pipe(2).
    assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
    let (master_rd, master_wr) = (m[0], m[1]);

    let mut buf = [0u8; 16];
    let t0 = Instant::now();
    let out = read_or_wake(master_rd, &mut buf, -1);
    let elapsed = t0.elapsed();

    assert!(
        matches!(out, ReadOutcome::Idle),
        "wakeless fallback must return Idle on a quiet master, never block or read"
    );
    // It returned via the bounded poll timeout, not by parking indefinitely.
    // Two halves, because a bound scaled to the constant moves with it: at
    // 40x `WAKELESS_POLL_TIMEOUT_MS` a constant regressed to 15 s passes.
    // The constant's own range is pinned at
    // compile time (the doc's "reclaimed promptly": at most a second), and
    // the fixed bound below — a minute's hang detector (AGENTS.md), never a
    // latency budget, so a descheduled test thread is not read as a park —
    // catches a call site that stops honouring the constant and parks.
    const _: () = assert!(WAKELESS_POLL_TIMEOUT_MS > 0 && WAKELESS_POLL_TIMEOUT_MS <= 1_000);
    const BOUND: Duration = HANG_DETECTOR;
    assert!(
        elapsed < BOUND,
        "fallback must return within the poll timeout, not hang: {elapsed:?} >= {BOUND:?}"
    );

    // SAFETY: closing the two pipe fds this test owns.
    unsafe {
        libc::close(master_rd);
        libc::close(master_wr);
    }
}

/// PROOF-CARRYING DSU (RFC Rung 1b) — the FD-handoff mechanism, proven with REAL
/// syscalls: a PTY master survives `execve` IFF `FD_CLOEXEC` is cleared. Opens a
/// real pty pair, and forks+execs a child that reports whether the inherited fd is
/// open — the exact "the shell's master survives the seamless re-exec" contract.
/// The public bare pair hands back BOTH ends close-on-exec, as
/// [`open_pty_pair_cloexec`] opens them, so a caller has no `fcntl` of its
/// own to race another thread's fork with (the seamless fixture's window).
#[test]
fn open_pty_pair_hands_back_both_ends_close_on_exec() {
    let (master, slave) = open_pty_pair().expect("pty pair");
    for (end, fd) in [("master", master), ("slave", slave)] {
        // SAFETY: F_GETFD on a descriptor this test just opened.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(flags >= 0, "{end} flags");
        assert_ne!(flags & libc::FD_CLOEXEC, 0, "{end} is close-on-exec");
    }
    close_fd(slave);
    close_fd(master);
}

/// A master opener under test: `posix_openpt` is the argument.
#[cfg(target_os = "macos")]
type MasterOpener = fn(&mut dyn FnMut(libc::c_int) -> libc::c_int) -> io::Result<libc::c_int>;

/// What a failed `posix_openpt` leaves behind, for the scripted kernel.
#[cfg(target_os = "macos")]
fn set_errno(code: libc::c_int) {
    // SAFETY: `__error` returns this thread's live `errno` slot.
    unsafe { *libc::__error() = code };
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

/// Everything `opener` gets wrong about `posix_openpt`'s failures, played as
/// the kernel plays them (`-1`, `errno` set — libsyscall's `cerror`), each
/// script failing with its errnos in turn and then opening descriptor 1000.
#[cfg(target_os = "macos")]
fn master_open_violations(opener: MasterOpener) -> Vec<String> {
    const OPENED: libc::c_int = 1000;
    let play = |errnos: &[libc::c_int]| {
        let mut flags_seen = Vec::new();
        let got = opener(&mut |flags| {
            flags_seen.push(flags);
            let Some(&errno) = errnos.get(flags_seen.len() - 1) else {
                return OPENED;
            };
            set_errno(errno);
            -1
        });
        (got, flags_seen)
    };
    let mut wrong = Vec::new();
    // A lost allocation race is redriven, not reported.
    let (got, seen) = play(&[XNU_EREDRIVEOPEN; 3]);
    if got.as_ref().ok() != Some(&OPENED) || seen.len() != 4 {
        wrong.push(format!("3 lost races: {got:?} after {} opens", seen.len()));
    }
    // A race lost on every redrive is reported, naming itself, with -6.
    let lost = [XNU_EREDRIVEOPEN; PTMX_REDRIVES as usize + 1];
    let (got, seen) = play(&lost);
    match got {
        Err(err)
            if seen.len() == lost.len()
                && carried_errno(&err) == Some(XNU_EREDRIVEOPEN)
                && err.to_string().starts_with("posix_openpt")
                && err.to_string().contains("EREDRIVEOPEN") => {}
        got => wrong.push(format!(
            "every race lost: {got:?} after {} opens",
            seen.len()
        )),
    }
    // A real limit is reported at once, naming the step, errno and kind intact.
    for errno in [libc::ENXIO, libc::EMFILE, libc::ENFILE, libc::EAGAIN] {
        let (got, seen) = play(&[errno]);
        match got {
            Err(err)
                if seen.len() == 1
                    && carried_errno(&err) == Some(errno)
                    && err.kind() == io::Error::from_raw_os_error(errno).kind()
                    && err.to_string().starts_with("posix_openpt: ") => {}
            got => wrong.push(format!("errno {errno}: {got:?} after {} opens", seen.len())),
        }
    }
    // EINVAL: once more without O_CLOEXEC, which is all that changes.
    let (got, seen) = play(&[libc::EINVAL]);
    let first = libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC;
    if got.as_ref().ok() != Some(&OPENED) || seen != [first, first & !libc::O_CLOEXEC] {
        wrong.push(format!("EINVAL: {got:?} with flags {seen:x?}"));
    }
    wrong
}

/// The master open maps every `posix_openpt` failure to its true errno and
/// names the step; the race XNU leaks as errno -6 (`EREDRIVEOPEN`, seen as
/// `open_pty_pair: Os { code: -6 }` under load) is redriven.
/// NEGATIVE CONTROL: the construction this replaced — one open, an EINVAL
/// retry, any other `errno` returned bare — fails the same script.
#[cfg(target_os = "macos")]
#[test]
fn master_open_redrives_a_lost_race_and_names_every_failure() {
    fn current(open: &mut dyn FnMut(libc::c_int) -> libc::c_int) -> io::Result<libc::c_int> {
        open_ptmx(open)
    }
    fn replaced(open: &mut dyn FnMut(libc::c_int) -> libc::c_int) -> io::Result<libc::c_int> {
        let opened = open(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
        if opened >= 0 {
            return Ok(opened);
        }
        let first = io::Error::last_os_error();
        if first.raw_os_error() != Some(libc::EINVAL) {
            return Err(first);
        }
        let retry = open(libc::O_RDWR | libc::O_NOCTTY);
        if retry < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(retry)
    }
    assert_eq!(master_open_violations(current), Vec::<String>::new());
    let old = master_open_violations(replaced);
    assert!(
        old.iter().any(|w| w.starts_with("3 lost races"))
            && old.iter().any(|w| w.starts_with("errno ")),
        "the replaced opener must fail the race and step checks: {old:?}"
    );
}

/// Set only on the child [`alone`] starts, naming the test it runs.
#[cfg(target_os = "macos")]
const RUN_ALONE: &str = "ATERM_PTY_TEST_ALONE";

/// Run the calling test's body in a child of this test binary, alone — for
/// a test that changes something process-wide (`RLIMIT_NOFILE`): `true` in
/// that child, `false` here once it ran exactly that test and passed it.
/// `--exact` with a name that selects nothing exits 0 too, so its `1
/// passed` is required, and the name is libtest's own (the test's thread).
/// Output goes to a file, not a pipe: a pipe's end of file waits for every
/// copy of its write end, which a sibling test's fork may hold.
#[cfg(target_os = "macos")]
fn alone() -> bool {
    let test = std::thread::current()
        .name()
        .expect("libtest names each test's thread after it")
        .to_string();
    if std::env::var_os(RUN_ALONE).is_some_and(|run| run == test.as_str()) {
        return true;
    }
    let log = std::env::temp_dir().join(format!(
        "aterm-pty-alone-{}-{}.log",
        std::process::id(),
        test.replace("::", "-")
    ));
    let out = std::fs::File::create(&log).expect("the child's log");
    // `spawnfd`'s: the re-run holds its stdio and nothing else of this process,
    // not another test thread's pipe for as long as it runs.
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
        .args(["--exact", &test, "--nocapture", "--test-threads=1"])
        .env(RUN_ALONE, &test)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(out.try_clone().expect("the log, twice"))
        .stderr(out)
        .spawn()
        .expect("re-run this test alone");
    let status = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        std::time::Duration::from_millis(5),
        || child.try_wait().expect("the child's exit"),
    );
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_file(&log);
    assert!(
        status.is_some_and(|s| s.success()) && text.contains(" 1 passed;"),
        "{test}, alone: {status:?}\n{text}"
    );
    false
}

/// A LATER step's failure names that step and leaks no descriptor. With
/// the soft `RLIMIT_NOFILE` one above the lowest free number, the master
/// takes that number and the slave's open fails `EMFILE`: the error reads
/// "open of the slave: …", carries `EMFILE` with its kind, and the master
/// is closed again (the lowest free number is free). POSITIVE CONTROL: room
/// up to the next free number and the same opener hands back a pair, so
/// the limit, met at the slave, is the failure. The limit is per process:
/// run alone. The next free number is read, not assumed one higher: the
/// child inherits whatever inheritable descriptor a sibling test held when
/// it was spawned (measured: 2 runs in 240 under a parallel stress).
#[cfg(target_os = "macos")]
#[test]
fn a_failed_slave_open_names_its_step_and_leaks_no_master() {
    if !alone() {
        return;
    }
    let null = || {
        // SAFETY: opens a descriptor of this process's own; closed below.
        let fd = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        assert!(fd >= 0, "open /dev/null: {}", io::Error::last_os_error());
        fd
    };
    // The two lowest free numbers: where the master and the slave land.
    let lowest_free = || {
        let (first, second) = (null(), null());
        close_fd(second);
        close_fd(first);
        (first, second)
    };
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: a live out-parameter `getrlimit` fills.
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
        0
    );
    let soft = |cur: libc::rlim_t| {
        let set = libc::rlimit {
            rlim_cur: cur,
            rlim_max: limit.rlim_max,
        };
        // SAFETY: a live, initialised rlimit that keeps the hard limit.
        let rc = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &set) };
        assert_eq!(rc, 0, "soft limit {cur}: {}", io::Error::last_os_error());
    };
    let free = lowest_free();
    let room = |fd| libc::rlim_t::try_from(fd).expect("a descriptor number") + 1;
    soft(room(free.0));
    let refused = open_pty_pair();
    soft(limit.rlim_cur);
    let after = lowest_free();
    soft(room(free.1));
    let opened = open_pty_pair();
    soft(limit.rlim_cur);
    if let Ok((master, slave)) = opened {
        close_fd(slave);
        close_fd(master);
    }
    let err = refused.expect_err("no number left for the slave");
    assert!(
        err.to_string().starts_with("open of the slave: ")
            && carried_errno(&err) == Some(libc::EMFILE)
            && err.kind() == io::Error::from_raw_os_error(libc::EMFILE).kind(),
        "the slave's step, with EMFILE: {err:?}"
    );
    assert_eq!(after, free, "the master was closed on the slave's failure");
    assert!(
        opened.is_ok(),
        "POSITIVE CONTROL, room for both: {opened:?}"
    );
}

/// `ptsname_r` RETURNS its error number, and musl leaves `errno` alone: the
/// error comes from the return value, never from a stale `errno`; a -1 (the
/// older convention, and Apple's `ptsname_r`) still reads `errno`. The real
/// call cannot take the returned-number branch on macOS — the `libc` crate
/// declares no `ptsname_r` for Apple, and Apple's answers -1 — so the mapping
/// is played here with `errno` left stale by a failed `close`.
/// NEGATIVE CONTROL: the construction it replaced, `last_os_error` alone,
/// reports the stale `EBADF` for a returned `ERANGE`.
#[test]
fn ptsname_r_errors_come_from_its_return_value() {
    fn replaced(_rc: libc::c_int) -> io::Error {
        io::Error::last_os_error()
    }
    // `errno` as a failed call leaves it, then the mapping read at once.
    let after_stale = |map: fn(libc::c_int) -> io::Error, rc| {
        // SAFETY: closing no descriptor; it only sets `errno` (EBADF).
        assert_eq!(unsafe { libc::close(-1) }, -1);
        map(rc).raw_os_error()
    };
    assert_eq!(
        after_stale(ptsname_r_error, libc::ERANGE),
        Some(libc::ERANGE),
        "the returned number"
    );
    assert_eq!(
        after_stale(ptsname_r_error, -1),
        Some(libc::EBADF),
        "-1 reads errno"
    );
    assert_eq!(
        after_stale(replaced, libc::ERANGE),
        Some(libc::EBADF),
        "NEGATIVE CONTROL: errno alone reports the stale EBADF"
    );
}

#[test]
fn cloexec_controls_master_survival_across_exec() {
    // A real pty pair (no shell), from the seam's own opener rather than
    // `openpty`. The MASTER's flag is what this test toggles and asserts, so
    // the starting state is immaterial to it — but `openpty`'s SLAVE is
    // never close-on-exec, and this pair stays open for the whole test, so
    // using `openpty` here would park an inheritable pty slave in a
    // multi-threaded harness. Any concurrent `Command::spawn` (the sibling
    // leak-detection test does exactly that) would inherit it and keep that
    // pts alive after this test closed it — and when the device number was
    // recycled by the next test's session, the stranger's exit would hang up
    // THAT session. Measured: it did, killing an unrelated test's child with
    // SIGHUP ~1 run in 8. That is precisely the defect this file now fixes,
    // so the test scaffolding must not re-create it.
    let (master, slave) = open_pty_pair_cloexec(None, None).expect("pty pair");

    // fd_is_tty is a SANITY backstop, not a master discriminator: it accepts any
    // tty (master AND slave) and rejects the obviously-wrong (pipe / bad fd).
    let mut pipe = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let tty_master = fd_is_tty(master);
    let tty_slave = fd_is_tty(slave); // a slave IS a tty — documents the non-guarantee
    let tty_bad = fd_is_tty(-1);
    let tty_pipe = fd_is_tty(pipe[0]);

    // A tiny helper: fork+exec `/bin/test -e /dev/fd/<master>` and return whether
    // the fd was OPEN in the exec'd child (i.e. survived the exec). NOTE: libtest
    // runs tests on a THREAD POOL, so this process is multi-threaded at fork time.
    // That is safe here because the child touches ONLY async-signal-safe calls
    // (`execv`, `_exit`) before the image is replaced — it never allocates, locks,
    // or calls back into std. The parent reaps the child.
    let survived = |m: i32| -> bool {
        let path = std::ffi::CString::new("/bin/test").unwrap();
        let arg0 = std::ffi::CString::new("test").unwrap();
        let arg1 = std::ffi::CString::new("-e").unwrap();
        let arg2 = std::ffi::CString::new(format!("/dev/fd/{m}")).unwrap();
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            let argv = [
                arg0.as_ptr(),
                arg1.as_ptr(),
                arg2.as_ptr(),
                std::ptr::null(),
            ];
            unsafe {
                libc::execv(path.as_ptr(), argv.as_ptr());
                libc::_exit(127); // execv only returns on failure
            }
        }
        let mut status = 0i32;
        unsafe { libc::waitpid(pid, &mut status, 0) };
        // WIFEXITED && WEXITSTATUS == 0  ⇒ `test -e` found the fd open.
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0
    };

    // CLOEXEC set (the isolation default the spawn seam opens the master with) ⇒ the
    // master is CLOSED at exec (a naive re-exec would orphan the shell). Cleared ⇒
    // it SURVIVES (the seamless handoff's contract).
    set_cloexec(master, true).unwrap();
    let survives_when_cloexec = survived(master);
    set_cloexec(master, false).unwrap();
    let survives_when_cleared = survived(master);

    // The surviving master is still a live, usable pty (write master → read slave).
    set_nonblocking(slave, true).unwrap();
    write_all(master, b"ping\n");
    // Polled under the hang detector, not a spin count: Linux hands master
    // writes to the line discipline on a workqueue, so the bytes can land
    // after a thousand back-to-back reads on a loaded machine.
    let mut buf = [0u8; 8];
    let deadline = std::time::Instant::now() + HANG_DETECTOR;
    let mut got = read(slave, &mut buf);
    while got <= 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(1));
        got = read(slave, &mut buf);
    }

    // Close every fd BEFORE asserting, so a failed assertion can't leak them.
    for fd in [master, slave, pipe[0], pipe[1]] {
        unsafe { libc::close(fd) };
    }

    assert!(tty_master, "the master is a tty");
    assert!(
        tty_slave,
        "a pty SLAVE is also a tty (fd_is_tty is not master-only)"
    );
    assert!(!tty_bad, "a bad fd is not a tty");
    assert!(!tty_pipe, "a pipe end is not a tty");
    assert!(
        !survives_when_cloexec,
        "a CLOEXEC master does NOT survive exec"
    );
    assert!(
        survives_when_cleared,
        "a cleared-CLOEXEC master SURVIVES exec (seamless handoff)"
    );
    assert!(got > 0, "the re-adoptable master still drives its slave");
}

// SEC-2: a confinement failure in the child must FAIL CLOSED. We force the
// sandbox `apply` to fail by handing it an UNTRUSTED `Cap<Sandbox>` (its gate
// requires Trusted+), so the child takes the `_exit(126)` path BEFORE exec and
// the parent returns an error instead of a master fd for an unconfined shell.
#[test]
fn sandbox_apply_failure_in_child_fails_closed_no_unconfined_shell() {
    // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    // A valid spawn cap (passes the PARENT gate) but a too-weak sandbox cap
    // (fails the CHILD's `apply` gate) — exactly the silent-unconfined hole.
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
    assert!(
        err.to_string().contains("fail-closed"),
        "error should describe the fail-closed confinement: {err}",
    );
}

#[test]
fn spawned_master_is_close_on_exec() {
    // Without FD_CLOEXEC a later session's child shell would inherit every
    // prior session's still-open master — a cross-session input-injection /
    // output-exfiltration channel.
    // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let master = spawn_shell(
        24,
        80,
        &spawn_cap,
        &sandbox_cap,
        &[],
        None, // shell_override
        None, // shell_args
        None,
        None,
        None,
        None,
    )
    .expect("a normal shell must spawn with a Trusted sandbox cap");
    assert!(master >= 0, "master fd must be valid, got {master}");
    // SAFETY: querying fd flags on a valid fd.
    let flags = unsafe { libc::fcntl(master, libc::F_GETFD) };
    assert!(flags >= 0, "F_GETFD must succeed on the master fd");
    assert!(
        flags & libc::FD_CLOEXEC != 0,
        "PTY master must be FD_CLOEXEC so it never leaks into a later session's shell"
    );
    // SAFETY: closing the master tears down the child's controlling tty.
    unsafe {
        libc::close(master);
    }
}

// ---- PTY pair creation: close-on-exec FROM BIRTH (the forkpty window) ----

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

/// The winsize `fd` reports, as `(rows, cols)`.
fn win_size(fd: libc::c_int, what: &str) -> (u16, u16) {
    // SAFETY: `TIOCGWINSZ` fills the zeroed, stack-owned out-param.
    unsafe {
        let mut w: libc::winsize = std::mem::zeroed();
        assert_eq!(
            libc::ioctl(fd, libc::TIOCGWINSZ, &mut w),
            0,
            "TIOCGWINSZ on {what} (fd {fd}): {}",
            io::Error::last_os_error()
        );
        (w.ws_row, w.ws_col)
    }
}

/// [`byte_crosses`]'s budget for a byte that MUST cross.
const CROSS_MUST: std::time::Duration = std::time::Duration::from_secs(60);
/// [`byte_crosses`]'s window for a byte that must NOT cross.
const CROSS_MUST_NOT: std::time::Duration = std::time::Duration::from_millis(500);

/// Does a byte written into `slave` come out of `master` within `budget`?
///
/// The ONLY observation that separates a real pty pair from two
/// correctly-flagged descriptors onto DIFFERENT terminals. Bounded by a
/// poll, so a broken pairing fails the test instead of hanging it. A
/// POSITIVE check passes [`CROSS_MUST`] (a hang detector: the poll returns
/// the moment the byte lands); a NEGATIVE control passes [`CROSS_MUST_NOT`],
/// a window that can only go vacuous under load, never red.
fn byte_crosses(
    slave: libc::c_int,
    master: libc::c_int,
    byte: u8,
    budget: std::time::Duration,
) -> bool {
    // SAFETY: `slave` is a live pts fd the caller owns; `write` reads
    // exactly the one byte living at `byte`'s address.
    let wrote = unsafe { libc::write(slave, ptr::from_ref(&byte).cast(), 1) };
    if wrote != 1 {
        return false;
    }
    if poll_revents(master, budget) & libc::POLLIN == 0 {
        return false;
    }
    let mut buf = [0u8; 16];
    // SAFETY: `read` fills at most `buf.len()` bytes of this live, owned
    // buffer, and we pass exactly that length.
    let got = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
    let Ok(n) = usize::try_from(got) else {
        return false;
    };
    buf[..n].contains(&byte)
}

/// `poll(fd, POLLIN)` for at most `budget`, returning the revents once any
/// arrive (or 0 on timeout). Mirrors `handoff_masters_closed`'s poll shape
/// exactly, including `events: POLLIN` — with `events: 0` macOS reports
/// nothing at all.
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

/// THE PLATFORM FACT THE WHOLE SEAM RESTS ON: `O_CLOEXEC` passed to the
/// `open(2)` that CREATES a pty descriptor is honored, so the descriptor is
/// close-on-exec before it exists anywhere else. That is the only window-free
/// way to get there — an `fcntl` afterwards leaves a gap in which a fork on
/// another thread yields an exec-surviving copy.
///
/// This is not redundant with the tests below: `open_pty_pair_cloexec` will
/// SILENTLY fall back to `fcntl` (correct, but with the narrow window back)
/// if a platform ever ignores the flag, and every other assertion here would
/// still pass. This test is what says so out loud. Both halves are checked,
/// so "the flag was set" cannot be trivially true of every descriptor.
#[test]
fn pty_open_honors_the_cloexec_open_flag() {
    // NEGATIVE CONTROL: the same call WITHOUT the flag must NOT be
    // close-on-exec. This is the state `openpty`/`forkpty` leave both ends
    // in, i.e. the defect itself, asserted rather than assumed. Each open is
    // the bare `posix_openpt` with its flag word fixed here, redriven by
    // `open_ptmx` if it loses XNU's allocation race to a parallel test.
    // SAFETY: `posix_openpt` takes only a flag word; both fds are closed below.
    let plain = open_ptmx(|_| unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) })
        .expect("posix_openpt failed");
    let flagged = open_ptmx(|_| {
        // SAFETY: as above, with the flag added.
        unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) }
    })
    .expect("posix_openpt(O_CLOEXEC) failed — the seam would fall back to a racy fcntl");
    let plain_flags = fd_flags(plain, "master opened without O_CLOEXEC");
    let flagged_flags = fd_flags(flagged, "master opened with O_CLOEXEC");

    // Same question for the SLAVE open, which is the end `forkpty` never
    // flagged at all. Open the SAME pts twice off the flagged master.
    // SAFETY: `flagged` is a live /dev/ptmx fd; both are ioctls on it.
    assert_eq!(unsafe { libc::grantpt(flagged) }, 0, "grantpt");
    // SAFETY: as above.
    assert_eq!(unsafe { libc::unlockpt(flagged) }, 0, "unlockpt");
    let name = pts_name(flagged).expect("pts_name must resolve the slave path");
    // SAFETY: `name` is the NUL-terminated pts path just reported for `flagged`.
    let slave_plain = unsafe { libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY) };
    // SAFETY: as above, with the flag added.
    let slave_flagged = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    assert!(
        slave_plain >= 0 && slave_flagged >= 0,
        "opening the pts failed"
    );
    let slave_plain_flags = fd_flags(slave_plain, "slave opened without O_CLOEXEC");
    let slave_flagged_flags = fd_flags(slave_flagged, "slave opened with O_CLOEXEC");

    // Close everything BEFORE asserting so a failure cannot leak descriptors.
    for fd in [plain, flagged, slave_plain, slave_flagged] {
        // SAFETY: each is a live fd this test opened and owns.
        unsafe { libc::close(fd) };
    }

    assert_eq!(
        plain_flags & libc::FD_CLOEXEC,
        0,
        "control: posix_openpt WITHOUT O_CLOEXEC must leave the master \
         inheritable — this is the forkpty state the seam replaces"
    );
    assert_ne!(
        flagged_flags & libc::FD_CLOEXEC,
        0,
        "posix_openpt must HONOR O_CLOEXEC — otherwise the master is only \
         close-on-exec after a follow-up fcntl, and a concurrent \
         fork+exec in that gap still inherits it"
    );
    assert_eq!(
        slave_plain_flags & libc::FD_CLOEXEC,
        0,
        "control: opening the pts WITHOUT O_CLOEXEC must leave it inheritable"
    );
    assert_ne!(
        slave_flagged_flags & libc::FD_CLOEXEC,
        0,
        "opening the pts must HONOR O_CLOEXEC — the slave is the end \
         forkpty never flagged at all"
    );
}

/// The pixel arithmetic itself: `(cols * cell_w, rows * cell_h)`, with
/// "unknown" and the `u16` ceiling handled explicitly rather than by wrapping.
#[test]
fn winsize_pixels_multiplies_the_cell_metrics_and_saturates() {
    // The measured 0.61.0 window: 24x80 cells of 9x17 px -> 720x408, which is
    // exactly what `CSI 14t` answers on the same window (`ESC[4;408;720t`).
    assert_eq!(winsize_pixels(24, 80, Some((9, 17))), (720, 408));
    // No metrics: zero, the honest "I cannot tell you" — never a guess.
    assert_eq!(winsize_pixels(24, 80, None), (0, 0));
    // Half a geometry is not a geometry: a zero axis zeroes BOTH fields, so no
    // consumer can divide by it.
    assert_eq!(winsize_pixels(24, 80, Some((0, 17))), (0, 0));
    assert_eq!(winsize_pixels(24, 80, Some((9, 0))), (0, 0));
    // The kernel's fields are u16: a monster window reports the cap, never a
    // wrapped small number (a thumbnail on a wall display).
    assert_eq!(
        winsize_pixels(u16::MAX, u16::MAX, Some((9, 17))),
        (u16::MAX, u16::MAX)
    );
}

/// The defect: `ws_xpixel`/`ws_ypixel` were hard-coded 0 at EVERY winsize
/// construction site, so every tool that sizes its output from
/// `ioctl(TIOCGWINSZ)` — chafa, timg, viu, img2sixel, libsixel's own probe —
/// got nothing out of aterm, at spawn and after every resize alike.
///
/// Read back through the kernel (not from our own struct): a pty pair carries
/// ONE winsize, so `TIOCGWINSZ` on the master returns exactly the bytes the
/// child's `TIOCGWINSZ` on its stdin returns. The NEGATIVE CONTROL in the same
/// run — the identical spawn with no metrics — reproduces the old zeros, so a
/// pass cannot be vacuous.
#[test]
fn a_spawned_child_can_read_real_pixel_geometry_from_the_kernel() {
    // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let exec: Vec<std::ffi::OsString> = vec!["/bin/sh".into(), "-c".into(), "exec sleep 30".into()];
    let spawn = |cell_px: Option<(u16, u16)>| {
        spawn_shell_with_pid_cell_px(
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
            cell_px,
        )
        .expect("the parked probe must spawn")
    };
    let read_ws = |master: i32| {
        // SAFETY: `master` is a live pty master fd; `TIOCGWINSZ` fills the
        // caller's `winsize`.
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::ioctl(master, libc::TIOCGWINSZ, &mut ws) },
            0,
            "TIOCGWINSZ on the master must succeed"
        );
        ws
    };

    // 1) AT SPAWN — before anything resizes anything.
    let sh = spawn(Some((9, 17)));
    let ws = read_ws(sh.master);
    assert_eq!(
        (ws.ws_row, ws.ws_col),
        (41, 137),
        "the cell grid still lands"
    );
    assert_eq!(
        (ws.ws_xpixel, ws.ws_ypixel),
        (137 * 9, 41 * 17),
        "the child's FIRST winsize must carry the real pixel geometry"
    );

    // 2) ON RESIZE — the path the GUI drives on every window drag.
    resize_with_cell_px(sh.master, 24, 80, Some((9, 17)));
    let ws = read_ws(sh.master);
    assert_eq!((ws.ws_row, ws.ws_col), (24, 80));
    assert_eq!((ws.ws_xpixel, ws.ws_ypixel), (720, 408));

    // 3) A FONT-SIZE change at an unchanged grid still moves the pixels — the
    // case a rows/cols-only comparison would have missed entirely.
    resize_with_cell_px(sh.master, 24, 80, Some((18, 34)));
    let ws = read_ws(sh.master);
    assert_eq!((ws.ws_row, ws.ws_col), (24, 80));
    assert_eq!((ws.ws_xpixel, ws.ws_ypixel), (1440, 816));

    hangup(sh.pid);
    // SAFETY: `master` is the fd this test was handed.
    unsafe { libc::close(sh.master) };

    // NEGATIVE CONTROL: no metrics ⇒ the pre-fix zeros, at spawn and on resize.
    let bare = spawn(None);
    let ws = read_ws(bare.master);
    assert_eq!(
        (ws.ws_xpixel, ws.ws_ypixel),
        (0, 0),
        "without metrics the fields must stay zero — the check above is not \
         reading a kernel default"
    );
    resize(bare.master, 24, 80);
    let ws = read_ws(bare.master);
    assert_eq!((ws.ws_xpixel, ws.ws_ypixel), (0, 0));
    hangup(bare.pid);
    // SAFETY: `master` is the fd this test was handed.
    unsafe { libc::close(bare.master) };
}

/// BOTH ends of the pair this seam hands the spawn are close-on-exec from the
/// moment they exist, and the pair is a REAL, correctly-configured pty — not
/// a pair of flags on nothing. The precondition is asserted alongside the
/// property: two distinct live descriptors, both ttys, carrying the requested
/// winsize and the requested termios deltas, readable from both ends.
#[test]
fn pty_pair_is_close_on_exec_from_birth() {
    let want = build_spawn_termios(kernel_default_termios().expect("kernel termios probe"));
    let ws = libc::winsize {
        ws_row: 41,
        ws_col: 137,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let (master, slave) =
        open_pty_pair_cloexec(Some(&want), Some(&ws)).expect("pty pair must open");

    // PRECONDITION — a real pair actually came into existence. Without this
    // the FD_CLOEXEC assertions could be true of two closed/bogus numbers.
    assert!(master >= 0 && slave >= 0, "both ends must be live fds");
    assert_ne!(master, slave, "the two ends must be distinct descriptors");
    let master_is_tty = fd_is_tty(master);
    let slave_is_tty = fd_is_tty(slave);
    let master_flags = fd_flags(master, "pty master");
    let slave_flags = fd_flags(slave, "pty slave");
    let slave_ws = win_size(slave, "pty slave");
    let master_ws = win_size(master, "pty master");
    // SAFETY: `tcgetattr` fills the zeroed out-param from the live slave.
    let slave_termios = unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        assert_eq!(libc::tcgetattr(slave, &mut t), 0, "tcgetattr(slave)");
        t
    };

    // NEGATIVE CONTROL, on this same machine and in this same process: the
    // `openpty` this replaced leaves BOTH ends inheritable. It is what makes
    // the assertions above non-trivial. `openpty` opens its master with the
    // same `posix_openpt`, so it loses XNU's allocation race the same way and
    // is redriven the same way (`open_ptmx`).
    let (mut om, mut os) = (-1i32, -1i32);
    open_ptmx(|_| {
        // SAFETY: valid out-params; the null name/termios/winsize form. Both
        // fds are closed below.
        let orc = unsafe {
            libc::openpty(
                &mut om,
                &mut os,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if orc == 0 { om } else { -1 }
    })
    .expect("control openpty failed");
    let open_pty_master_flags = fd_flags(om, "openpty master");
    let open_pty_slave_flags = fd_flags(os, "openpty slave");

    for fd in [master, slave, om, os] {
        // SAFETY: each is a live fd this test owns.
        unsafe { libc::close(fd) };
    }

    assert!(master_is_tty && slave_is_tty, "both ends must be ttys");
    assert_ne!(
        master_flags & libc::FD_CLOEXEC,
        0,
        "the MASTER must be close-on-exec from birth: an unrelated \
         fork+exec must never inherit a readable/writable handle on \
         another session's terminal"
    );
    assert_ne!(
        slave_flags & libc::FD_CLOEXEC,
        0,
        "the SLAVE must be close-on-exec from birth: this is the end \
         forkpty never flagged at all, and an inherited copy both holds \
         the pty open (suppressing the master's hangup) and is a live \
         bidirectional channel onto the session"
    );
    assert_eq!(
        (
            open_pty_master_flags & libc::FD_CLOEXEC,
            open_pty_slave_flags & libc::FD_CLOEXEC
        ),
        (0, 0),
        "control: openpty (what forkpty uses) leaves BOTH ends inheritable — \
         if this ever changes, the assertions above stopped being meaningful"
    );
    // The winsize/termios were applied to the slave in the parent, before any
    // fork could observe them — the atomicity property the seam relies on.
    assert_eq!(slave_ws, (41, 137), "winsize must be live on the slave");
    assert_eq!(master_ws, (41, 137), "and visible from the master");
    assert_ne!(
        slave_termios.c_iflag & libc::IUTF8,
        0,
        "the spawn termios must be applied at slave-open time"
    );
    // SAFETY: `cfgetospeed` only reads the termios.
    assert_eq!(unsafe { libc::cfgetospeed(&slave_termios) }, libc::B230400);
}

/// The two descriptors this seam returns are ACTUALLY each other's ends.
///
/// Everything else asserted about the pair is a FLAG — close-on-exec,
/// `O_NOCTTY`, termios, winsize — and every one of those assertions would
/// still pass if the slave belonged to a DIFFERENT terminal. That is not
/// hypothetical: reaching the slave by path (`ptsname_r` →
/// `open("/dev/pts/N")`) was MEASURED on Linux 6.8 returning a live,
/// correctly-flagged pts from another devpts instance — one of the reasons
/// [`open_pts_slave`] asks the kernel for the peer instead of asking the
/// filesystem for a name. So prove the pairing the one way flags cannot
/// fake, and prove the proof is non-vacuous with a pair that must NOT cross.
#[test]
fn pty_pair_ends_are_peers_of_each_other() {
    let (master, slave) = open_pty_pair_cloexec(None, None).expect("pty pair");
    let (other_master, other_slave) = open_pty_pair_cloexec(None, None).expect("second pty pair");

    let paired = byte_crosses(slave, master, b'A', CROSS_MUST);
    // NEGATIVE CONTROL: an unrelated pair's slave must not reach this master.
    let crossed = byte_crosses(other_slave, master, b'B', CROSS_MUST_NOT);

    // Close BEFORE asserting so a failure cannot leak descriptors.
    for fd in [master, slave, other_master, other_slave] {
        // SAFETY: each is a live fd this test owns.
        unsafe { libc::close(fd) };
    }

    assert!(
        paired,
        "the slave must be the peer OF THIS MASTER: a byte written into it \
         has to come out of the master handed back alongside it"
    );
    assert!(
        !crossed,
        "control: a foreign pty's slave must NOT reach this master — if it \
         could, the positive assertion above would prove nothing"
    );
}

/// Linux takes the slave FROM THE MASTER, not from a path.
///
/// `ioctl(TIOCGPTPEER)` returns the peer of this exact ptmx with the
/// `open(2)` flag word applied, so the slave is close-on-exec from birth AND
/// cannot be a descriptor onto another devpts instance's terminal — the two
/// failure modes measured for the path route ([`open_pts_slave`]). On a
/// kernel too old to know the ioctl (pre-4.13) the seam falls back to the
/// path, so this asserts THAT route is a true peer too rather than
/// pretending the ioctl exists everywhere.
#[cfg(target_os = "linux")]
#[test]
fn linux_slave_comes_from_the_master_via_tiocgptpeer() {
    // SAFETY: `posix_openpt` takes only a flag word; the fd is closed below.
    let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
    assert!(master >= 0, "posix_openpt: {}", io::Error::last_os_error());
    // SAFETY: `master` is a live /dev/ptmx descriptor; both are ioctls on it.
    assert_eq!(unsafe { libc::grantpt(master) }, 0, "grantpt");
    // SAFETY: as above.
    assert_eq!(unsafe { libc::unlockpt(master) }, 0, "unlockpt");

    // SAFETY: `TIOCGPTPEER` takes its argument BY VALUE (an `open(2)` flag
    // word, not a pointer) and returns a fresh descriptor or -1.
    let peer = unsafe {
        libc::ioctl(
            master,
            libc::TIOCGPTPEER,
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    let peer_errno = io::Error::last_os_error();

    // NEGATIVE CONTROL: the ioctl is specific to a ptmx. If it handed out a
    // descriptor for an ordinary pipe, its success above would mean nothing.
    let mut pipe_fds = [0 as libc::c_int; 2];
    // SAFETY: `pipe` fills exactly the two-element array it is given.
    assert_eq!(unsafe { libc::pipe(pipe_fds.as_mut_ptr()) }, 0, "pipe");
    // SAFETY: as the peer call above, on a descriptor that is not a ptmx.
    let non_ptmx = unsafe {
        libc::ioctl(
            pipe_fds[0],
            libc::TIOCGPTPEER,
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };

    // Whichever route this kernel supports, the seam's own opener must end
    // up with a close-on-exec descriptor that is a TRUE peer of `master`.
    let slave = open_pts_slave(master).expect("the slave must open by SOME route");
    let slave_flags = fd_flags(slave, "pts slave");
    let slave_is_tty = fd_is_tty(slave);
    let slave_is_peer = byte_crosses(slave, master, b'P', CROSS_MUST);

    // Close BEFORE asserting so a failure cannot leak descriptors.
    for fd in [master, slave, pipe_fds[0], pipe_fds[1]] {
        // SAFETY: each is a live fd this test owns.
        unsafe { libc::close(fd) };
    }
    if peer >= 0 {
        // SAFETY: the peer descriptor this test opened and still owns.
        unsafe { libc::close(peer) };
    }
    if non_ptmx >= 0 {
        // SAFETY: as above, on the (unexpected) pipe-derived descriptor.
        unsafe { libc::close(non_ptmx) };
    }

    assert!(
        non_ptmx < 0,
        "control: TIOCGPTPEER must refuse a non-ptmx descriptor"
    );
    if peer < 0 {
        // Only "this kernel has no such ioctl" may send the seam down the
        // path fallback. Any other errno means the primary route broke.
        assert!(
            matches!(peer_errno.raw_os_error(), Some(libc::EINVAL | libc::ENOTTY)),
            "TIOCGPTPEER must either work or be unsupported (pre-4.13 \
             kernel), not fail for some other reason: {peer_errno}"
        );
    }
    assert!(slave_is_tty, "the slave must be a tty");
    assert_ne!(
        slave_flags & libc::FD_CLOEXEC,
        0,
        "TIOCGPTPEER applies the open(2) flag word it is given, so the peer \
         is close-on-exec from birth exactly as the path open is"
    );
    assert!(
        slave_is_peer,
        "the descriptor must be the peer OF THIS MASTER — the property a \
         path lookup cannot guarantee once /dev/pts is not the devpts this \
         master came from"
    );
}

/// Run an UNRELATED `/bin/sh` that reports which of `fds` it was handed
/// (`got:<n>` for each), then runs `body` and prints `end`; its report.
///
/// Started by [`aterm_uds::spawnfd::transparent`], the one spawn that hands
/// a child every descriptor this process holds without `FD_CLOEXEC` on
/// every std: a std `Command` child gets fds 0-2 alone under Trust's SP-1
/// std (and the whole table under a std before it), so it cannot carry a leak on
/// both. The `/dev/fd` probes run BEFORE any redirection, so no descriptor
/// the shell opens for one can sit at a probed number. The child starts on
/// this process's stdio; after the probes one `exec` (which keeps no saved
/// copies) points it at `/dev/null`, a report file and `/dev/null`.
fn unrelated_exec_report(tag: &str, fds: &[libc::c_int], body: &str) -> String {
    let probed: String = fds.iter().map(|fd| format!(" {fd}")).collect();
    let script = format!(
        "got=; for f in{probed}; do [ -e /dev/fd/$f ] && got=\"$got got:$f\"; done; \
         exec </dev/null >\"$1\" 2>/dev/null; \
         printf '%s ran ' \"$got\"; {body}printf 'end'"
    );
    let path = std::env::temp_dir().join(format!(
        "aterm-pty-unrelated-exec-{}-{tag}",
        std::process::id()
    ));
    let status = aterm_uds::spawnfd::transparent("/bin/sh")
        .args(["-c", &script, "sh"])
        .arg(&path)
        .status()
        .expect("the unrelated helper must run");
    let report = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    assert!(
        status.success(),
        "the unrelated helper: {status}; report: {report:?}"
    );
    report
}

/// THE DEFECT, as a deterministic regression: while a session's pty pair is
/// open in this (multi-threaded) parent, an UNRELATED spawn that hands its
/// child every descriptor without `FD_CLOEXEC` ([`unrelated_exec_report`])
/// must inherit NEITHER end.
///
/// Deterministic by construction, with no race and no timing: the pair is
/// open across the whole spawn, so anything inheritable WOULD be inherited.
///
/// NON-VACUITY is carried by a control fd in the same run: a plain `dup` of
/// the slave, which POSIX defines to clear `FD_CLOEXEC`, is handed to the
/// same helper. It must be inherited AND must be able to write onto this
/// session's terminal — which simultaneously proves the detector can see a
/// leak at all, and reproduces the actual harm (forged output injected into
/// aterm's render path from a process that was never given a terminal).
#[test]
fn an_unrelated_exec_inherits_neither_end_of_a_live_pty() {
    use std::time::Duration;
    let ws = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let (master, slave) = open_pty_pair_cloexec(None, Some(&ws)).expect("pty pair");
    // The control leak: `dup` never copies FD_CLOEXEC, so this is a slave
    // handle in exactly the state forkpty left the real one in.
    // SAFETY: `slave` is live; `dup` returns a new fd onto the same pty.
    let leaked = unsafe { libc::dup(slave) };
    assert!(leaked >= 0, "dup(slave) failed");
    assert_eq!(
        fd_flags(leaked, "control dup of the slave") & libc::FD_CLOEXEC,
        0,
        "PRECONDITION: the control fd must be inheritable, or it proves nothing"
    );

    // The unrelated process: reports which of the three fd numbers it was
    // actually handed, then writes a marker through each. Its stdout is a
    // file, so nothing it prints can be confused with pty traffic.
    let report = unrelated_exec_report(
        "pty",
        &[master, slave, leaked],
        &format!(
            "{{ printf 'LEAKMARK' >&{leaked}; }} 2>/dev/null; \
             {{ printf 'SLAVEMARK' >&{slave}; }} 2>/dev/null; \
             {{ printf 'MASTERMARK' >&{master}; }} 2>/dev/null; "
        ),
    );
    // Parse the reported fd NUMBERS rather than substring-matching them:
    // with concurrent tests the descriptors reach two digits, and "got:3"
    // is a substring of "got:33" — a false leak report waiting to happen.
    let inherited: Vec<libc::c_int> = report
        .split_whitespace()
        .filter_map(|tok| tok.strip_prefix("got:"))
        .filter_map(|n| n.parse::<libc::c_int>().ok())
        .collect();

    // Drain whatever reached our terminal. A hang detector for the POSITIVE
    // control (the helper has exited, so LEAKMARK is already queued and the
    // loop leaves the moment it is read); the negatives below read the same
    // bytes, which the helper wrote before it could exit.
    set_nonblocking(master, true).expect("nonblocking master");
    let mut seen = Vec::new();
    // `None` is not judged here: the LEAKMARK assert below names it.
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

    for fd in [leaked, slave, master] {
        // SAFETY: each is a live fd this test owns.
        unsafe { libc::close(fd) };
    }

    // PRECONDITION: the helper really ran and really reported.
    assert!(
        report.contains("ran") && report.contains("end"),
        "the unrelated helper did not run to completion; report: {report:?}"
    );
    // NON-VACUITY: an inheritable slave IS inherited, and IS a live channel
    // onto this session's terminal.
    assert!(
        inherited.contains(&leaked),
        "control: an fd WITHOUT FD_CLOEXEC must be inherited across exec, \
         else this test cannot detect a leak at all; report: {report:?}"
    );
    assert!(
        seen.contains("LEAKMARK"),
        "control: a leaked slave lets an unrelated process forge output into \
         this session's terminal; master saw: {seen:?}"
    );
    // THE PROPERTY.
    assert!(
        !inherited.contains(&master),
        "an unrelated exec'd process inherited the pty MASTER (fd {master}); \
         report: {report:?}"
    );
    assert!(
        !inherited.contains(&slave),
        "an unrelated exec'd process inherited the pty SLAVE (fd {slave}); \
         report: {report:?}"
    );
    assert!(
        !seen.contains("SLAVEMARK") && !seen.contains("MASTERMARK"),
        "an unrelated exec'd process wrote onto this session's terminal \
         through an inherited pty end; master saw: {seen:?}"
    );
}

// ---- The EXEC-STATUS CHANNEL: the window, and the bound behind it ----

/// Build the ORIGINAL, RACY carrier — `pipe(2)` + two `fcntl(F_SETFD)` —
/// so the tests below can reproduce the defect deliberately.
///
/// This is the shape `open_exec_status_channel` now only reaches as a
/// last-resort fallback. Reproducing it by hand is the point: the fix makes
/// the leak unreachable through the real seam, so the only honest way to test
/// that the BOUND still ends the wait is to re-create the leak on purpose.
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

/// Is the channel still WITHOUT a verdict after `grace`?
///
/// `true` means someone other than us holds a write end: every copy we own is
/// closed, so if nobody else had one the read would answer EOF (0) at once.
/// This is the precondition that makes the bound tests non-vacuous — it is
/// exactly the state in which the ORIGINAL blocking `read` was measured
/// parking for the stranger's entire lifetime (5 s, 20 s, 65 s).
fn eof_is_suppressed(rd: libc::c_int, grace: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + grace;
    while std::time::Instant::now() < deadline {
        if read_exec_status_now(rd).is_some() {
            return false; // a verdict arrived — nothing is suppressing EOF
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    true
}

/// Spawn an unrelated process that INHERITS `fd` and holds it open.
///
/// Started by [`aterm_uds::spawnfd::transparent`], which hands its child
/// every descriptor this process holds without `FD_CLOEXEC` on every std,
/// as a std `Command`'s `posix_spawn(2)` did before Trust's SP-1 — so any
/// descriptor without the flag rides straight through its `exec`, the whole
/// defect. Not a std `Command`: under SP-1 its child gets fds 0-2 alone and
/// the stranger would hold nothing. The child starts on this process's
/// stdio and points it at `/dev/null` as it becomes `sleep`.
fn stranger_holding(fd: libc::c_int) -> aterm_uds::spawnfd::TransparentChild {
    assert_eq!(
        fd_flags(fd, "the fd handed to the stranger") & libc::FD_CLOEXEC,
        0,
        "PRECONDITION: the fd must be inheritable, or the stranger cannot \
         receive it and the test proves nothing"
    );
    aterm_uds::spawnfd::transparent("/bin/sh")
        .arg("-c")
        .arg("exec sleep 600 </dev/null >/dev/null 2>&1")
        .spawn()
        .expect("the unrelated helper must spawn")
}

/// THE FIX: the exec-status channel's two ends are close-on-exec FROM BIRTH.
///
/// The carrier is asserted to be an ATOMIC one, not the racy fallback,
/// because the flag alone is not the property that matters — `pipe(2)` plus
/// two `fcntl`s ends up with the same two flags set and is precisely the
/// defect. What matters is that no unflagged instant ever existed, and the
/// only machine-checkable proxy for that from here is WHICH carrier ran.
///
/// NON-VACUITY is carried by a plain `pipe(2)` in the same run: it must show
/// both ends unflagged, or this test could not tell the two apart at all.
#[test]
fn exec_status_channel_is_close_on_exec_from_birth() {
    let (rd, wr, carrier) = open_exec_status_channel().expect("the exec-status channel must open");

    // The control: a plain `pipe(2)` carrier.
    let mut racy = [0 as libc::c_int; 2];
    // SAFETY: `pipe` fills exactly the two-element array it is given.
    assert_eq!(unsafe { libc::pipe(racy.as_mut_ptr()) }, 0, "pipe");
    let racy_flags = (
        fd_flags(racy[0], "control pipe read end"),
        fd_flags(racy[1], "control pipe write end"),
    );

    let flags = (
        fd_flags(rd, "status read end"),
        fd_flags(wr, "status write end"),
    );
    // Close BEFORE asserting so a failure cannot leak descriptors.
    for fd in [rd, wr, racy[0], racy[1]] {
        // SAFETY: each is a live fd this test owns.
        unsafe { libc::close(fd) };
    }

    assert_eq!(
        (
            racy_flags.0 & libc::FD_CLOEXEC,
            racy_flags.1 & libc::FD_CLOEXEC
        ),
        (0, 0),
        "control: `pipe(2)` must return BOTH ends unflagged — that unflagged \
         instant IS the defect, and if it were not observable here this test \
         would be comparing nothing"
    );
    assert_ne!(
        flags.0 & libc::FD_CLOEXEC,
        0,
        "the status READ end must be close-on-exec"
    );
    assert_ne!(
        flags.1 & libc::FD_CLOEXEC,
        0,
        "the status WRITE end must be close-on-exec — this is the end whose \
         exec-close IS the success signal"
    );
    assert_ne!(
        carrier,
        ExecStatusCarrier::RacyPipe,
        "the channel must come from a carrier that flags both ends AT BIRTH \
         ({:?} on this platform), not from `pipe(2)` + `fcntl`, which sets \
         the same flags a moment too late",
        if cfg!(target_os = "linux") {
            "pipe2(O_CLOEXEC)"
        } else {
            "an unlinked O_CLOEXEC fifo"
        }
    );
}

/// THE DEFECT, as a deterministic regression: an unrelated spawn that hands
/// its child every descriptor without `FD_CLOEXEC`
/// ([`unrelated_exec_report`]), running while a status channel is open,
/// must inherit NEITHER end.
///
/// Deterministic by construction — the channel is open across the whole
/// spawn, so anything inheritable WOULD be inherited. NON-VACUITY comes
/// from a control fd in the same run: a plain `dup` of the write end, which
/// POSIX defines to clear `FD_CLOEXEC`, must be inherited, and must still be
/// able to suppress EOF — which is not just "the detector works" but the
/// actual harm, since that suppression is what parked the session-opening
/// thread for the stranger's entire lifetime.
#[test]
fn an_unrelated_exec_inherits_neither_end_of_the_exec_status_channel() {
    let (rd, wr, _carrier) = open_exec_status_channel().expect("the exec-status channel must open");
    // The control leak: `dup` never copies FD_CLOEXEC.
    // SAFETY: `wr` is live; `dup` returns a new fd onto the same channel.
    let leaked = unsafe { libc::dup(wr) };
    assert!(leaked >= 0, "dup(status_wr) failed");
    assert_eq!(
        fd_flags(leaked, "control dup of the status write end") & libc::FD_CLOEXEC,
        0,
        "PRECONDITION: the control fd must be inheritable, or it proves nothing"
    );

    // Report which of the three numbers actually arrived.
    let report = unrelated_exec_report("status", &[rd, wr, leaked], "");
    // Parse fd NUMBERS, not substrings: "got:3" is a substring of "got:33".
    let inherited: Vec<libc::c_int> = report
        .split_whitespace()
        .filter_map(|tok| tok.strip_prefix("got:"))
        .filter_map(|n| n.parse::<libc::c_int>().ok())
        .collect();

    // The harm, demonstrated with the control fd: hand it to a LIVE stranger,
    // drop every copy the "parent" holds, and EOF still does not arrive.
    let mut stranger = stranger_holding(leaked);
    // SAFETY: these are the parent's own copies; the stranger has its own.
    unsafe {
        libc::close(leaked);
        libc::close(wr);
    }
    let control_suppressed = eof_is_suppressed(rd, std::time::Duration::from_millis(300));
    let _ = stranger.kill();
    let _ = stranger.wait();
    // SAFETY: the read end is the last descriptor this test owns.
    unsafe { libc::close(rd) };

    // PRECONDITION: the helper really ran and really reported.
    assert!(
        report.contains("ran") && report.contains("end"),
        "the unrelated helper did not run to completion; report: {report:?}"
    );
    // NON-VACUITY: an inheritable write end IS inherited, and IS enough to
    // suppress the success verdict indefinitely.
    assert!(
        inherited.contains(&leaked),
        "control: an fd WITHOUT FD_CLOEXEC must be inherited across exec, \
         else this test cannot detect a leak at all; report: {report:?}"
    );
    assert!(
        control_suppressed,
        "control: a stranger holding an inherited WRITE end must suppress EOF \
         — that suppression is the hang, and without it the property below \
         would be asserting nothing"
    );
    // THE PROPERTY.
    assert!(
        !inherited.contains(&wr),
        "an unrelated exec'd process inherited the status WRITE end (fd {wr}) \
         — it can now suppress this spawn's EOF for its whole lifetime; \
         report: {report:?}"
    );
    assert!(
        !inherited.contains(&rd),
        "an unrelated exec'd process inherited the status READ end (fd {rd}); \
         report: {report:?}"
    );
}

/// THE HANG, bounded: with a stranger holding a duplicate of the write end,
/// the status wait must END — by the budget — instead of parking forever.
///
/// The leak is re-created ON PURPOSE (the fix makes it unreachable through
/// the real seam): a non-close-on-exec duplicate inherited by a real spawn
/// ([`stranger_holding`]).
///
/// NON-VACUITY is the whole design of this test:
///  * the stranger is asserted to be ALIVE at the end, so the wait ended
///    because of the bound and not because the holder happened to exit;
///  * `eof_is_suppressed` asserts the channel genuinely has no verdict — every
///    copy the "parent" owns is closed, so without the stranger EOF would be
///    immediate, which is exactly the state the ORIGINAL blocking read was
///    measured parking in for 5 s / 20 s / 65 s;
///  * a CONTROL run with no stranger must answer `Execed` promptly, proving
///    the harness can see a verdict when one exists.
///
/// A short budget is passed explicitly rather than using
/// `EXEC_STATUS_BUDGET`: what is under test is that the bound is what ends
/// the wait, not how long the shipping bound is (that is a separate
/// assertion below, and waiting 10 s here would buy nothing).
#[test]
fn a_stranger_holding_the_write_end_cannot_block_the_status_wait_forever() {
    const BUDGET: std::time::Duration = std::time::Duration::from_millis(400);

    // CONTROL first: same protocol, no stranger. A verdict must arrive. It
    // gets the hang detector, not the stranger arm's 400 ms: the EOF
    // arrives only once every copy of the write end is closed, and a fork
    // elsewhere in this binary (the spawn tests, the 300-fork loop) holds
    // one until it execs — longer than 400 ms on a loaded machine. The wait
    // returns the instant EOF lands, so a correct run still costs nothing.
    let (crd, cwr) = racy_status_pipe();
    // SAFETY: the only write end; closing it is the "child exec'd" signal.
    unsafe { libc::close(cwr) };
    let control = wait_for_exec_status(crd, HANG_DETECTOR);
    // SAFETY: the read end this test owns.
    unsafe { libc::close(crd) };

    // THE DEFECT: a stranger inherits a duplicate of the write end.
    let (rd, wr) = racy_status_pipe();
    // SAFETY: `wr` is live; `dup` copies it WITHOUT FD_CLOEXEC — exactly what
    // `pipe(2)`'s unflagged window handed to a concurrent `posix_spawn`.
    let leaked = unsafe { libc::dup(wr) };
    assert!(leaked >= 0, "dup(status_wr) failed");
    let mut stranger = stranger_holding(leaked);
    // Drop every copy the "parent" holds — including the one that stands in
    // for the child's, which a successful `execve` would have closed.
    // SAFETY: all three are this process's own copies.
    unsafe {
        libc::close(leaked);
        libc::close(wr);
    }

    let suppressed = eof_is_suppressed(rd, std::time::Duration::from_millis(250));
    let started = std::time::Instant::now();
    let status = wait_for_exec_status(rd, BUDGET);
    let waited = started.elapsed();
    let stranger_alive = stranger.try_wait().expect("try_wait").is_none();

    let _ = stranger.kill();
    let _ = stranger.wait();
    // SAFETY: the read end is the last descriptor this test owns.
    unsafe { libc::close(rd) };

    assert!(
        matches!(control, ExecStatus::Execed),
        "control: with no stranger, closing the last write end must answer \
         `Execed` — if it did not, the harness could not tell a suppressed \
         verdict from a missing one; got {control:?}"
    );
    assert!(
        suppressed,
        "PRECONDITION: with every parent-side copy closed, the channel must \
         still have NO verdict — that is the state the original unbounded \
         `read` parked in for the stranger's entire lifetime"
    );
    assert!(
        stranger_alive,
        "PRECONDITION: the stranger must STILL hold the write end when the \
         wait returns — otherwise the wait ended because the holder exited, \
         which is the old behaviour passing as the new one"
    );
    assert!(
        matches!(status, ExecStatus::NoVerdict),
        "a suppressed EOF must expire as `NoVerdict` (which the seam turns \
         into a fail-closed error), got {status:?}"
    );
    // Ended BY THE BUDGET: never before it, and not by anything later —
    // the unbounded read would park for the stranger's ten-minute life (and
    // `stranger_alive` would fail with it), a wait that ignored `budget` for
    // the shipping `EXEC_STATUS_BUDGET` would take 10 s. The upper bound is
    // that shipping budget, and it is exact against that defect:
    // `wait_for_exec_status` leaves only once `now >= deadline`, so a wait
    // that ignored `budget` always reads 10 s or more. The accepted loss:
    // an overshoot of the 400 ms budget by anything under ~9.6 s now
    // passes (the old `BUDGET * 8` caught one past 2.8 s) — the price of
    // not reading a descheduled test thread as a defect.
    assert!(
        waited >= BUDGET && waited < EXEC_STATUS_BUDGET,
        "the wait must be ended BY THE BUDGET: expected ~{BUDGET:?}, took \
         {waited:?}"
    );
}

/// The case a naive timeout would silently turn into a FALSE SUCCESS: a
/// pre-exec failure, reported while a stranger holds the write end.
///
/// This is the asymmetry the whole bounded design rests on. The failure byte
/// sits in the channel's BUFFER, so `read` returns it regardless of who else
/// holds a write end — only EOF depends on every write end being gone. If
/// that were not true, a bound would convert a sandbox-confinement failure
/// into the EOF success verdict and hand back an unconfined shell, which is
/// precisely the §5.6 hole `b'S'` exists to plug.
///
/// NON-VACUITY: once the byte is back, the stranger is asserted to be alive
/// AND still holding a write end — with the buffer drained and every copy of
/// ours closed, the channel must still have no EOF (`eof_is_suppressed`) —
/// so the byte really did cross a channel with a third holder; and the
/// verdict is required to arrive in a small fraction of the budget — a byte
/// that only appeared at expiry would mean the buffer claim is false and the
/// test had merely timed out into the right answer.
#[test]
fn a_pre_exec_failure_is_still_detected_while_a_stranger_holds_the_write_end() {
    // Five minutes: far past the bound below, a minute's hang detector
    // (AGENTS.md) that no descheduled test thread reaches, and below the
    // stranger's ten-minute life, so a byte only seen at expiry still comes
    // back while the stranger holds the write end, five times the bound on.
    const BUDGET: std::time::Duration = std::time::Duration::from_secs(300);

    let (rd, wr) = racy_status_pipe();
    // SAFETY: `wr` is live; `dup` copies it WITHOUT FD_CLOEXEC.
    let leaked = unsafe { libc::dup(wr) };
    assert!(leaked >= 0, "dup(status_wr) failed");
    let mut stranger = stranger_holding(leaked);
    // SAFETY: the parent's own copy of the leaked duplicate.
    unsafe { libc::close(leaked) };

    // The child's report: the sandbox-confinement failure byte, then its copy
    // of the write end goes away exactly as `_exit` would take it.
    let b: u8 = b'S';
    // SAFETY: `wr` is live; `write` reads exactly the one byte at `b`.
    let wrote = unsafe { libc::write(wr, ptr::from_ref(&b).cast::<libc::c_void>(), 1) };
    // SAFETY: the parent's own copy of the write end.
    unsafe { libc::close(wr) };

    let started = std::time::Instant::now();
    let status = wait_for_exec_status(rd, BUDGET);
    let waited = started.elapsed();
    // The byte is read and both of our copies are closed: EOF stays away
    // only while the stranger holds its copy of the write end.
    let held = eof_is_suppressed(rd, std::time::Duration::from_millis(250));
    let stranger_alive = stranger.try_wait().expect("try_wait").is_none();

    let _ = stranger.kill();
    let _ = stranger.wait();
    // SAFETY: the read end is the last descriptor this test owns.
    unsafe { libc::close(rd) };

    assert_eq!(wrote, 1, "the failure byte must have been written");
    assert!(
        stranger_alive,
        "PRECONDITION: the stranger must still be alive when the byte comes \
         back, or this proves nothing about a suppressed channel"
    );
    assert!(
        matches!(status, ExecStatus::FailedBeforeExec(b'S')),
        "a pre-exec failure must still be read as itself while a third party \
         holds a write end — a bound must never launder it into success; \
         got {status:?}"
    );
    assert!(
        held,
        "PRECONDITION: the stranger must still hold a write end once the byte \
         is back — EOF arrived, so the byte crossed a channel with no third \
         holder and this proves nothing about a suppressed one"
    );
    assert!(
        waited < HANG_DETECTOR,
        "the byte is in the channel BUFFER, so it must come back immediately \
         rather than at expiry; took {waited:?} of a {BUDGET:?} budget"
    );
}

/// The real seam, end to end: a spawn succeeds on the ATOMIC carrier. Success
/// itself proves the status arrived before the fail-closed expiry: a spawn
/// that waited out the budget reads `NoVerdict`, which the seam turns into a
/// `TimedOut` error.
///
/// The counter assertion is what makes this more than a smoke test: if
/// `open_exec_status_channel` had quietly fallen back to `pipe(2)` + `fcntl`
/// on this machine, every other assertion here would still pass while the
/// window sat wide open. The count is required not to move.
#[test]
fn a_real_spawn_settles_on_the_atomic_status_carrier() {
    use std::sync::atomic::Ordering;
    // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);

    let before = RACY_EXEC_STATUS_CARRIERS.load(Ordering::Relaxed);
    let spawned = spawn_shell_with_pid_cell_px(
        24,
        80,
        &spawn_cap,
        &sandbox_cap,
        &[],
        None, // shell_override
        None, // shell_args
        None,
        None,
        None,
        None,
        aterm_sandbox::Limits::inherit(),
        None, // cell_px
    )
    .expect("a normal shell must still spawn");
    let after = RACY_EXEC_STATUS_CARRIERS.load(Ordering::Relaxed);

    hangup(spawned.pid);
    // SAFETY: `master` is the fd this test was just handed.
    unsafe { libc::close(spawned.master) };
    reap(spawned.pid);

    assert_eq!(
        before, after,
        "the spawn fell back to the RACY `pipe(2)`+fcntl status carrier — the \
         close-on-exec window is open on this machine, and every other \
         assertion in this test would pass anyway"
    );
}

/// The fork race's stall detector: which side of the churn, if any, has
/// not moved for the limit while still at or under its floor.
///
/// Each side keeps its OWN clock. One clock restarted when EITHER counter
/// moved never fired while the other side kept counting, and the two sides
/// do not stall together: the path-naming threads do no filesystem I/O,
/// while the channel builders do `mkfifo` + `open` + `unlink` on a volume
/// that can stop answering. Builders stalled with the naming alive then left
/// the floor unmet and the fork loop running with no bound at all. Measured
/// with a stand-in (the two builder threads sleeping 50 ms and counting
/// nothing, the naming threads untouched): under the one clock the test was
/// still forking at 3m34s, and again when killed at 154 s; under these
/// per-side clocks it ended at 60 s on the PRECONDITION, naming "the channel
/// builders" (built 0, named 162840489, 48301 forks). A side past its floor
/// never stalls: the loop no longer needs it to move.
#[cfg(target_os = "macos")]
struct ChurnStall {
    floors: (u64, u64),
    limit: std::time::Duration,
    last: (u64, u64),
    moved_at: (std::time::Instant, std::time::Instant),
}

#[cfg(target_os = "macos")]
impl ChurnStall {
    fn new(floors: (u64, u64), limit: std::time::Duration, now: std::time::Instant) -> Self {
        Self {
            floors,
            limit,
            last: (0, 0),
            moved_at: (now, now),
        }
    }

    /// Record `(built, named)` at `now`; the side that has stalled, if any.
    fn observe(&mut self, seen: (u64, u64), now: std::time::Instant) -> Option<&'static str> {
        if seen.0 != self.last.0 {
            self.moved_at.0 = now;
        }
        if seen.1 != self.last.1 {
            self.moved_at.1 = now;
        }
        self.last = seen;
        let stuck = |count: u64, floor: u64, since: std::time::Instant| {
            count <= floor && now.saturating_duration_since(since) >= self.limit
        };
        match (
            stuck(seen.0, self.floors.0, self.moved_at.0),
            stuck(seen.1, self.floors.1, self.moved_at.1),
        ) {
            (false, false) => None,
            (true, false) => Some("the channel builders"),
            (false, true) => Some("the path naming"),
            (true, true) => Some("both sides"),
        }
    }
}

/// With `$TMPDIR` unset the status FIFO goes in `/tmp` — not where
/// `std::env::temp_dir` would put it, because std on Apple answers an unset
/// `$TMPDIR` with `confstr(_CS_DARWIN_USER_TEMP_DIR)`, the lookup
/// [`exec_status_fifo_dir`] exists to avoid (measured: it was the shipped
/// session's first libnotify call). A set one is used as given, ending in
/// one `/`.
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

/// Building the status channel must not KILL a concurrent fork's child.
///
/// This is a scar, not a hypothetical. The first version of this carrier
/// located its FIFO with `confstr(_CS_DARWIN_USER_TEMP_DIR)` — the
/// conservative choice, since it asks the OS instead of trusting `$TMPDIR`.
/// That call reaches libsystem_notify, whose shared region sits behind an
/// `os_alloc_once` gate, and `fork(2)` runs `notify_fork_child` in the child
/// before it returns. A fork that lands while another thread is inside that
/// gate leaves the child holding a once-gate owned by a thread that does not
/// exist, and libplatform kills it:
///
/// ```text
///   BUG IN CLIENT OF LIBPLATFORM: os_once_t is corrupt
///   libsystem_c.dylib: crashed on child side of fork pre-exec
///   _os_once_gate_corruption_abort <- _os_alloc_once <- notify_fork_child
///     <- libSystem_atfork_child <- fork
/// ```
///
/// SIGKILL, raised INSIDE `fork()` before one instruction of the child branch
/// runs — so no amount of care in that branch can prevent it. MEASURED on
/// otherwise-unmodified production code, with a test doing nothing but
/// `confstr` on six threads: a concurrent `forkpty` child died in 16 of 25
/// runs. In aterm that is a user's shell SIGKILLed at random because another
/// thread asked where the temp directory was.
///
/// So the invariant under test is deliberately broader than "don't call
/// `confstr`": building the exec-status channel must not make a CONCURRENT
/// FORK'S CHILD DIE, whatever it is built from. Anything added to the carrier
/// later — a different directory lookup, a randomness source, a logging call —
/// is covered by construction.
///
/// NON-VACUITY, and its LIMIT, stated honestly because this one matters: the
/// channel builder and the fork loop are both asserted to have actually run,
/// and the builder is asserted to have produced the ATOMIC carrier, so the
/// fifo path (the one with a directory lookup in it) is the one being
/// exercised rather than the pipe fallback. Checked against the real defect
/// by putting the `confstr` back: with libnotify's gate cold it FAILS — in
/// 21 runs of 150 (raising the fork count to 1500 and 4000 did not improve
/// it: the limit is how briefly the once-gate is held, not how many forks are thrown at it).
///
/// That gate is no longer cold here, which changed what this test can see.
/// It DID cry wolf: a sibling test making the process's first libnotify
/// call (aterm-spec's `getpwuid_r`, in every run) armed the same gate while
/// this loop forked, and killed one of its children in 3 of 160 loaded full
/// runs. This test binary now passes the gate at image load
/// (`aterm_uds::pass_libnotify_gate_at_load!`, in `lib.rs`), which ended
/// that — and with it this test's view of the `confstr` defect: put back,
/// it failed 0 of 150 runs. That hazard is closed where it bites instead:
/// every fork that strips its child passes the gate first
/// (`aterm_uds::spawnfd::InheritStrip::prepare`, held by aterm-uds'
/// `preparing_a_fork_passes_the_libnotify_gate`), and this carrier's own
/// lookup cannot reach `confstr` ([`fifo_dir_in`], held by
/// `an_unset_tmpdir_puts_the_status_fifo_in_tmp`).
///
/// So: this is a SMOKE ALARM for the rest — a carrier that arms some OTHER
/// once-gate a fork child handler takes (libxpc's and
/// libsystem_coreservices' have that shape). It cannot cry wolf about those,
/// and a single green run is not evidence of fork-safety.
///
/// COUNT-BOUNDED, NOT TIME-BOUNDED. The fork loop runs until the race has
/// reached its density floor — at least `MIN_FORKS` forks, and more than
/// `BUILT_FLOOR` whole channels and `NAMED_FLOOR` path resolutions, all
/// counted after a start barrier and read INSIDE the loop, so every one of
/// them overlapped forking — and only the CHURN STALLING ends it otherwise
/// (either side, channels built or paths named, not moving for
/// `CHURN_STALL` while still under its floor — [`ChurnStall`]), failing
/// loudly with the counts. Not a deadline or a fixed fork count: under load
/// a fork + reap takes ~2 s, and a channel (`mkfifo` + `unlink`) costs
/// 15-24 ms on a busy volume, so either bound fails a race that is dense and
/// still running. A slow build just means more forks land inside each
/// channel, which is denser, not sparser.
///
/// DARWIN ONLY, and for a structural reason rather than convenience: the
/// hazard belongs to the PATHNAME-based carrier, and the naming path this
/// churns ([`exec_status_fifo_path`]) exists only there. Linux's
/// `pipe2(O_CLOEXEC)` is one syscall with no directory lookup, no lazily
/// initialised subsystem behind it, and therefore nothing of this shape to
/// get wrong.
#[cfg(target_os = "macos")]
#[test]
fn building_the_status_channel_never_kills_a_concurrent_forks_child() {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};

    // THE DENSITY FLOOR. The fork loop does not stop until all three are
    // met, so a slow volume costs time, never coverage.
    const MIN_FORKS: u64 = 300;
    const BUILT_FLOOR: u64 = 50; // whole channels: strictly more than this
    const NAMED_FLOOR: u64 = 10_000; // path resolutions: strictly more than this
    // What ends the loop if the floor is never met: the churn making NO
    // progress for this long — not the clock. On a quiet volume the floor
    // is met in well under a second, and under a build storm on the same
    // volume (channel p90 ~95 ms) 160 passing runs took p50
    // ~1.3 s and at most 5.5 s, process start included. A slow machine only
    // makes the forks slower; each fork the loop makes is its own progress,
    // so the one thing that can keep the floor unmet forever is a carrier
    // that has stalled, which the PRECONDITION below reports with every
    // count. A minute in which either side, still under its floor, did not
    // move once is that — a hang detector, not a timing claim. Per side,
    // because the sides stall apart: the builders touch the volume, the
    // naming threads do not.
    const CHURN_STALL: std::time::Duration = std::time::Duration::from_secs(60);
    const CHURN_THREADS: usize = 6;

    let stop = Arc::new(AtomicBool::new(false));
    let built = Arc::new(AtomicU64::new(0));
    let named = Arc::new(AtomicU64::new(0));
    let racy = Arc::new(AtomicU64::new(0));
    // Every churn thread is running before the first fork, so the counts
    // below are all taken while forking was under way — none of them is a
    // head start the forks never saw.
    let start = Arc::new(Barrier::new(CHURN_THREADS + 1));

    // TWO kinds of churn, and the split is what makes the test sensitive.
    //
    // Building a whole channel costs ~270 us on a quiet volume (mkfifo + two
    // opens + unlink) and tens of ms on a busy one, so threads doing only
    // that touch the NAMING path a few thousand times a second at best — far
    // too sparse to reliably catch a once-gate that is only held for the
    // duration of one lookup. VERIFIED: with channel-building churn alone,
    // restoring the `confstr` did NOT fail this test in 10 runs. So most
    // threads hammer the naming path DIRECTLY, in a tight loop, which is
    // what raises the probability of a fork landing inside the gate to
    // something a test can rely on.
    let churn: Vec<_> = (0..CHURN_THREADS)
        .map(|i| {
            let (stop, built, named, racy, start) = (
                stop.clone(),
                built.clone(),
                named.clone(),
                racy.clone(),
                start.clone(),
            );
            std::thread::spawn(move || -> Result<(), String> {
                // Two threads exercise the FULL carrier (so mkfifo/open/unlink
                // are in the race too); the rest hammer path resolution.
                let full = i < 2;
                start.wait();
                while !stop.load(Ordering::Relaxed) {
                    if full {
                        let (rd, wr, carrier) = open_exec_status_channel()
                            .map_err(|e| format!("thread {i}: building the channel: {e}"))?;
                        if carrier == ExecStatusCarrier::RacyPipe {
                            racy.fetch_add(1, Ordering::Relaxed);
                        }
                        built.fetch_add(1, Ordering::Relaxed);
                        // SAFETY: the two ends this thread just opened.
                        unsafe {
                            libc::close(rd);
                            libc::close(wr);
                        }
                    } else {
                        exec_status_fifo_path()
                            .map_err(|e| format!("thread {i}: naming the fifo: {e}"))?;
                        named.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Ok(())
            })
        })
        .collect();

    // Meanwhile, fork — until the race has been dense enough, not for a fixed
    // count. The child does nothing but `_exit(0)` — so ANY death by signal is
    // the fork machinery itself dying, never our own code.
    start.wait();
    let began = std::time::Instant::now();
    let mut forks = 0u64;
    let mut signalled: Vec<(i32, i32)> = Vec::new();
    // Either side stalling under its floor ends the loop, however busy the
    // other side is.
    let mut churn_stall = ChurnStall::new((BUILT_FLOOR, NAMED_FLOOR), CHURN_STALL, began);
    // Read INSIDE the loop, at the fork that met the floor: what the churn
    // threads do after `stop` overlapped no fork and is not counted.
    let (built_while_forking, named_while_forking, stalled_sides) = loop {
        // SAFETY: `fork` takes no arguments; the child below reaches only
        // `_exit`, which is async-signal-safe.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork: {}", io::Error::last_os_error());
        if pid == 0 {
            // SAFETY: async-signal-safe, and the only call the child makes.
            unsafe { libc::_exit(0) };
        }
        forks += 1;
        let mut wstatus: libc::c_int = 0;
        // SAFETY: reaping the child just forked; `wstatus` is a valid out-param.
        unsafe { libc::waitpid(pid, &mut wstatus, 0) };
        if !libc::WIFEXITED(wstatus) {
            signalled.push((pid, libc::WTERMSIG(wstatus)));
        }
        let seen = (built.load(Ordering::Relaxed), named.load(Ordering::Relaxed));
        let dense = forks >= MIN_FORKS && seen.0 > BUILT_FLOOR && seen.1 > NAMED_FLOOR;
        // A churn thread only finishes before `stop` by failing, and then it
        // is no longer racing: stop now and let the join below say why.
        let churn_quit = churn.iter().any(std::thread::JoinHandle::is_finished);
        let stalled = churn_stall.observe(seen, std::time::Instant::now());
        if dense || churn_quit || stalled.is_some() {
            break (seen.0, seen.1, stalled.unwrap_or("none"));
        }
    };
    let raced_for = began.elapsed();
    stop.store(true, Ordering::Relaxed);
    let quit: Vec<String> = churn
        .into_iter()
        .filter_map(|t| match t.join() {
            Ok(Ok(())) => None,
            Ok(Err(why)) => Some(why),
            Err(_) => Some("a churn thread panicked".to_owned()),
        })
        .collect();
    let racy = racy.load(Ordering::Relaxed);

    // THE PROPERTY, first: a killed child is a finding however sparse the
    // race around it was, so no precondition may mask it. The preconditions
    // below exist only to keep a PASS from being vacuous.
    assert!(
        signalled.is_empty(),
        "a child that does nothing but _exit(0) was killed by a signal while \
         the exec-status channel was being built on other threads — the \
         carrier is doing something that is not fork-safe (SIGKILL here means \
         `fork` itself aborted in libSystem's atfork child handler, e.g. the \
         `confstr`/libsystem_notify `os_once` corruption this test records). \
         (pid, signal) pairs: {signalled:?}"
    );
    // PRECONDITIONS: both sides of the race really ran, together, densely.
    assert!(
        quit.is_empty(),
        "PRECONDITION: every churn thread must race until told to stop; these \
         quit early, so the race stopped being exercised: {quit:?}"
    );
    assert!(
        forks >= MIN_FORKS
            && built_while_forking > BUILT_FLOOR
            && named_while_forking > NAMED_FLOOR,
        "PRECONDITION: the carrier must have been exercised HARD alongside \
         the forks, else nothing was raced (and a sparse race is exactly what \
         was measured to miss this defect); in {raced_for:?} of forking \
         (stalled under its floor for {CHURN_STALL:?}: {stalled_sides}) it \
         was built {built_while_forking} times (need > {BUILT_FLOOR}) and \
         named {named_while_forking} (need > {NAMED_FLOOR}) across {forks} \
         forks (need >= {MIN_FORKS})"
    );
    assert_eq!(
        racy, 0,
        "PRECONDITION: the builder must have produced the ATOMIC carrier — \
         the pipe fallback has no directory lookup, so racing it would not \
         exercise the hazard this test exists for"
    );
}

/// The platform trap that dictates `select(2)` in `wait_readable_briefly`.
///
/// On macOS a FIFO read end whose LAST WRITER HAS CLOSED is at EOF —
/// `read(2)` returns 0 — but `poll(2)` reports nothing at all and
/// `kqueue`/`EVFILT_READ` reports nothing at all, indefinitely. Only
/// `select(2)` sees it. All three are correct on a pipe, which is what makes
/// this a trap rather than a curiosity: a maintainer who "modernises" the
/// `select` into a `poll` would watch every pipe-based test stay green and
/// silently add the full budget to every successful spawn on macOS.
///
/// NON-VACUITY: the same `poll` and `kqueue` calls are run against a PIPE at
/// EOF in the same test and must both report it — so a failure here means
/// "fifos are special", not "the probe is broken". Pinned as an executable
/// fact so that if a future macOS fixes it, this test says so out loud
/// instead of leaving the comment to rot.
#[cfg(target_os = "macos")]
#[test]
fn darwin_fifo_eof_is_invisible_to_poll_and_kqueue_but_not_select() {
    /// Does `poll` report readability on `fd` within `ms`?
    fn polls_ready(fd: libc::c_int, ms: libc::c_int) -> bool {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pfd` is a live, fully-initialized one-element array.
        unsafe { libc::poll(&mut pfd, 1, ms) > 0 }
    }
    /// Does `kqueue`/`EVFILT_READ` report readability on `fd` within `ms`?
    fn kqueue_ready(fd: libc::c_int, ms: i64) -> bool {
        // SAFETY: `ch`/`ev` are zeroed, fully-initialized kevents; `kevent`
        // reads one change and writes at most one event through them.
        unsafe {
            let kq = libc::kqueue();
            let mut ch: libc::kevent = std::mem::zeroed();
            ch.ident = fd as usize;
            ch.filter = libc::EVFILT_READ;
            ch.flags = libc::EV_ADD | libc::EV_ENABLE;
            let mut ev: libc::kevent = std::mem::zeroed();
            // Split: a whole-second timeout in `tv_nsec` alone is out of
            // range, and the call answers EINVAL — read as "not ready".
            let ts = libc::timespec {
                tv_sec: ms / 1000,
                tv_nsec: (ms % 1000) * 1_000_000,
            };
            let n = libc::kevent(kq, &ch, 1, &mut ev, 1, &ts);
            libc::close(kq);
            n > 0
        }
    }
    /// Does `select` report readability on `fd` within `ms`?
    fn selects_ready(fd: libc::c_int, ms: i32) -> bool {
        // SAFETY: `set` is a zeroed fd_set and test fds are far below
        // FD_SETSIZE; `select` touches only `set` and `tv`.
        unsafe {
            let mut set: libc::fd_set = std::mem::zeroed();
            libc::FD_SET(fd, &mut set);
            // Split, as in `kqueue_ready`: `tv_usec` holds under a second.
            let mut tv = libc::timeval {
                tv_sec: libc::time_t::from(ms / 1000),
                tv_usec: (ms % 1000) * 1000,
            };
            libc::select(fd + 1, &mut set, ptr::null_mut(), ptr::null_mut(), &mut tv) > 0
        }
    }

    // A fifo at EOF: the real carrier, with its write end closed.
    //
    // Every probe EXPECTED to report ready waits the minute hang detector,
    // not 200 ms: EOF arrives only once every copy of the write end is
    // closed, and a fork elsewhere in this binary holds one until it execs —
    // past 10 s on a heavily loaded machine. A ready descriptor answers at once,
    // so a correct run costs nothing; the probes expected NOT to report keep
    // 200 ms, since a late EOF only keeps them false.
    let (frd, fwr) = open_exec_status_fifo().expect("the exec-status fifo must open");
    // SAFETY: the only write end; closing it puts the read end at EOF.
    unsafe { libc::close(fwr) };
    let fifo = (
        // `select` first, and it establishes the EOF: once an EOF has been
        // READ on a Darwin fifo, select stops reporting it (below).
        selects_ready(frd, 60_000),
        polls_ready(frd, 200),
        kqueue_ready(frd, 200),
    );
    // And what `read(2)` says about the very same descriptor.
    let fifo_verdict = read_exec_status_now(frd);
    // SAFETY: the read end this test owns.
    unsafe { libc::close(frd) };

    // THE PRODUCTION FUNCTION, bound to the fact above — on a FRESH carrier,
    // and in the state `wait_for_exec_status` actually waits in.
    //
    // That distinction is load-bearing and was got wrong once: the loop reads
    // FIRST and returns on EOF, so it never waits on a descriptor already at
    // EOF. It waits with a writer still open and nothing sent — i.e. the
    // child has not exec'd yet — and needs to WAKE when that last writer goes
    // away. (MEASURED aside, which is why this must use a fresh fifo: once an
    // EOF has been READ on a Darwin fifo, `select` stops reporting the
    // descriptor ready, so timing a wait after a read measures a state
    // production never sees and reports a 2 s "hang" that is not one.)
    //
    // So: hold the write end, close it 100 ms later, and time the wait.
    // `select` wakes on that transition; `poll` and `kqueue` never do.
    //
    // The slice is the minute hang detector, and the verdict is the slice
    // itself: a readiness primitive blind to fifo EOF returns only when its
    // timeout expires, never before, so "woke inside the slice" is exact
    // against it at any load. The EOF's own arrival is not the test's to
    // time: the closer thread can be starved, and a fork elsewhere in this
    // binary holds a copy of the write end until it execs (beside 72
    // spinners, with the test clamped to background QoS, the wake read
    // 14 s: red against the old 10 s bound, though its 20 s slice had not
    // expired, so it was a real wake).
    const WAKE_SLICE: std::time::Duration = std::time::Duration::from_secs(60);
    // Without this binding, swapping `select` for `poll` in
    // `wait_readable_briefly` leaves every other test green — the slice cap
    // keeps it CORRECT, just slow — and the latency regression ships.
    let (drd, dwr) = open_exec_status_fifo().expect("the exec-status fifo must open");
    let closer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        // SAFETY: the only write end; this thread owns it now.
        unsafe { libc::close(dwr) };
    });
    let waited = std::time::Instant::now();
    wait_readable_briefly(drd, WAKE_SLICE);
    let wait_readable_took = waited.elapsed();
    let after_wake = read_exec_status_now(drd);
    closer.join().expect("closer thread");
    // SAFETY: the read end this test owns.
    unsafe { libc::close(drd) };

    // The CONTROL: a pipe at EOF, probed identically.
    let (prd, pwr) = racy_status_pipe();
    // SAFETY: the only write end; closing it puts the read end at EOF.
    unsafe { libc::close(pwr) };
    // All three are expected ready: `select` waits up to the minute for the
    // EOF to land, and a pipe at EOF is level-triggered, so the other two answer at
    // once after it.
    let pipe = (
        selects_ready(prd, 60_000),
        polls_ready(prd, 200),
        kqueue_ready(prd, 200),
    );
    // SAFETY: the read end this test owns.
    unsafe { libc::close(prd) };

    // CONTROL: on a pipe all three mechanisms see EOF. If this ever fails,
    // the probes above are broken and the fifo result below means nothing.
    assert_eq!(
        pipe,
        (true, true, true),
        "control: select/poll/kqueue must ALL report EOF on a PIPE \
         (select, poll, kqueue) = {pipe:?}"
    );
    // The ground truth `wait_for_exec_status` actually relies on.
    assert!(
        matches!(fifo_verdict, Some(ExecStatus::Execed)),
        "a fifo whose last writer closed must read as EOF; got {fifo_verdict:?}"
    );
    assert!(
        fifo.0,
        "select(2) must report EOF on a fifo — it is the ONLY readiness \
         primitive `wait_readable_briefly` can use on this platform"
    );
    // The binding that makes the fact above load-bearing on real code.
    assert!(
        matches!(after_wake, Some(ExecStatus::Execed)),
        "PRECONDITION: the last writer really did close, so the wait had a \
         real EOF to wake on; got {after_wake:?}"
    );
    // The line that separates the two behaviours is the SLICE, not an idle
    // machine's latency: a primitive blind to fifo EOF sleeps out the whole
    // slice, a correct one wakes when the last writer closes, however late
    // load makes that. Every stopwatch bound under the slice measured the
    // machine instead (10 s against a 20 s slice read 14 s under load).
    assert!(
        wait_readable_took < WAKE_SLICE,
        "`wait_readable_briefly` must WAKE when a fifo's last writer closes \
         rather than sleeping out its {WAKE_SLICE:?} slice — it took \
         {wait_readable_took:?}, which is what a readiness primitive blind to \
         fifo EOF (poll, kqueue) does. Correctness is unaffected (the caller \
         re-reads), but every successful spawn on macOS pays this in latency"
    );
    assert_eq!(
        (fifo.1, fifo.2),
        (false, false),
        "poll/kqueue are expected to MISS fifo EOF on macOS; if this now \
         fails, the platform was fixed — update `EXEC_STATUS_SLICE`'s and \
         `wait_readable_briefly`'s comments rather than deleting this test \
         (poll, kqueue) = {:?}",
        (fifo.1, fifo.2)
    );
}

/// A spawned child inherits its stdio and NOTHING else (step 0c). A pipe end
/// is parked on a number from 100 up WITHOUT close-on-exec — the state the GUI's Metal
/// driver leaves its shader-cache files in, measured leaking into a login
/// shell — and the child reports whether that number is open.
/// POSITIVE CONTROLS: the same probe sees its own stdout (fd 1), so "closed"
/// is a reading, not a broken probe; and the parked number is asserted inheritable in
/// the parent before the spawn, so the test is about the child's strip and
/// not about a descriptor that was never going to cross the `execve`.
#[test]
fn a_spawned_child_inherits_its_stdio_and_no_other_descriptor() {
    assert_the_child_never_holds_a_descriptor_parked_at(100);
}

/// The same above the strip's OLD ceiling: the soft RLIMIT_NOFILE capped at
/// 65 536, while Darwin hands out numbers up to `kern.maxfilesperproc`
/// (92 160 on one Mac, 10 240 on another) whenever the soft limit is higher
/// — a shell's is 1 048 576, and aterm-gui raises its own. A descriptor
/// parked at 70 000 would ride every shell aterm spawned. The stand-in sits at 70 000, or just
/// under the host's per-process maximum (`spawnfd::descriptor_bound`) when
/// that is lower. A host whose maximum leaves no number above 65 536 cannot
/// hold such a descriptor, so nothing can leak: the test says so on stderr
/// and returns. The soft limit is raised here first; a hard limit too low
/// to stage it is said, not passed.
#[test]
fn a_spawned_child_inherits_no_descriptor_above_the_old_65536_ceiling() {
    /// Free numbers kept between the stand-in and the host's maximum.
    const HEADROOM: libc::c_int = 8;
    let bound = aterm_uds::spawnfd::descriptor_bound();
    let floor = 70_000.min(bound.saturating_sub(HEADROOM));
    if floor <= 65_536 {
        let name = if cfg!(target_vendor = "apple") {
            "kern.maxfilesperproc"
        } else {
            "a hard RLIMIT_NOFILE"
        };
        eprintln!(
            "skipped here: needs {name} above {}, this host allows {bound}",
            65_536 + HEADROOM
        );
        return;
    }
    let floor_rlim = libc::rlim_t::try_from(floor).expect("a positive floor");
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: a live out-parameter `getrlimit` fills.
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
        0
    );
    if limit.rlim_cur <= floor_rlim + 64 {
        limit.rlim_cur = limit.rlim_max.min(floor_rlim + 64);
        // SAFETY: a live, initialised rlimit that keeps the hard limit.
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    }
    if limit.rlim_cur <= floor_rlim {
        println!(
            "NOT RUN: the hard descriptor limit {} is below {floor}",
            limit.rlim_max
        );
        return;
    }
    assert_the_child_never_holds_a_descriptor_parked_at(floor);
}

/// Park an unflagged pipe end at `floor` or above, spawn a probe through the
/// real seam, and assert it sees its stdout and not the parked number.
fn assert_the_child_never_holds_a_descriptor_parked_at(floor: libc::c_int) {
    use std::time::Duration;
    // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let (rd, wr) = racy_status_pipe();
    // F_DUPFD takes the lowest FREE number from `floor` up, so no sibling
    // test's descriptor is disturbed, and its copy carries NO close-on-exec
    // flag — which is the point.
    // SAFETY: `fcntl(F_DUPFD)` on a descriptor this test owns.
    let parked = unsafe { libc::fcntl(rd, libc::F_DUPFD, floor) };
    assert!(parked >= floor, "park a copy of the pipe: {parked}");
    // SAFETY: F_GETFD reads the flag word of a descriptor this test owns.
    let flags = unsafe { libc::fcntl(parked, libc::F_GETFD) };
    assert_eq!(
        flags & libc::FD_CLOEXEC,
        0,
        "PRECONDITION: fd {parked} is inheritable in the parent"
    );
    let script = format!(
        "o=closed; {{ : >&{parked}; }} 2>/dev/null && o=open; \
         s=closed; {{ : >&1; }} 2>/dev/null && s=open; \
         printf 'PROBE parked=%s stdout=%s\\n' \"$o\" \"$s\"; exec sleep 30"
    );
    let exec: Vec<std::ffi::OsString> = vec!["/bin/sh".into(), "-c".into(), script.into()];
    let sh = spawn_shell_with_pid_cell_px(
        24,
        80,
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
        None, // cell_px
    )
    .expect("the probe command must spawn");
    // SAFETY: closing this test's own descriptors; the child has its copies
    // (or, as asserted below, does not).
    unsafe {
        libc::close(parked);
        libc::close(rd);
        libc::close(wr);
    }

    set_nonblocking(sh.master, true).expect("nonblocking master");
    let mut seen = Vec::new();
    // A hang detector for the spawned probe's one line (a fresh shell exec
    // on a loaded machine); the loop leaves the moment the line is in.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < deadline && !seen.ends_with(b"\n") {
        let mut buf = [0u8; 256];
        let n = read(sh.master, &mut buf);
        if n > 0 {
            seen.extend_from_slice(&buf[..n as usize]);
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    hangup(sh.pid);
    // SAFETY: closing the master this test owns.
    unsafe { libc::close(sh.master) };
    reap(sh.pid);
    let seen = String::from_utf8_lossy(&seen).into_owned();
    assert!(
        seen.contains("stdout=open"),
        "POSITIVE CONTROL: the probe must see its own stdout: {seen:?}"
    );
    assert!(
        seen.contains("parked=closed"),
        "the child inherited fd {parked}, a descriptor it was never given: {seen:?}"
    );
}

/// The property that would SILENTLY break the shell if the close-on-exec
/// design were wrong: the child's stdio must NOT be close-on-exec.
///
/// `login_tty` `dup2`s the (close-on-exec) slave onto 0/1/2, and `dup2` never
/// carries `FD_CLOEXEC` to the new descriptor — so the copies survive
/// `execve`. If that were wrong the exec'd program would start with fds
/// 0/1/2 CLOSED and this test could not receive a single byte: the `printf`
/// below is written to fd 1, through the real spawn seam, and read off the
/// master. Same run also proves the child got a CONTROLLING TERMINAL
/// (`/dev/tty` opens) and the parent-applied winsize (`stty size`).
#[test]
fn spawned_child_stdio_is_an_inherited_tty_with_a_ctty_and_our_winsize() {
    use std::time::Duration;
    // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    // Deliberately NOT 24x80: a default-shaped answer would not distinguish
    // "our winsize arrived" from "the kernel's default happens to match".
    // The child reports, then PARKS, so the parent can interrogate the
    // process identities below while the session is still live.
    let script = "s=n; [ -t 0 ] && [ -t 1 ] && [ -t 2 ] && s=y; \
                  c=n; { : < /dev/tty; } 2>/dev/null && c=y; \
                  printf 'PROBE stdio=%s ctty=%s size=[%s] tty=[%s]\\n' \
                    \"$s\" \"$c\" \"$(stty size)\" \"$(tty)\"; \
                  exec sleep 30";
    let exec: Vec<std::ffi::OsString> = vec!["/bin/sh".into(), "-c".into(), script.into()];
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
        None, // cell_px
    )
    .expect("the probe command must spawn");

    set_nonblocking(sh.master, true).expect("nonblocking master");
    let mut seen = Vec::new();
    // A hang detector for the probe's line; it breaks as soon as the line
    // is complete, and the child parks in `sleep` only AFTER printing it.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        let mut buf = [0u8; 512];
        let n = read(sh.master, &mut buf);
        if n > 0 {
            seen.extend_from_slice(&buf[..n as usize]);
            if seen.windows(1).len() > 0 && seen.ends_with(b"\n") && seen.starts_with(b"PROBE") {
                break;
            }
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let seen = String::from_utf8_lossy(&seen).into_owned();

    // The child's controlling terminal must be THIS session's slave, named
    // exactly. `ctty=y` alone would be satisfied by an inherited terminal
    // (the harness has one); matching the device proves setsid + TIOCSCTTY
    // ran against OUR pts.
    let want_tty = pts_name(sh.master)
        .expect("the master must still resolve its pts name")
        .to_string_lossy()
        .into_owned();

    // Process identities, read from the PARENT while the session is parked:
    // pid == pgid == sid, and the pty's foreground process group is the
    // child. These are the exact probes `hangup` (killpg),
    // `record_adopted_shell` (the session leader of the PTY), and the GUI's
    // quit_safety/control_input (tcgetpgrp) depend on.
    // SAFETY: read-only identity probes on a live child pid / our master fd.
    let (pgid, sid, fg) = unsafe {
        (
            libc::getpgid(sh.pid),
            libc::getsid(sh.pid),
            libc::tcgetpgrp(sh.master),
        )
    };

    hangup(sh.pid);
    // SAFETY: closing the master this test owns.
    unsafe { libc::close(sh.master) };
    reap(sh.pid);

    // PRECONDITION: bytes arrived at all. This IS the stdio-survives-exec
    // proof — an exec'd child with close-on-exec 0/1/2 has nothing to write
    // to, so an empty read here is exactly the failure being guarded.
    assert!(
        seen.contains("PROBE"),
        "no output reached the master: the exec'd child had no usable stdio \
         (this is what a close-on-exec 0/1/2 would look like). Saw: {seen:?}"
    );
    assert!(
        seen.contains("stdio=y"),
        "the exec'd child's fds 0/1/2 must all be ttys — dup2 must have \
         cleared FD_CLOEXEC on the slave copies. Saw: {seen:?}"
    );
    assert!(
        seen.contains("ctty=y"),
        "the child must have a CONTROLLING TERMINAL (/dev/tty openable) — \
         login_tty's setsid + TIOCSCTTY. Saw: {seen:?}"
    );
    assert!(
        seen.contains("size=[41 137]"),
        "the parent-applied winsize must be live before the child's first \
         read of it. Saw: {seen:?}"
    );
    assert!(
        seen.contains(&format!("tty=[{want_tty}]")),
        "the child's controlling terminal must be THIS session's slave \
         ({want_tty}), not an inherited one. Saw: {seen:?}"
    );
    assert_eq!(
        (pgid, sid, fg),
        (sh.pid, sh.pid, sh.pid),
        "the child must be a SESSION LEADER with pid == pgid == sid and be \
         the pty's foreground process group — the identity hangup (killpg), \
         record_adopted_shell (tcgetsid) and the GUI's tcgetpgrp all depend on"
    );
}

/// The parent must not keep its copy of the slave. A retained slave holds the
/// pty open for this process's whole life, so the master could never report
/// hangup after the shell died — and `handoff_masters_closed` is fail-closed
/// on exactly that condition, so the seamless update would wait forever on a
/// session that is already gone.
///
/// Non-vacuous by asserting the PRECONDITION in the same run: while the child
/// is demonstrably alive and attached, the same poll must NOT report hangup.
/// Without that half, a master that reported `POLLHUP` unconditionally (or a
/// bad fd reporting `POLLNVAL`) would pass.
#[test]
fn master_reports_hangup_once_the_session_dies() {
    use std::time::Duration;
    // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    // Announce liveness, then park: the session stays up until we hang it up.
    let exec: Vec<std::ffi::OsString> = vec![
        "/bin/sh".into(),
        "-c".into(),
        "printf 'UP\\n'; exec sleep 30".into(),
    ];
    let sh = spawn_shell_with_pid_cell_px(
        24,
        80,
        &spawn_cap,
        &sandbox_cap,
        &[],
        None,
        None,
        None,
        Some(&exec),
        None,
        None,
        aterm_sandbox::Limits::inherit(),
        None, // cell_px
    )
    .expect("the parked session must spawn");

    set_nonblocking(sh.master, true).expect("nonblocking master");
    let mut alive = false;
    // A hang detector for `UP` (the child parks only after printing it).
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        let mut buf = [0u8; 64];
        let n = read(sh.master, &mut buf);
        if n > 0 && String::from_utf8_lossy(&buf[..n as usize]).contains("UP") {
            alive = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // PRECONDITION: a live, attached session does NOT report hangup.
    let while_alive = poll_revents(sh.master, Duration::from_millis(150));

    // Now end it, exactly as teardown does.
    hangup(sh.pid);
    reap(sh.pid);
    // A hang detector: the hangup must be reported, and the poll returns the
    // moment it is. (`while_alive` above is the negative window; it stays.)
    let after_death = poll_revents(sh.master, Duration::from_secs(60));

    // SAFETY: closing the master this test owns.
    unsafe { libc::close(sh.master) };

    assert!(
        alive,
        "the child never announced itself; the test proved nothing"
    );
    assert_eq!(
        while_alive & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL),
        0,
        "PRECONDITION: a LIVE session must not look dead to \
         handoff_masters_closed; revents were 0x{while_alive:04x}"
    );
    assert_ne!(
        after_death & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL),
        0,
        "the master never reported hangup after the session died — the \
         parent is still holding a copy of the pty SLAVE, and \
         handoff_masters_closed would block the seamless update forever"
    );
}

// ---- spawn termios (kernel defaults + IUTF8/B230400 deltas) ----

#[test]
fn kernel_default_termios_probe_yields_cooked_defaults() {
    let d = kernel_default_termios().expect("throwaway-pty termios probe must succeed");
    // Sanity that we captured the kernel's COOKED defaults, not garbage: a
    // fresh NULL-termios pty is canonical + echoing + output-processing.
    assert_ne!(d.c_lflag & libc::ICANON, 0, "default pty must be canonical");
    assert_ne!(d.c_lflag & libc::ECHO, 0, "default pty must echo");
    assert_ne!(
        d.c_oflag & libc::OPOST,
        0,
        "default pty must post-process output"
    );
}

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
    // ...and NOTHING else: flag-for-flag identical to the kernel defaults.
    assert_eq!(t.c_iflag & !libc::IUTF8, d.c_iflag & !libc::IUTF8);
    assert_eq!(t.c_oflag, d.c_oflag);
    assert_eq!(t.c_lflag, d.c_lflag);
    assert_eq!(t.c_cc, d.c_cc);
    // On macOS speed lives in dedicated c_ispeed/c_ospeed fields, so c_cflag
    // must be untouched too (Linux stores CBAUD bits inside c_cflag — skip).
    #[cfg(target_os = "macos")]
    assert_eq!(t.c_cflag, d.c_cflag);
}

#[test]
fn spawned_pty_carries_iutf8_and_b230400() {
    // END-TO-END: the explicit termios must actually reach the spawned PTY.
    // Run `sleep` (touches no termios, unlike a shell) and tcgetattr the
    // master — on both BSD and Linux ptys the master reflects the slave's
    // termios, so the two deltas must be visible.
    let master = spawn_on_pty(&["/bin/sleep", "5"]);
    // SAFETY: tcgetattr on a valid master fd fills the zeroed out-param.
    let t = unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        assert_eq!(libc::tcgetattr(master, &mut t), 0, "tcgetattr(master)");
        t
    };
    assert_ne!(t.c_iflag & libc::IUTF8, 0, "spawned pty must carry IUTF8");
    // SAFETY: cfgetospeed only reads the termios.
    assert_eq!(unsafe { libc::cfgetospeed(&t) }, libc::B230400);
    // SAFETY: close the master (sleep gets SIGHUP via slave close at exit).
    unsafe {
        libc::close(master);
    }
}

/// Spawn `argv` on a fresh PTY through the real spawn seam (so the
/// child's own `tcsetattr` on the SLAVE is what a master read sees) and
/// return the master. Test-only helper for the spawn-seam tests: the
/// `tty_echo_*` reads, the termios delta check and the close-on-exec dup.
fn spawn_on_pty(argv: &[&str]) -> i32 {
    // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let exec: Vec<std::ffi::OsString> = argv.iter().map(std::ffi::OsString::from).collect();
    let master = spawn_shell(
        24,
        80,
        &spawn_cap,
        &sandbox_cap,
        &[],
        None, // shell_override
        None, // shell_args
        None, // argv_override
        Some(&exec),
        None,
        None,
    )
    .expect("child must spawn");
    assert!(master >= 0);
    master
}

/// Poll [`tty_echo`] on `master` until `want` answers `true` or
/// [`HANG_DETECTOR`] passes (a `/bin/sh -c 'stty …; sleep 5'` child needs a
/// few ms to reach its `stty`, and holds the mode for 5 s after). Returns
/// the last reading either way so a failure names it.
fn wait_tty_echo(master: i32, want: impl Fn(crate::TtyEcho) -> bool) -> Option<crate::TtyEcho> {
    let deadline = std::time::Instant::now() + HANG_DETECTOR;
    loop {
        let last = tty_echo(master);
        if last.is_some_and(&want) || std::time::Instant::now() >= deadline {
            return last;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// The kernel's default signal characters — `^C`, `^\`, `^Z` — which a
/// fresh pty's slave carries (the spawn seam copies the kernel defaults).
const DEFAULT_SIGNALS: crate::TtySignals = crate::TtySignals {
    intr: Some(0x03),
    quit: Some(0x1c),
    susp: Some(0x1a),
};

/// THE MASTER REPORTS THE SLAVE'S ECHO/ICANON — the fact the rainbow's
/// "a press the tty will not echo banks no credit" law rides on.
///
/// Three real children, each setting its OWN termios on the slave the
/// way the programs they stand for do, read back through the master:
///
/// * `sleep` (a cooked tty, the shell prompt's baseline): echo + canonical
///   — the kernel echoes, the press banks.
/// * `stty -echo` (what `read -s`, `sudo`, `ssh`'s passphrase, `passwd`
///   and `getpass` do — measured: all four clear ECHO and keep
///   ICANON): no echo, canonical — `swallows_input()`.
/// * `stty raw -echo` (a TUI: Claude Code's `setRawMode`, vim, less):
///   no echo, NOT canonical — the program echoes for itself, so this is
///   NOT a swallow; the stall fix's whole premise is that these presses
///   keep banking.
///
/// Non-vacuous in both directions: the cooked and raw children prove the
/// verdict is `false` on both sides of the `read -s` shape, so a reader
/// that ignored ICANON (any `!echo`) or ignored ECHO (any `canonical`)
/// fails here.
#[test]
fn tty_echo_reads_the_slaves_canonical_no_echo_through_the_master() {
    // Cooked: the baseline every shell prompt starts from.
    let cooked = spawn_on_pty(&["/bin/sleep", "5"]);
    let t = wait_tty_echo(cooked, |t| t.echo && t.canonical).expect("tcgetattr(master)");
    assert_eq!(
        t,
        crate::TtyEcho {
            echo: true,
            canonical: true,
            signals: Some(DEFAULT_SIGNALS),
        },
        "sleep on a fresh pty is cooked: echo + canonical + ISIG"
    );
    assert!(!t.swallows_input(), "a cooked tty echoes: the press banks");
    // SAFETY: close the master (the child gets SIGHUP via slave close at exit).
    unsafe {
        libc::close(cooked);
    }

    // Password mode: `read -s` / sudo / ssh / passwd / getpass.
    let secret = spawn_on_pty(&["/bin/sh", "-c", "stty -echo; sleep 5"]);
    let t = wait_tty_echo(secret, |t| !t.echo).expect("tcgetattr(master)");
    assert_eq!(
        t,
        crate::TtyEcho {
            echo: false,
            canonical: true,
            signals: Some(DEFAULT_SIGNALS),
        },
        "`stty -echo` on the SLAVE must be visible through the MASTER"
    );
    assert!(
        t.swallows_input(),
        "canonical no-echo is iTerm2's password mode: the press will never echo"
    );
    // SAFETY: as above.
    unsafe {
        libc::close(secret);
    }

    // Raw: a TUI that echoes for itself — NOT a swallow.
    let raw = spawn_on_pty(&["/bin/sh", "-c", "stty raw -echo; sleep 5"]);
    let t = wait_tty_echo(raw, |t| !t.echo && !t.canonical).expect("tcgetattr(master)");
    assert_eq!(
        t,
        crate::TtyEcho {
            echo: false,
            canonical: false,
            signals: None,
        },
        "`stty raw -echo` on the SLAVE must be visible through the MASTER (raw clears ISIG)"
    );
    assert!(
        !t.swallows_input(),
        "a raw-mode program draws its own echo: the press keeps banking (the stall fix)"
    );
    // SAFETY: as above.
    unsafe {
        libc::close(raw);
    }
}

/// A NON-TTY ANSWERS `None`, never a verdict: a pipe sink (every
/// headless test), the `-1` sentinel, and a closed fd all leave the
/// caller on its default path. Fail-open by construction — the reader
/// can only ever WITHHOLD a bank on positive evidence.
#[test]
fn tty_echo_answers_none_off_a_tty() {
    assert_eq!(tty_echo(-1), None, "the sentinel fd is not a tty");
    let mut pipe = [0i32; 2];
    // SAFETY: `pipe` is a valid two-element out-array.
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    assert_eq!(tty_echo(pipe[0]), None, "a pipe read end is not a tty");
    assert_eq!(tty_echo(pipe[1]), None, "a pipe write end is not a tty");
    // SAFETY: both ends are ours and open.
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
    // A number no process can hold, never one just freed: a sibling test's
    // pty may already have taken it.
    assert_eq!(tty_echo(libc::c_int::MAX), None, "a bad fd is not a tty");
}

/// A pty pair for the queue probes, with the SLAVE's termios set by the
/// test itself (no child, no race): canonical with ECHO off, or
/// `cfmakeraw` with `VMIN = vmin` and `VTIME = 0`. From the seam's own
/// close-on-exec opener, never `openpty`, for the reason
/// `cloexec_controls_master_survival_across_exec` gives.
#[cfg(target_os = "macos")]
fn queue_pair(raw: bool, vmin: u8) -> (i32, i32) {
    let (master, slave) = open_pty_pair_cloexec(None, None).expect("pty pair");
    // SAFETY: `libc::termios` is plain integer fields and a byte array; an
    // all-zeros bit pattern is a valid value, overwritten by `tcgetattr`.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `slave` is this test's live pty slave; `t` is a valid out-param.
    assert_eq!(unsafe { libc::tcgetattr(slave, &mut t) }, 0);
    if raw {
        // SAFETY: `t` is a live, initialised termios on this stack.
        unsafe { libc::cfmakeraw(&mut t) };
        t.c_cc[libc::VMIN] = vmin;
        t.c_cc[libc::VTIME] = 0;
    } else {
        t.c_lflag |= libc::ICANON;
        t.c_lflag &= !libc::ECHO;
    }
    // SAFETY: `slave` is live; `t` is the termios just derived from its own.
    assert_eq!(unsafe { libc::tcsetattr(slave, libc::TCSANOW, &t) }, 0);
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

/// CANONICAL MODE COUNTS COMPLETE LINES ONLY: `abc` typed at a cooked
/// slave reads 0 — the partial line is the line discipline's, where no
/// `read(2)` can see it — and the `\n` that completes it reads 4. The
/// consumer's word for this is `typeahead`, never a stall: a count here
/// is a whole line the program has not asked for yet, the shell's
/// ordinary type-ahead.
#[test]
#[cfg(target_os = "macos")]
fn queue_len_canonical_counts_complete_lines_only() {
    let (master, slave) = queue_pair(false, 1);
    assert_eq!(
        input_queue_len(master),
        Some(0),
        "an empty queue reads 0, not None"
    );
    type_into(master, b"abc");
    assert_eq!(
        input_queue_len(master),
        Some(0),
        "a partial canonical line is invisible"
    );
    type_into(master, b"\n");
    assert_eq!(
        input_queue_len(master),
        Some(4),
        "the completed line counts whole"
    );
    assert_eq!(read_exactly(slave, 4), b"abc\n");
    assert_eq!(
        input_queue_len(master),
        Some(0),
        "the program read its line"
    );
    close_pair(master, slave);
}

/// RAW MODE COUNTS EVERY BYTE, and the count falls as the program reads
/// — an unread `\r` with a driver's `\x1b[B` queued behind it reads 1
/// then 4. A program
/// that reads ONE byte leaves 3, so the reading is the unread tail, not a
/// high-water mark.
#[test]
#[cfg(target_os = "macos")]
fn queue_len_raw_counts_every_byte_and_falls_as_the_slave_reads() {
    let (master, slave) = queue_pair(true, 1);
    type_into(master, b"\r");
    assert_eq!(input_queue_len(master), Some(1), "one raw Enter, unread");
    type_into(master, b"\x1b[B");
    assert_eq!(
        input_queue_len(master),
        Some(4),
        "the down arrow queued behind it"
    );
    assert_eq!(read_exactly(slave, 1), b"\r");
    assert_eq!(
        input_queue_len(master),
        Some(3),
        "the unread tail after a 1-byte read"
    );
    assert_eq!(read_exactly(slave, 3), b"\x1b[B");
    assert_eq!(input_queue_len(master), Some(0));
    close_pair(master, slave);
}

/// RAW WITH `VMIN` ABOVE THE QUEUED COUNT AND `VTIME = 0` READS 0: the
/// driver answers what a `read(2)` would return right now, and that read
/// would still be waiting for `VMIN` bytes. A named undercount — the
/// probe never reports bytes the program could not yet read — pinned so a
/// consumer never reads this 0 as "the program drained its input".
#[test]
#[cfg(target_os = "macos")]
fn queue_len_raw_below_vmin_reads_zero() {
    let (master, slave) = queue_pair(true, 5);
    type_into(master, b"abc");
    assert_eq!(
        input_queue_len(master),
        Some(0),
        "3 bytes under VMIN=5 read as 0"
    );
    type_into(master, b"de");
    assert_eq!(
        input_queue_len(master),
        Some(5),
        "VMIN met: the whole queue counts"
    );
    close_pair(master, slave);
}

/// THE FLUSH ON THE MASTER: `flush_input_queue(master)`
/// discards exactly the slave's unread INPUT and reports its count — a
/// `\r` and the `\x1b[B` behind it, 4 bytes — and leaves the
/// program's unread OUTPUT alone. An empty queue flushes to `Some(0)`;
/// off a tty it is `None`, and nothing is touched.
#[test]
#[cfg(target_os = "macos")]
fn flush_input_queue_discards_the_unread_input_and_nothing_else() {
    use std::os::fd::AsRawFd;
    let (master, slave) = queue_pair(true, 1);
    type_into(master, b"\r\x1b[B");
    type_into(slave, b"drawn");
    assert_eq!(flush_input_queue(master), Some(4), "the discarded count");
    assert_eq!(input_queue_len(master), Some(0), "the input is gone");
    assert_eq!(output_queue_len(master), Some(5), "the output is not");
    assert_eq!(read_exactly(master, 5), b"drawn");
    assert_eq!(flush_input_queue(master), Some(0), "nothing left to drop");
    type_into(master, b"x");
    assert_eq!(read_exactly(slave, 1), b"x", "the queue still works");
    close_pair(master, slave);
    let (a, b) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    assert_eq!(flush_input_queue(a.as_raw_fd()), None);
    assert_eq!(flush_input_queue(-1), None);
    drop(b);
}

/// A SIGNAL CHARACTER IS NEVER QUEUED WHILE ISIG IS SET — the fact the
/// unread-input gate's exemption rides on. With
/// ICANON off and ISIG on (cbreak: readline between keys, `watch`,
/// `htop`) a `q` the program has not read counts 1, and a `^C` written
/// behind it is turned into SIGINT as it is written and flushes the queue
/// to 0 — it can never be read after the `q`. In raw mode (ISIG off) the
/// same `^C` is a byte queued behind the `q`: 2. `tty_echo` reports the
/// characters the line discipline acts on — a remapped `VINTR` moves the
/// signal (`^C` then queues, `^G` flushes) and a disabled one
/// (`_POSIX_VDISABLE`) reads `None`.
#[test]
#[cfg(target_os = "macos")]
fn queue_len_never_counts_a_signal_character_while_isig_is_set() {
    let set_lflag = |slave: i32, edit: &dyn Fn(&mut libc::termios)| {
        // SAFETY: all-zeros is a valid termios, overwritten by `tcgetattr`.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `slave` is this test's live pty slave; `t` is an out-param.
        assert_eq!(unsafe { libc::tcgetattr(slave, &mut t) }, 0);
        edit(&mut t);
        // SAFETY: `slave` is live; `t` is derived from its own termios.
        assert_eq!(unsafe { libc::tcsetattr(slave, libc::TCSANOW, &t) }, 0);
    };

    // Cbreak: raw plus ISIG.
    let (master, slave) = queue_pair(true, 1);
    set_lflag(slave, &|t| t.c_lflag |= libc::ISIG);
    let echo = tty_echo(master).expect("tcgetattr(master)");
    assert!(!echo.canonical);
    assert_eq!(echo.signals, Some(DEFAULT_SIGNALS));
    for signal in [0x03, 0x1c, 0x1a] {
        assert!(DEFAULT_SIGNALS.signals(signal), "{signal:#04x}");
        type_into(master, b"q");
        assert_eq!(input_queue_len(master), Some(1), "one unread `q`");
        type_into(master, &[signal]);
        assert_eq!(
            input_queue_len(master),
            Some(0),
            "{signal:#04x} was a signal, not a byte behind the `q`"
        );
    }
    assert!(!DEFAULT_SIGNALS.signals(b'q'));
    // A remapped VINTR and a disabled VQUIT.
    set_lflag(slave, &|t| {
        t.c_cc[libc::VINTR] = 0x07;
        t.c_cc[libc::VQUIT] = 0xff;
    });
    let signals = tty_echo(master).and_then(|e| e.signals).expect("ISIG");
    assert_eq!(signals.intr, Some(0x07));
    assert_eq!(signals.quit, None, "_POSIX_VDISABLE signals nothing");
    assert!(!signals.signals(0x03) && !signals.signals(0xff));
    type_into(master, b"q\x03");
    assert_eq!(input_queue_len(master), Some(2), "`^C` is a byte now");
    type_into(master, b"\x07");
    assert_eq!(input_queue_len(master), Some(0), "`^G` is the signal");
    close_pair(master, slave);

    // Raw: ISIG off, so `^C` queues behind the `q` like any byte.
    let (master, slave) = queue_pair(true, 1);
    assert_eq!(tty_echo(master).map(|e| e.signals), Some(None));
    type_into(master, b"q\x03");
    assert_eq!(input_queue_len(master), Some(2));
    assert_eq!(read_exactly(slave, 2), b"q\x03");
    close_pair(master, slave);
}

/// A BYTE THE LINE DISCIPLINE CONSUMES LOOKS LIKE A READ: only a slave with ICANON,
/// ISIG, IEXTEN, IXON and IGNCR all clear queues every byte written, so
/// only there can a shrinking queue mean the program read. Raw
/// (`cfmakeraw`) passes every byte; each flag alone makes the driver eat
/// some byte with nothing reading the slave — `^C` (ISIG), `^S` (IXON),
/// `^O` (IEXTEN), `\r` (IGNCR) — which is measured here, so the answer is
/// tied to the kernel's behaviour, not to a list. Off a tty: `None`.
#[test]
#[cfg(target_os = "macos")]
fn tty_passes_every_byte_only_in_raw_mode() {
    use std::os::fd::AsRawFd;
    let (master, slave) = queue_pair(true, 1);
    assert_eq!(tty_passes_every_byte(master), Some(true), "cfmakeraw");
    type_into(master, b"\x03\x13\x0f\r");
    assert_eq!(input_queue_len(master), Some(4), "raw queues all four");
    close_pair(master, slave);
    // (name, the c_lflag bits, the c_iflag bits, a byte that flag eats)
    let flags: [(&str, libc::tcflag_t, libc::tcflag_t, u8); 4] = [
        ("ISIG", libc::ISIG, 0, 0x03),
        ("IXON", 0, libc::IXON, 0x13),
        ("IEXTEN", libc::IEXTEN, 0, 0x0f),
        ("IGNCR", 0, libc::IGNCR, b'\r'),
    ];
    for (name, lflag, iflag, eaten) in flags {
        let (master, slave) = queue_pair(true, 1);
        // SAFETY: all-zeros is a valid termios, overwritten by `tcgetattr`.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `slave` is this test's live pty slave; `t` is an out-param.
        assert_eq!(unsafe { libc::tcgetattr(slave, &mut t) }, 0);
        t.c_lflag |= lflag;
        t.c_iflag |= iflag;
        // SAFETY: `slave` is live; `t` is derived from its own termios.
        assert_eq!(unsafe { libc::tcsetattr(slave, libc::TCSANOW, &t) }, 0);
        assert_eq!(tty_passes_every_byte(master), Some(false), "{name}");
        type_into(master, &[eaten]);
        assert_eq!(
            input_queue_len(master),
            Some(0),
            "{name}: {eaten:#04x} left the queue with nothing reading it"
        );
        close_pair(master, slave);
    }
    let (master, slave) = queue_pair(false, 1);
    assert_eq!(tty_passes_every_byte(master), Some(false), "canonical");
    close_pair(master, slave);
    let (a, b) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    assert_eq!(tty_passes_every_byte(a.as_raw_fd()), None);
    assert_eq!(tty_passes_every_byte(-1), None);
    drop(b);
}

/// THE OUTPUT QUEUE: a raw slave writing 10 bytes reads 10 at the master
/// until the master reads them, then 0. And the two probes are two
/// questions, not one: the program's unread OUTPUT never shows in
/// `input_queue_len`, which is the reason the probe is macOS-only (a
/// Linux master's `FIONREAD` is this output count).
#[test]
#[cfg(target_os = "macos")]
fn queue_len_output_counts_what_the_master_has_not_read() {
    let (master, slave) = queue_pair(true, 1);
    assert_eq!(output_queue_len(master), Some(0));
    // SAFETY: a bounded write of a live slice to this test's live slave.
    let wrote = unsafe { libc::write(slave, b"0123456789".as_ptr().cast(), 10) };
    assert_eq!(wrote, 10);
    assert_eq!(
        output_queue_len(master),
        Some(10),
        "the program's unread output"
    );
    assert_eq!(
        input_queue_len(master),
        Some(0),
        "output is not unread input"
    );
    assert_eq!(read_exactly(master, 10), b"0123456789");
    assert_eq!(output_queue_len(master), Some(0), "aterm read it");
    close_pair(master, slave);
}

/// OFF A TTY BOTH PROBES ANSWER `None`, never a count: a pipe has a
/// perfectly good `FIONREAD`, which is exactly why the probe asks
/// `isatty` first — a pipe sink (every headless test) must read as "not
/// measured", or a consumer would gate on a pipe's buffer.
#[test]
fn queue_len_answers_none_off_a_tty() {
    assert_eq!(input_queue_len(-1), None, "the sentinel fd");
    assert_eq!(output_queue_len(-1), None, "the sentinel fd");
    let mut pipe = [0i32; 2];
    // SAFETY: `pipe` is a valid two-element out-array.
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    // SAFETY: a bounded write to this test's live pipe write end.
    assert_eq!(
        unsafe { libc::write(pipe[1], b"xyz".as_ptr().cast(), 3) },
        3
    );
    assert_eq!(
        input_queue_len(pipe[0]),
        None,
        "a pipe with 3 bytes in it is not a tty"
    );
    assert_eq!(output_queue_len(pipe[0]), None, "a pipe is not a tty");
    assert_eq!(input_queue_len(pipe[1]), None, "the write end is not a tty");
    // SAFETY: both ends are ours and open.
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
    // A number no process can hold, never one just freed: a sibling test's
    // pty may already have taken it.
    assert_eq!(
        input_queue_len(libc::c_int::MAX),
        None,
        "a bad fd is not a tty"
    );
}

/// OFF macOS THE PROBES ANSWER `None` EVEN ON A REAL PTY MASTER: a
/// Linux master's `FIONREAD` counts unread OUTPUT, so a number here would
/// answer a different question than the one the consumer asks. "Not
/// measured" keeps every consumer's gate open on these platforms.
#[test]
#[cfg(not(target_os = "macos"))]
fn queue_len_answers_none_off_macos() {
    let (master, slave) = open_pty_pair_cloexec(None, None).expect("pty pair");
    assert!(fd_is_tty(master), "the fixture is a real pty");
    assert_eq!(input_queue_len(master), None);
    assert_eq!(output_queue_len(master), None);
    // SAFETY: both fds are this test's and still open.
    unsafe {
        libc::close(slave);
        libc::close(master);
    }
}

/// A DUPLICATE OF THE MASTER IS CLOSE-ON-EXEC TOO.
///
/// The spawn seam above opens both PTY ends close-on-exec FROM BIRTH, but
/// that guarantee is only as strong as every copy anyone later makes of
/// them. `dup(2)` is defined by POSIX to CLEAR `FD_CLOEXEC` on the new
/// descriptor, so the spill drainer — which duplicates the master and holds
/// its copy for as long as a wedged shell keeps the write queue spilling —
/// must not hand any concurrently-`exec`ing process a writable descriptor
/// onto that session's terminal for that whole period.
///
/// Non-vacuous in both directions: it asserts the ORIGINAL is close-on-exec
/// (so a regression in the spawn seam cannot make this pass by accident) and
/// that a plain `dup` of the same fd is NOT — which is the property that
/// makes `F_DUPFD_CLOEXEC` load-bearing rather than decorative.
#[test]
fn a_duplicated_master_stays_close_on_exec() {
    let master = spawn_on_pty(&["/bin/sleep", "5"]);

    // SAFETY: F_GETFD only reads the descriptor flags of a valid fd.
    let flags_of = |fd: i32| unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(
        flags_of(master) & libc::FD_CLOEXEC != 0,
        "PRECONDITION: the spawn seam opens the master close-on-exec"
    );

    let dup = dup_fd(master).expect("dup_fd must succeed on a live master");
    assert!(
        flags_of(std::os::fd::AsRawFd::as_raw_fd(&dup)) & libc::FD_CLOEXEC != 0,
        "a dup_fd copy of the master must be close-on-exec: the drainer holds \
         this for the whole spill, so a bare dup(2) leaks the tty into every \
         process that execs meanwhile"
    );

    // NON-VACUITY: prove the flag is not simply inherited by duplication —
    // a plain `dup` of the SAME fd clears it, which is the defect.
    // SAFETY: `dup` on a valid fd; the result is closed below.
    let bare = unsafe { libc::dup(master) };
    assert!(bare >= 0, "dup must succeed");
    assert_eq!(
        flags_of(bare) & libc::FD_CLOEXEC,
        0,
        "control: POSIX dup(2) clears FD_CLOEXEC, so the assertion above is \
         testing F_DUPFD_CLOEXEC rather than restating inheritance"
    );

    // SAFETY: closing fds this test exclusively owns.
    unsafe {
        libc::close(bare);
        libc::close(master);
    }
}

// ---- write_all branch logic (pure, via the extracted classifier) ----
//
// The EINTR-retry / short-write / peer-closed branch ladder of `write_all` is
// a behavior-preserving extraction into `classify_write_result`. Testing the
// pure classifier covers the EXACT decision the loop drives, WITHOUT having to
// provoke a real (timing-dependent, flaky) `EINTR`.

#[test]
fn classify_write_result_decides_each_write_all_step() {
    // (write(2)'s return, errno is EINTR) => the step the loop takes.
    for (r, is_eintr, want) in [
        // EINTR => retry the write (do not drop the rest of the buffer).
        (-1, true, WriteStep::Retry),
        // Any other errno (EIO, EBADF, EPIPE, …) => stop: the master is gone.
        (-1, false, WriteStep::Stop),
        // 0 => peer closed; stop draining (errno is irrelevant here).
        (0, false, WriteStep::Stop),
        (0, true, WriteStep::Stop),
        // r > 0 => advance the cursor by EXACTLY r bytes (short-write handling).
        (1, false, WriteStep::Advance(1)),
        (4096, false, WriteStep::Advance(4096)),
    ] {
        assert_eq!(
            classify_write_result(r, is_eintr),
            want,
            "r={r} eintr={is_eintr}"
        );
    }
}

// ---- locale resolution: the child always runs under a UTF-8 LC_CTYPE ----
//
// A present-but-non-UTF-8 locale (LANG=C, bare en_US, LC_ALL=C, a stray
// non-UTF-8 LC_CTYPE) must not reach the child, or programs like emacs
// re-encode pasted box-drawing UTF-8 to ASCII `?`. These pin
// `resolve_spawn_locale` (which `build_child_env` then composes onto the env).

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
    // No codeset, or a non-UTF-8 one, is NOT UTF-8.
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

#[test]
fn resolve_spawn_locale_edge_cases() {
    let kv = |k: &str, v: &str| (k.to_string(), v.to_string());
    // The injected value is platform-aware (C.UTF-8 off macOS; see SPAWN_UTF8_LOCALE).
    let ctype = || vec![kv("LC_CTYPE", SPAWN_UTF8_LOCALE)];
    let ctype_and_neutralize = || vec![kv("LC_CTYPE", SPAWN_UTF8_LOCALE), kv("LC_ALL", "")];

    // All unset (Finder/.app launch): inject the encoding category.
    assert_eq!(resolve_spawn_locale(None, None, None), ctype());
    // present-but-non-UTF-8 LANG — the emacs `?` repro.
    assert_eq!(resolve_spawn_locale(None, None, Some("C")), ctype());
    assert_eq!(resolve_spawn_locale(None, None, Some("POSIX")), ctype());
    // bare LANG (no codeset).
    assert_eq!(resolve_spawn_locale(None, None, Some("en_US")), ctype());
    // a non-UTF-8 LC_CTYPE shadowing a UTF-8 LANG: override LC_CTYPE; LANG untouched.
    assert_eq!(
        resolve_spawn_locale(None, Some("en_US.ISO8859-1"), Some("en_US.UTF-8")),
        ctype()
    );
    // LC_ALL=C dominating a UTF-8 LANG: override LC_CTYPE AND neutralize LC_ALL,
    // else the LC_CTYPE override would be dead (LC_ALL > LC_CTYPE).
    assert_eq!(
        resolve_spawn_locale(Some("C"), None, Some("en_US.UTF-8")),
        ctype_and_neutralize()
    );
    // both LC_ALL and LC_CTYPE non-UTF-8 at once: LC_ALL wins, same outcome.
    assert_eq!(
        resolve_spawn_locale(Some("C"), Some("en_US.ISO8859-1"), None),
        ctype_and_neutralize()
    );

    // Already UTF-8 anywhere in the effective slot: change NOTHING (no clobber).
    assert!(resolve_spawn_locale(None, None, Some("en_US.UTF-8")).is_empty());
    assert!(resolve_spawn_locale(Some("en_US.UTF-8"), None, None).is_empty());
    assert!(resolve_spawn_locale(None, Some("fr_FR.UTF-8"), Some("C")).is_empty());
    // A UTF-8 LC_ALL must NOT be touched even though a lower slot is non-UTF-8.
    assert!(resolve_spawn_locale(Some("en_US.UTF-8"), Some("C"), Some("C")).is_empty());
    // set-but-empty falls through (POSIX): empty LC_ALL/LC_CTYPE + UTF-8 LANG -> nothing.
    assert!(resolve_spawn_locale(Some(""), Some(""), Some("en_US.UTF-8")).is_empty());
    // UTF-8 spelling variants are all recognized (no needless override).
    for v in ["en_US.UTF8", "en_US.utf-8", "de_DE.UTF-8@euro"] {
        assert!(
            resolve_spawn_locale(None, None, Some(v)).is_empty(),
            "{v} is UTF-8"
        );
    }
}

/// CONFORMANCE: drive the REAL `resolve_spawn_locale` + `build_child_env` over
/// every inherited-locale shape and assert the child's effective `LC_CTYPE` is
/// UTF-8. The UTF-8/precedence oracle here is written INDEPENDENTLY of
/// `is_utf8_locale` (a `.ends_with` check vs the production codeset parse) so a
/// shared predicate bug cannot make the assertion vacuous.
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
    // Representative concrete value per class (None = the var is unset).
    let val = |c: Cls| match c {
        Cls::Unset => None,
        Cls::Empty => Some(""),
        Cls::NonUtf8 => Some("C"),
        Cls::Utf8 => Some("en_US.UTF-8"),
    };
    // Independent codeset check (different impl than `is_utf8_locale`).
    let looks_utf8 = |s: &str| {
        let lo = s.to_ascii_lowercase();
        lo.ends_with(".utf-8") || lo.ends_with(".utf8")
    };
    // Effective LC_CTYPE of a composed child env (POSIX precedence, empty==unset).
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
    let mut checked = 0u32;
    for &a in &classes {
        for &c in &classes {
            for &l in &classes {
                // Build the inherited env the child would have started from.
                let mut inherited: Vec<(OsString, OsString)> =
                    vec![(OsString::from("PATH"), OsString::from("/usr/bin"))];
                for (k, cl) in [("LC_ALL", a), ("LC_CTYPE", c), ("LANG", l)] {
                    if let Some(v) = val(cl) {
                        inherited.push((OsString::from(k), OsString::from(v)));
                    }
                }

                let overrides = resolve_spawn_locale(val(a), val(c), val(l));

                // INDEPENDENT "was the inherited effective locale already UTF-8?"
                let ne = |cl: Cls| val(cl).filter(|s| !s.is_empty());
                let orig_utf8 = ne(a).or(ne(c)).or(ne(l)).map(looks_utf8).unwrap_or(false);

                // No-clobber & always-fix: overrides are empty IFF already UTF-8.
                assert_eq!(
                    overrides.is_empty(),
                    orig_utf8,
                    "overrides emptiness must track already-UTF-8 for (LC_ALL,LC_CTYPE,LANG)=({:?},{:?},{:?})",
                    val(a),
                    val(c),
                    val(l)
                );

                // A dominating non-UTF-8 LC_ALL must be neutralized, else the
                // LC_CTYPE override would be powerless.
                if matches!(a, Cls::NonUtf8) {
                    assert!(
                        overrides.iter().any(|(k, v)| k == "LC_ALL" && v.is_empty()),
                        "non-UTF-8 LC_ALL must be neutralized; got {overrides:?}"
                    );
                }

                // THE INVARIANT: the child the terminal spawns is UTF-8.
                let child = build_child_env(inherited.into_iter(), &overrides);
                assert!(
                    child_ctype_utf8(&child),
                    "child LC_CTYPE NOT UTF-8 for inherited (LC_ALL,LC_CTYPE,LANG)=({:?},{:?},{:?}); env={:?}",
                    val(a),
                    val(c),
                    val(l),
                    child
                );
                checked += 1;
            }
        }
    }
    assert_eq!(
        checked, 64,
        "all 4x4x4 inherited-locale shapes must be exercised"
    );
}

// ---- read() syscall wrapper: EOF and bad-fd error contract ----

// EOF: when the write end of a pipe is closed and the buffer is drained, a
// `read` of the read end returns exactly 0 (not negative, not a partial-read
// surprise). This is the `0 = EOF` half of the documented `read` contract.
#[test]
fn read_returns_zero_on_eof_after_write_end_closed() {
    let mut fds = [0i32; 2];
    // SAFETY: `fds` is a valid 2-element buffer for `pipe`.
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe() failed");
    let (rd, wr) = (fds[0], fds[1]);
    // Close the only write end with no data pending => the next read sees EOF.
    // SAFETY: `wr` is the pipe write end we just opened.
    unsafe {
        libc::close(wr);
    }
    let mut buf = [0u8; 16];
    let n = read(rd, &mut buf);
    assert_eq!(n, 0, "read at EOF must return 0, got {n}");
    // SAFETY: closing the read end we opened.
    unsafe {
        libc::close(rd);
    }
}

// Error: a `read` of an invalid descriptor must return a negative value (the
// `< 0 = error` half of the contract), with `errno == EBADF`. We use fd -1,
// which is never a valid descriptor, so this is hermetic and deterministic and
// never touches a real, possibly-open fd. (We assert the raw errno, not
// `ErrorKind`, because libstd categorizes EBADF as `Uncategorized` here — the
// stable contract is the negative return + the POSIX errno, not the kind.)
#[test]
fn read_returns_negative_with_ebadf_on_invalid_fd() {
    let mut buf = [0u8; 16];
    let n = read(-1, &mut buf);
    assert!(n < 0, "read on a bad fd must be negative, got {n}");
    let err = io::Error::last_os_error();
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EBADF),
        "read on a bad fd must set errno=EBADF, got {err}",
    );
    // And EBADF is NOT EINTR, so the read loop would STOP (not spin-retry) on it
    // — the very decision the classifier encodes.
    assert_ne!(err.kind(), io::ErrorKind::Interrupted);
}

// Zero-length buffer: `read(2)` with count 0 returns 0 WITHOUT consuming any
// of the pending bytes. This is the third arm of the wrapper's return
// contract, and the one that is easy to get wrong: `buf.as_mut_ptr()` on an
// empty slice is a dangling-but-aligned pointer, so a wrapper that did any
// pointer math or passed a bogus count would fault or, worse, silently eat
// the queued data. We prove BOTH halves — the 0 return AND that the data is
// still there afterwards — so a regression cannot hide behind the 0.
#[test]
fn read_into_an_empty_buffer_reads_nothing() {
    let mut fds = [0i32; 2];
    // SAFETY: `fds` is a valid 2-element buffer for `pipe`.
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe() failed");
    let (rd, wr) = (fds[0], fds[1]);

    // Queue data on the pipe so a "reads nothing" result is meaningful: the
    // read end is READABLE, so a 0 return can only come from the count, not
    // from an empty pipe. (Well under the pipe buffer, so no blocking.)
    let pending: &[u8] = b"data-is-waiting";
    write_all(wr, pending);

    let mut empty: [u8; 0] = [];
    let n = read(rd, &mut empty);
    assert_eq!(
        n, 0,
        "zero-length read must return 0 even with data pending"
    );

    // The pending bytes must be UNCONSUMED — a real read still sees them all.
    let mut buf = [0u8; 64];
    let n = read(rd, &mut buf);
    assert!(n > 0, "the queued bytes must survive the zero-length read");
    assert_eq!(
        &buf[..n as usize],
        pending,
        "the zero-length read must not have consumed any pending byte",
    );

    // SAFETY: closing both pipe ends we opened above.
    unsafe {
        libc::close(rd);
        libc::close(wr);
    }
}

// ---- fail-closed spawn: under-tier capability is denied WITHOUT forking ----

// An under-tier `Cap<Spawn>` (Untrusted, below the required Trusted) must be
// rejected by the PARENT gate BEFORE any fork — there must be no way to
// spawn a child with an insufficient capability. We assert PermissionDenied;
// the absence of a leaked child is implicit (no fork happened, so there is
// nothing to reap), and the error originates from `aterm_cap::require`, not
// from a child status byte.
#[test]
fn under_tier_spawn_cap_is_denied_before_forking() {
    // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    // Untrusted spawn cap: below the Trusted floor `spawn_shell` requires.
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

// ---- fail-closed spawn: a child that cannot exec takes the _exit(127) path ----

// The exec-failure path through the REAL production code: a `-e` command naming
// a nonexistent absolute program forces the child's `execve` to fail, so the
// child writes the b'E' status byte and `_exit(127)`s. The parent reads that
// byte off the status pipe, reaps the child internally, and surfaces an
// `io::Error` (ErrorKind::Other) describing the pre-exec exec failure — never a
// master fd. This drives a real fork/exec + the full status-pipe protocol.
//
// NOTE on "$SHELL in the child": `spawn_shell` resolves the exec target in the
// PARENT (it must, to stay async-signal-safe in the child), so a bogus `$SHELL`
// can only be injected by mutating the parent's env — which is a data race
// against the multi-threaded test harness under edition 2024. We therefore
// drive the SAME child exec-failure path hermetically via a bogus `exec_command`
// (no env mutation). The raw 127 exit code is consumed by `spawn_shell`'s own
// `waitpid` reap and is not observable here; the parent's verdict is the
// status byte.
#[test]
fn bogus_exec_command_takes_child_exec_failure_path() {
    // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);

    // An absolute path that cannot exist => `resolve_program` returns it
    // verbatim => the child's `execve` fails => b'E' + _exit(127).
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
    assert_eq!(
        err.to_string(),
        "`/nonexistent/aterm-pty-no-such-prog-xyz` does not exist",
        "the error names the missing program",
    );
}

// What the exec-failure line says for each thing that can be wrong with the
// target, and whose setting it names. Pure over a scratch folder: no spawn.
#[test]
fn an_exec_failure_says_what_is_wrong_with_the_target() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = std::env::temp_dir().join(format!("aterm-pty-exec-failure-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("folder")).expect("scratch folder");
    let plain = dir.join("plain");
    std::fs::write(&plain, b"#!/bin/sh\n").expect("a non-executable file");
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
    let runnable = dir.join("runnable");
    std::fs::write(&runnable, b"not a program").expect("an executable file");
    std::fs::set_permissions(&runnable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let c = |p: &std::path::Path| CString::new(p.as_os_str().as_bytes()).unwrap();
    let shell = " (from $SHELL)";

    assert_eq!(
        exec_failure(&c(&dir.join("zhs")), shell, false, None),
        format!(
            "`{}` does not exist (from $SHELL)",
            dir.join("zhs").display()
        )
    );
    assert_eq!(
        exec_failure(&c(&dir.join("folder")), shell, false, None),
        format!(
            "`{}` is a folder (from $SHELL)",
            dir.join("folder").display()
        )
    );
    assert_eq!(
        exec_failure(&c(&plain), shell, false, None),
        format!("`{}` is not executable (from $SHELL)", plain.display())
    );
    assert_eq!(
        exec_failure(&c(&runnable), "", true, None),
        format!("`{}` could not be run", runnable.display())
    );
    // A bare name is the fault itself: `execve` does no PATH search.
    assert_eq!(
        exec_failure(
            c"fish",
            " (set by --shell or `shell` in aterm.toml)",
            false,
            None
        ),
        "`fish` is not a full path (set by --shell or `shell` in aterm.toml)"
    );
    assert_eq!(
        exec_failure(c"nosuch", "", true, None),
        "`nosuch` is not on PATH"
    );
    assert_eq!(
        exec_failure(c"", shell, false, None),
        "the shell path is empty (from $SHELL)"
    );
    // A relative path is judged from the folder the child ran it in.
    assert_eq!(
        exec_failure(c"./plain", "", true, Some(&c(&dir))),
        "`./plain` is not executable"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- OS-sandbox wrap (sandbox_wrap) ----

// The seam's inlined wrapper path MUST be the SAME bytes as the policy crate's
// canonical SANDBOX_EXEC_PATH. They are kept in lockstep by hand (the seam
// stays dependency-light), so this test fails loudly if either drifts.
#[test]
fn inlined_sandbox_exec_path_matches_policy_crate() {
    assert_eq!(SANDBOX_EXEC_PATH, aterm_containment::SANDBOX_EXEC_PATH);
    assert_eq!(SANDBOX_EXEC_PATH, "/usr/bin/sandbox-exec");
}

// FAIL-CLOSED: when the wrapper binary is absent at the given path,
// build_sandbox_wrap returns NotFound — the caller (`spawn_shell`) propagates
// it and NEVER forks, so a policy-demanded sandbox that can't be applied
// refuses to spawn rather than silently running an unsandboxed shell. We point
// it at a guaranteed-nonexistent path to drive this without disturbing the real
// /usr/bin/sandbox-exec.
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
    assert!(
        err.to_string().contains("fail-closed"),
        "error must describe the fail-closed refusal: {err}",
    );
}

// The wrapped argv has the exact shape the kernel needs: sandbox-exec, -p,
// <profile>, <program-path>, then the original args AFTER argv[0]. The login
// argv[0] ("-zsh") is replaced by the program PATH; "--rcfile FILE" style real
// args are carried through verbatim. Uses the REAL /usr/bin/sandbox-exec path
// (present on macOS) so the access() probe passes.
#[cfg(target_os = "macos")]
#[test]
fn build_sandbox_wrap_produces_correct_argv_shape() {
    let prog = CString::new("/bin/zsh").unwrap();
    // Original argv: a login-shell argv[0] plus a real flag+value pair.
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

// The wrap path is well-formed AND actually applies Seatbelt: wrap a `-e`
// command in the real `(deny network*)` profile and run `/usr/bin/nc` against a
// live loopback listener bound in this parent. WITHOUT the wrap nc connects;
// WITH the wrap the kernel denies network so nc cannot connect — observed via
// the child's exit code (the wrapped sandbox-exec→nc child fails). This drives
// the REAL `spawn_shell` wrap-argv construction end to end, not just a direct
// sandbox-exec call.
#[cfg(target_os = "macos")]
#[test]
fn wrapped_spawn_enforces_network_deny_via_seatbelt() {
    use std::io::Write;
    use std::net::TcpListener;

    // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);

    // Loopback listener in the parent + a draining accept thread.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().unwrap().port();
    let accepter = std::thread::spawn(move || {
        for _ in 0..2 {
            match listener.accept() {
                Ok((mut s, _)) => {
                    let _ = s.write_all(b"x");
                }
                Err(_) => break,
            }
        }
    });
    let port_s = port.to_string();

    // Control: unwrapped `-e nc` to the listener CONNECTS (so the probe works).
    // We can't read nc's exit code through spawn_shell's API, so prove the
    // control via a direct connect from the parent instead, then focus the
    // wrapped assertion on the seam producing a sandbox-exec'd child that the
    // kernel network-denies (nc fails → its PTY closes quickly with no data
    // that looks like a successful connect).
    let probe = std::net::TcpStream::connect(("127.0.0.1", port));
    assert!(
        probe.is_ok(),
        "loopback listener must be connectable (probe)"
    );
    drop(probe);

    // Wrapped `-e nc` under (deny network*). The wrap is built by spawn_shell:
    // sandbox-exec -p <profile> /usr/bin/nc <args>. The connect is denied.
    let nc: Vec<std::ffi::OsString> = vec![
        "/usr/bin/nc".into(),
        "-G".into(),
        "1".into(),
        "-w".into(),
        "1".into(),
        "-z".into(),
        "127.0.0.1".into(),
        port_s.clone().into(),
    ];
    let profile = aterm_containment::BASE_PROFILE;
    let master = spawn_shell(
        24,
        80,
        &spawn_cap,
        &sandbox_cap,
        &[],
        None, // shell_override
        None, // shell_args
        None,
        Some(&nc),
        None,
        Some(profile),
    )
    .expect("wrapped -e nc must spawn (sandbox-exec applies the profile)");
    // Drain to EOF (the child exits fast: nc's connect is denied). The success
    // banner "succeeded!" must NOT appear — a denied connect never prints it.
    let mut out = Vec::new();
    let mut buf = [0u8; 256];
    for _ in 0..200 {
        let n = read(master, &mut buf);
        if n <= 0 {
            break;
        }
        out.extend_from_slice(&buf[..n as usize]);
    }
    // SAFETY: tear down the child.
    unsafe {
        libc::close(master);
    }
    let _ = std::net::TcpStream::connect(("127.0.0.1", port)); // unblock accepter
    let _ = accepter.join();
    let s = String::from_utf8_lossy(&out);
    assert!(
        !s.contains("succeeded"),
        "DENY FAILED: wrapped nc reported a successful connect under (deny network*): {s:?}",
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

/// A `-e` argument reaches the program as the bytes it was given: a raw byte
/// that is not UTF-8 (`aterm --window -e prog $'\xff'`) is not turned into
/// U+FFFD on the way, and the argument after it is where it was.
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

/// An argument holding a NUL byte cannot be handed to a program (`execve`
/// takes C strings), and dropping it would shift every argument after it into
/// its place. The spawn is refused instead, naming the argument, for each list
/// a spawn takes its argv from: `-e`, the argv override and config `shell_args`.
#[test]
fn an_argument_holding_a_nul_refuses_the_spawn_instead_of_shifting_the_rest() {
    // SAFETY: a test process; the trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    fn list<T: From<&'static str>>(v: &[&'static str]) -> Vec<T> {
        v.iter().map(|s| T::from(*s)).collect()
    }
    let exec: Vec<std::ffi::OsString> = list(&["/bin/sh", "-c", ARGV_HEX, "sh", "a\0b", "c"]);
    let args: Vec<String> = list(&["-c", ARGV_HEX, "sh", "a\0b", "c"]);
    let over: Vec<String> = list(&["sh", "-c", ARGV_HEX, "sh", "a\0b", "c"]);
    let spawn = |exec, shell_args, argv_override| {
        spawn_shell_with_pid_cell_px(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            Some("/bin/sh"),
            shell_args,
            argv_override,
            exec,
            None, // cwd
            None, // sandbox_wrap
            aterm_sandbox::Limits::inherit(),
            None, // cell_px
        )
    };
    // Each list's NUL element, by its index in that list.
    for (what, index, got) in [
        ("-e", 4, spawn(Some(&exec[..]), None, None)),
        ("shell_args", 3, spawn(None, Some(&args[..]), None)),
        ("argv_override", 4, spawn(None, None, Some(&over[..]))),
    ] {
        match got {
            Ok(sh) => {
                hangup(sh.pid);
                // SAFETY: closing the master this test owns.
                unsafe { libc::close(sh.master) };
                reap(sh.pid);
                panic!("{what}: an argument holding a NUL was dropped and the rest shifted");
            }
            Err(e) => {
                assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{what}: {e}");
                let said = e.to_string();
                assert!(
                    said.contains("NUL") && said.contains(&format!("argument {index} ")),
                    "{what}: the refusal names the argument: {said}"
                );
            }
        }
    }
}

/// A configured shell holding a NUL byte refuses the spawn instead of running
/// another program. `execve` reads a path only up to its NUL, and this seam once
/// swapped in `/bin/sh` for such a shell, so `shell = "/bin/zsh\u0000x"` with
/// `shell_args = ["-c", "echo $0"]` printed `/bin/sh`. Each branch that runs
/// the shell refuses (config `shell_args`, the argv override, the bare login
/// shell); an `-e` command does not run the shell, so it is not refused for it.
#[test]
fn a_shell_holding_a_nul_refuses_the_spawn_instead_of_running_another() {
    // SAFETY: a test process; the trusted-launcher contract trivially holds.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    fn list<T: From<&'static str>>(v: &[&'static str]) -> Vec<T> {
        v.iter().map(|s| T::from(*s)).collect()
    }
    let args: Vec<String> = list(&["-c", "exit 0"]);
    let over: Vec<String> = list(&["sh", "-c", "exit 0"]);
    let spawn = |shell_args, argv_override, exec| {
        spawn_shell_with_pid_cell_px(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            Some("/bin/sh\0x"),
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
        // SAFETY: closing the master this test owns.
        unsafe { libc::close(sh.master) };
        reap(sh.pid);
    };
    for (what, got) in [
        ("shell_args", spawn(Some(&args[..]), None, None)),
        ("argv_override", spawn(None, Some(&over[..]), None)),
        ("login shell", spawn(None, None, None)),
    ] {
        match got {
            Ok(sh) => {
                end(sh);
                panic!("{what}: a shell holding a NUL ran another program");
            }
            Err(e) => {
                assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{what}: {e}");
                let said = e.to_string();
                assert!(
                    said.contains("NUL") && said.contains("the shell (set by --shell"),
                    "{what}: the refusal names the shell and where it was set: {said}"
                );
            }
        }
    }
    let exec = list(&["/bin/sh", "-c", "exit 0"]);
    end(spawn(None, None, Some(&exec[..]))
        .expect("an -e command does not run the shell, so it is not refused for it"));
}
