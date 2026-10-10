// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The non-parking egress must bound its spill.
//!
//! `SinkWriter::write_frame_nonparking` (the UI-thread keystroke egress) must not
//! park, so it normally skips the `SPILL_CAP` backpressure the blocking
//! `write_frame` applies. Skipping the cap entirely let a machine-rate small-frame
//! producer (the cross-session `input`/`mouse` verbs use this egress) grow the
//! spill without bound while the foreground program stopped reading. An over-cap
//! non-parking frame therefore takes the blocking, cap-enforcing path.
//!
//! A blocking pipe whose read end is never drained stands in for the wedged
//! foreground; a detached producer pumps small frames; a counting global
//! allocator (this binary only) checks that retained heap stays near the 2 MiB
//! `SPILL_CAP`. A pipe, not a pty: a raw-mode wedged pty backpressures the same
//! way, but a canonical-mode pty discards overflow input at the line discipline
//! and would make the test vacuous. Unix-only: a ConPTY handle has no spill path.
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
    // SAFETY: `fds` is two writable c_int slots, as pipe(2) requires.
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe failed: {}", std::io::Error::last_os_error());

    let sink = Arc::new(SinkWriter::new(fds[1]));

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
    // wall-clock window never gates correctness (a slow machine just takes longer
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
    // Do NOT join: with the fix the producer is parked in the blocking cap wait and
    // will not observe `stop` until the wedge clears; the process reaps it at exit.
    drop(producer);

    eprintln!(
        "SPILL-BOUND: peak retained delta = {peak}B held flat for {STABLE_TARGET} samples \
         (ceiling {HEAP_CEILING}B, floor {REPRODUCED_FLOOR}B, SPILL_CAP = 2 MiB)"
    );
}
