// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `write_frame_nonparking` (`Room::NoWait`) spill bound against a wedged peer.
#![cfg(unix)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use aterm_session::sink::SinkWriter;

static NET: AtomicI64 = AtomicI64::new(0);

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

const HEAP_CEILING: i64 = 8 * 1024 * 1024;
const REPRODUCED_FLOOR: i64 = 256 * 1024;
const FRAME: usize = 4096;

#[test]
fn nonparking_spill_is_bounded_against_a_wedged_pty() {
    let mut fds: [libc::c_int; 2] = [0; 2];
    // SAFETY: `fds` is two writable c_int slots, as pipe(2) requires.
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe failed: {}", std::io::Error::last_os_error());

    let sink = Arc::new(SinkWriter::new(fds[1]));
    let baseline = net();

    let stop = Arc::new(AtomicBool::new(false));
    let producer = {
        let sink = Arc::clone(&sink);
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let frame = vec![b'x'; FRAME];
            while !stop.load(Ordering::Relaxed) {
                if sink.write_frame_nonparking(&frame).is_err() {
                    break;
                }
            }
        })
    };

    const STABLE_TARGET: u32 = 8; // ~160ms of no growth above the running peak
    let reproduce_deadline = Instant::now() + Duration::from_secs(60);
    let mut peak: i64 = 0;
    let mut stable: u32 = 0;
    loop {
        thread::sleep(Duration::from_millis(20));
        let delta = net() - baseline;
        assert!(
            delta <= HEAP_CEILING,
            "write_frame_nonparking retained {delta}B (> {HEAP_CEILING}B ceiling) — SPILL_CAP is \
             bypassed on the Room::NoWait path, so a wedged peer grows the spill without bound"
        );
        if delta >= REPRODUCED_FLOOR {
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
    drop(producer);

    eprintln!(
        "SPILL-BOUND: peak retained delta = {peak}B held flat for {STABLE_TARGET} samples \
         (ceiling {HEAP_CEILING}B, floor {REPRODUCED_FLOOR}B, SPILL_CAP = 2 MiB)"
    );
}
