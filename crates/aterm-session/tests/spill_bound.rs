// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The non-parking egress bounds its spill.
//!
//! `SinkWriter::write_frame_nonparking` (the off-thread small-frame egress) normally
//! skips the `SPILL_CAP` backpressure the blocking
//! `write_frame` applies. Skipping the cap entirely let a machine-rate small-frame
//! producer (the cross-session `input`/`mouse` verbs use this egress) grow the
//! spill without bound while the foreground program stopped reading. An over-cap
//! non-parking frame therefore takes the blocking, cap-enforcing path. The UI
//! thread uses `write_frame_interactive_with_receipt` instead.
//!
//! A blocking pipe whose read end is never drained stands in for the wedged
//! foreground, a detached producer pumps small frames, and a counting global allocator
//! (this binary only) checks that retained heap stays near the 2 MiB `SPILL_CAP`. A
//! pipe, not a pty: a canonical-mode pty discards overflow input at the line
//! discipline, which would make the test vacuous. Unix-only: a ConPTY handle has no
//! spill path.
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

/// `SPILL_CAP` (2 MiB) bounds the spill to about that plus one frame, and `VecDeque`
/// doubling can take its allocation to ~2×; 8 MiB leaves margin over the ~2–4 MiB
/// steady state while catching unbounded growth (hundreds of MiB in under a second).
const HEAP_CEILING: i64 = 8 * 1024 * 1024;
/// The spill must actually run: without a wedge retained heap stays tiny and the
/// ceiling check passes vacuously, so the delta must first pass this.
const REPRODUCED_FLOOR: i64 = 256 * 1024;

/// Small enough to take the non-parking path (`<= NONPARK_MAX = 4096`); large enough
/// that a handful of appends reaches the cap quickly.
const FRAME: usize = 4096;

#[test]
fn nonparking_spill_is_bounded_against_a_wedged_pty() {
    // The write end is the sink's (blocking) master; the read end stays open but is
    // never read, so the kernel buffer fills and the spill runs as on a wedged
    // raw-mode pty.
    let mut fds: [libc::c_int; 2] = [0; 2];
    // SAFETY: `fds` is two writable c_int slots, as pipe(2) requires.
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe failed: {}", std::io::Error::last_os_error());

    let sink = Arc::new(SinkWriter::new(fds[1]));

    // Baseline after setup, so the delta is spill growth alone.
    let baseline = net();

    let stop = Arc::new(AtomicBool::new(false));
    let producer = {
        let sink = Arc::clone(&sink);
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let frame = vec![b'x'; FRAME];
            // A machine-rate small-frame flood. Bounded, it parks on the cap once the
            // pipe wedges; unbounded, it grows the spill forever. Never joined (the
            // process reaps it at exit); `stop` only ends the loop on the fast path.
            while !stop.load(Ordering::Relaxed) {
                if sink.write_frame_nonparking(&frame).is_err() {
                    break;
                }
            }
        })
    };

    // Two signals, checked on every sample so wall-clock never gates correctness (a
    // slow machine just fills the pipe more slowly):
    //   * regression: the delta crosses the ceiling (unbounded growth reaches hundreds
    //     of MiB in under a second), checked continuously so no early `break` hides it;
    //   * bounded: once reproduced (peak >= floor) the delta holds flat for
    //     STABLE_TARGET consecutive samples, which unbounded growth never does.
    // The absolute timeout tells "never wedged" (a broken setup) from either outcome.
    const STABLE_TARGET: u32 = 8; // ~160ms of no growth above the running peak
    let reproduce_deadline = Instant::now() + Duration::from_secs(60);
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
            "spill only reached {peak}B in 60s — the foreground never wedged, so the non-parking \
             spill path was not exercised and the ceiling check above passed vacuously; fix the \
             test setup"
        );
    }

    stop.store(true, Ordering::Relaxed);
    // Not joined: the producer is parked in the blocking cap wait and will not see
    // `stop` until the wedge clears; the process reaps it at exit.
    drop(producer);

    eprintln!(
        "SPILL-BOUND: peak retained delta = {peak}B held flat for {STABLE_TARGET} samples \
         (ceiling {HEAP_CEILING}B, floor {REPRODUCED_FLOOR}B, SPILL_CAP = 2 MiB)"
    );
}
