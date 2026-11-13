// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::DeferredReaderGate;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

#[test]
fn prepared_reader_cannot_activate_before_infallible_release() {
    let gate = DeferredReaderGate::closed();
    let waiter = gate.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let waiter_stop = stop.clone();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let join = std::thread::spawn(move || {
        let _ = tx.send(waiter.wait_until_released(&waiter_stop));
    });
    while gate.parked() == 0 {
        aterm_uds::deadline::poll_yield();
    }
    assert!(
        rx.try_recv().is_err(),
        "closed gate must prevent pre-Commit activation"
    );
    gate.release();
    gate.release();
    assert_eq!(
        rx.recv_timeout(std::time::Duration::from_secs(60)),
        Ok(true),
        "release is idempotent and requires no resource acquisition"
    );
    join.join().unwrap();
}
