// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::{
    num::NonZeroU32,
    os::fd::AsRawFd as _,
    time::{Duration, Instant},
};

fn queued(epoch: &Epoch) -> libc::c_int {
    let mut count: libc::c_int = -1;
    // SAFETY: observes one actual held original FIFO and writes only the
    // caller-owned integer. It consumes no byte and changes no OFD flags.
    // Darwin dispatches a read/write FIFO ioctl to both socket ends: its
    // write-side result can overwrite the read count. Observe the original
    // read-only witness, not the private read/write control description.
    assert_eq!(
        unsafe {
            libc::ioctl(
                epoch.transport.as_ref().unwrap().read.as_raw_fd(),
                libc::FIONREAD,
                &mut count,
            )
        },
        0
    );
    assert!(count >= 0);
    count
}

fn epoch(width: u32) -> Epoch {
    let mut epoch = Epoch::fresh().unwrap();
    epoch.open(NonZeroU32::new(width).unwrap()).unwrap();
    epoch
}

#[test]
fn native_fifo_failed_return_keeps_original_byte_attempt_unresolved() {
    let mut epoch = epoch(2);
    let mut loan = epoch.try_loan().unwrap().unwrap();
    let proof = epoch.close_unused(&loan).unwrap();
    let transport = epoch.transport.as_mut().unwrap();
    let started = Instant::now();
    loop {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "native FIFO did not reach its bounded kernel capacity"
        );
        match transport.control.write(&[b'+'; 4096]) {
            Ok(written) => assert!(written > 0),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            result => panic!("unexpected native FIFO fill: {result:?}"),
        }
    }
    // A large write may meet PIPE_BUF before the final byte is unavailable.
    loop {
        assert!(started.elapsed() < Duration::from_secs(60));
        match transport.control.write(b"+") {
            Ok(1) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            result => panic!("unexpected last-byte FIFO fill: {result:?}"),
        }
    }
    let (error, proof) = epoch
        .return_loan(&mut loan, proof)
        .unwrap_err()
        .into_parts();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert!(
        proof.is_none(),
        "a native write attempt cannot replay its one-use receipt"
    );
    assert!(matches!(epoch.loan_state(&loan).unwrap(), State::Returning));
    assert!(epoch.retire().is_err());
    assert!(epoch.try_loan().is_err());
}

#[test]
fn native_fifo_same_epoch_wrong_loan_keeps_both_original_receipts() {
    let mut epoch = epoch(3);
    assert_eq!(queued(&epoch), 2);
    let mut first = epoch.try_loan().unwrap().unwrap();
    assert_eq!(queued(&epoch), 1);
    let mut second = epoch.try_loan().unwrap().unwrap();
    assert_eq!(queued(&epoch), 0);
    let first_proof = epoch.close_unused(&first).unwrap();
    let second_proof = epoch.close_unused(&second).unwrap();
    let (_, first_proof) = epoch
        .return_loan(&mut second, first_proof)
        .unwrap_err()
        .into_parts();
    epoch.return_loan(&mut first, first_proof.unwrap()).unwrap();
    assert_eq!(queued(&epoch), 1);
    epoch.return_loan(&mut second, second_proof).unwrap();
    assert_eq!(queued(&epoch), 2);
    assert!(epoch.loan_state(&first).is_err());
    assert!(epoch.loan_state(&second).is_err());
    epoch.retire().unwrap();
}
