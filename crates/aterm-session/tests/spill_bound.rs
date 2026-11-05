// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `write_frame_nonparking` (`Room::NoWait`) spill bound against a wedged peer.
#![cfg(unix)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak, mpsc};
use std::thread;
use std::time::Duration;

use aterm_session::sink::{Seam, SinkWriter};

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
const FRAME: usize = 4096;

/// NEGATIVE CONTROL: `write_frame_nonparking` delegates to blocking backpressure once
/// `spilled >= SPILL_CAP`, bounding `|Spill|` by `SPILL_CAP + FRAME + 12` against a wedged peer.
///
/// `|Spill|` is read where the producer parks at the cap (`Seam::SpillCapPark`, under the spill's
/// lock, where the count is the buffer's length), never after: a macOS pipe is no fixed wedge, and
/// once the kernel grows its buffer the drainer can complete a chunk between the park and a later
/// read (measured: 508 frames and 511 bytes, two 8 KiB chunks under the cap).
#[test]
fn nonparking_spill_is_bounded_against_a_wedged_pty() {
    let mut fds: [libc::c_int; 2] = [-1; 2];
    // SAFETY: `fds` is two writable c_int slots, as pipe(2) requires.
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe failed: {}", std::io::Error::last_os_error());
    // SAFETY: `pipe` succeeded and returned two distinct owned descriptors.
    let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    // SAFETY: distinct write descriptor from `pipe`.
    let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };

    let sink = Arc::new(SinkWriter::new(write.as_raw_fd()));
    let baseline = net();

    // The producer's thread alone reads through `weak` (it is joined before `sink` drops).
    let peak = Arc::new(AtomicUsize::new(0));
    let (parked_tx, parked_rx) = mpsc::sync_channel(1);
    {
        let weak: Weak<SinkWriter> = Arc::downgrade(&sink);
        let peak = Arc::clone(&peak);
        sink.seam_for_testing(move |seam| {
            if seam == Seam::SpillCapPark
                && let Some(sink) = weak.upgrade()
            {
                peak.fetch_max(sink.spilled_bytes(), Ordering::AcqRel);
                let _ = parked_tx.try_send(());
            }
        });
    }

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

    parked_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("non-parking producer must park once spill reaches SPILL_CAP");

    let bound = SinkWriter::SPILL_CAP + FRAME + 12;
    let parked_at = peak.load(Ordering::Acquire);
    assert!(
        parked_at > SinkWriter::SPILL_CAP && parked_at <= bound,
        "the producer parked at a spill of {parked_at}B, which must sit in (SPILL_CAP={}, {bound}]",
        SinkWriter::SPILL_CAP,
    );
    let spilled = sink.spilled_bytes();
    assert!(
        spilled <= bound,
        "spill length {spilled}B must stay at most {bound}B"
    );
    let delta = net() - baseline;
    assert!(
        delta <= HEAP_CEILING,
        "write_frame_nonparking retained {delta}B (> {HEAP_CEILING}B ceiling)"
    );

    stop.store(true, Ordering::Relaxed);
    drop(read);
    sink.sever_input();
    producer.join().expect("producer exits cleanly after sever");
    let _ = sink.wait_egress_drained_to_kernel();
    drop(sink);
    drop(write);
}
