// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! SPILL-CAP regression gate — the non-parking egress must bound its spill.
//!
//! `SinkWriter::write_frame_nonparking` (the UI-thread keystroke egress) must NOT
//! park the calling thread, so it normally SKIPS the `SPILL_CAP` backpressure that
//! the blocking `write_frame` applies. The bug this test pins: skipping the cap
//! *entirely* let an automated machine-rate small-frame producer — the cross-session
//! `input`/`mouse` control verbs funnel through this same egress — grow the spill
//! `VecDeque<u8>` without bound whenever the foreground program wedged (stopped
//! reading its PTY input). The fix routes an over-cap non-parking frame to the
//! blocking, cap-enforcing path so the producer feels backpressure instead.
//!
//! This drives the exact mechanism end-to-end: a blocking fd whose read end is opened
//! and NEVER drained (a wedged foreground), and a detached producer pumping small
//! frames at machine rate. Under a counting global allocator we assert the retained
//! heap stays bounded (≈ the 2 MiB `SPILL_CAP`, with slack for VecDeque doubling)
//! rather than climbing with every frame. An integration test is its own crate, so
//! the global allocator here affects no other test. Unix-only (the spill / `poll(2)`
//! machinery does not exist for a Windows ConPTY handle).
//!
//! We wedge with a `pipe(2)`, not a pty: it engages the sink's spill path exactly —
//! `poll(POLLOUT)` goes false and `write(2)` would block once the kernel buffer fills
//! with no reader, which is precisely what a *raw-mode* wedged foreground pty does. A
//! *canonical*-mode pty instead DISCARDS overflow input at the line discipline, so it
//! never backpressures the master — a tty-layer detail orthogonal to the spill bound
//! under test, and one that would make this test flaky/vacuous.
//!
//! Run: `cargo test -p aterm-session --test spill_bound -- --nocapture`.
#![cfg(unix)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use aterm_session::sink::SinkWriter;

static NET: AtomicI64 = AtomicI64::new(0);

/// System allocator tracking net live bytes (alloc − dealloc) across this binary.
struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        NET.fetch_add(l.size() as i64, Ordering::Relaxed);
        // SAFETY: forwarding to System with the same layout.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        NET.fetch_sub(l.size() as i64, Ordering::Relaxed);
        // SAFETY: forwarding to System with the same ptr+layout.
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn net() -> i64 {
    NET.load(Ordering::Relaxed)
}

/// `SPILL_CAP` in sink.rs is 2 MiB. With the fix, the spill `VecDeque` is bounded to
/// ~that plus one frame; its backing allocation can be up to ~2× via doubling. 8 MiB
/// leaves generous margin over the ~2–4 MiB steady state while still catching the
/// unbounded case (which climbs into the hundreds of MiB in well under a second).
const HEAP_CEILING: i64 = 8 * 1024 * 1024;
/// The spill must actually be exercised: if we never wedge the pty, retained heap
/// stays tiny and the ceiling assert would pass vacuously. Require the buffer to have
/// grown past this, proving the non-parking spill path really ran.
const REPRODUCED_FLOOR: i64 = 256 * 1024;

/// Small enough to take the non-parking path (`<= NONPARK_MAX = 4096`); large enough
/// that a handful of appends reaches the cap quickly.
const FRAME: usize = 4096;

#[test]
fn nonparking_spill_is_bounded_against_a_wedged_pty() {
    // A blocking pipe stands in for the wedged foreground: the write end is the sink's
    // "master" (blocking, matching the sink's contract); the read end is kept open but
    // NEVER read, so the kernel buffer fills and further writes block — driving the
    // spill exactly as a wedged raw-mode pty would.
    let mut fds: [libc::c_int; 2] = [0; 2];
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe failed: {}", std::io::Error::last_os_error());
    let read_end = fds[0];
    let write_end = fds[1];
    // Keep the read end alive (undrained) for the whole test so the write end wedges.
    let _read_end_kept_open = read_end;

    let sink = Arc::new(SinkWriter::new(write_end));

    // Baseline AFTER setup so the delta is attributable to spill growth alone.
    let baseline = net();

    let stop = Arc::new(AtomicBool::new(false));
    let producer = {
        let sink = Arc::clone(&sink);
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let frame = vec![b'x'; FRAME];
            // Machine-rate small-frame flood, like an automated control-verb driver.
            // With the fix, this blocks on the cap once the pty wedges; without it,
            // it spins forever growing the spill. Either way we never join it — the
            // process reclaims it at exit — but honour `stop` for a clean shutdown
            // on the (blocking) fast path.
            while !stop.load(Ordering::Relaxed) {
                if sink.write_frame_nonparking(&frame).is_err() {
                    break;
                }
            }
        })
    };

    // Sample retained heap. Two independent signals, checked on every sample so the
    // wall-clock window never gates correctness (a slow/emulated CI just takes longer
    // to fill the pipe one byte per syscall):
    //   * REGRESSION (fail fast): delta crosses the ceiling. Without the cap the spill
    //     climbs into the hundreds of MiB in well under a second, so this trips almost
    //     immediately — and it is checked continuously, so a leak can never slip past
    //     an early `break`.
    //   * BOUNDED (pass): once the spill has reproduced (peak >= floor) it stops
    //     growing and holds flat for STABLE_TARGET consecutive samples. Unbounded
    //     growth never stabilises, so it can only exit via the ceiling assert.
    // A generous absolute timeout distinguishes "never wedged" (a broken test setup)
    // from either real outcome.
    const STABLE_TARGET: u32 = 8; // ~160ms of no growth above the running peak
    let reproduce_deadline = Instant::now() + Duration::from_secs(10);
    let mut peak: i64 = 0;
    let mut stable: u32 = 0;
    loop {
        thread::sleep(Duration::from_millis(20));
        let delta = net() - baseline;
        assert!(
            delta <= HEAP_CEILING,
            "non-parking spill retained {delta}B (> {HEAP_CEILING}B ceiling) — SPILL_CAP is \
             bypassed on the wait_for_room=false path, so a wedged foreground grows the spill \
             without bound (regression of the sink.rs non-parking cap)"
        );
        if delta >= REPRODUCED_FLOOR {
            // Reproduced. Count consecutive samples with no growth above the peak;
            // the bounded spill plateaus, an unbounded one keeps setting new peaks.
            if delta <= peak {
                stable += 1;
                if stable >= STABLE_TARGET {
                    break;
                }
            } else {
                stable = 0;
            }
        }
        peak = peak.max(delta);
        assert!(
            Instant::now() < reproduce_deadline,
            "spill only reached {peak}B in 10s — the foreground never wedged, so the non-parking \
             spill path was not exercised and the ceiling check above passed vacuously; fix the \
             test setup"
        );
    }

    stop.store(true, Ordering::Relaxed);
    // Do NOT join: with the fix the producer is parked in the blocking cap wait and
    // will not observe `stop` until the wedge clears; the process reaps it at exit.
    drop(producer);

    eprintln!(
        "SPILL-BOUND: peak retained delta = {peak}B held flat for {STABLE_TARGET} samples \
         (ceiling {HEAP_CEILING}B, floor {REPRODUCED_FLOOR}B, SPILL_CAP = 2 MiB)"
    );
}
