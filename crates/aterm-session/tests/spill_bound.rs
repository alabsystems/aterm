// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `write_frame_nonparking` (`Room::NoWait`) spill bound against a wedged peer.
#![cfg(unix)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, Ordering};
use std::sync::{Arc, mpsc};
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

    const DRAIN_PARKED: u8 = 1;
    const FRAME_LOCK_AT_CAP: u8 = 2;
    let state = Arc::new(AtomicU8::new(0));
    let (parked_tx, parked_rx) = mpsc::sync_channel(1);
    {
        let state = Arc::clone(&state);
        sink.seam_for_testing(move |seam| match seam {
            Seam::DrainPark => {
                if state.fetch_or(DRAIN_PARKED, Ordering::AcqRel) & FRAME_LOCK_AT_CAP != 0 {
                    let _ = parked_tx.try_send(());
                }
            }
            Seam::FrameLock => {
                if state.fetch_or(FRAME_LOCK_AT_CAP, Ordering::AcqRel) & DRAIN_PARKED != 0 {
                    let _ = parked_tx.try_send(());
                }
            }
            Seam::SpillCapPark => {
                let _ = parked_tx.try_send(());
            }
            _ => {}
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

    let spilled = sink.spilled_bytes();
    let bound = SinkWriter::SPILL_CAP + FRAME + 12;
    assert!(
        spilled >= SinkWriter::SPILL_CAP && spilled <= bound,
        "spill length {spilled}B must sit in [SPILL_CAP={}, {bound}]",
        SinkWriter::SPILL_CAP,
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
