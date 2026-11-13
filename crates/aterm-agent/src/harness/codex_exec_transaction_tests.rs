// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn exec_transaction_reconciles_lost_ack_only_for_the_bound_images() {
    let mut tx = Transaction::new(10);
    assert!(!tx.load(11));
    assert!(!tx.consume(11));
    assert!(!tx.execute(11));
    assert!(!tx.execute(10));
    assert!(!tx.reserve(11));
    assert!(tx.reserve(10));
    assert!(tx.reserve(10));
    assert!(tx.execute(10));
    assert!(tx.execute(10));
    assert!(!tx.load(10));
    assert!(tx.load(11));
    assert!(tx.load(11));
    assert!(!tx.load(12));
    assert!(!tx.execute(10));
    assert!(!tx.consume(12));
    assert!(tx.consume(11));
    assert!(tx.consume(11));
    assert!(!tx.consume(12));
    assert!(!tx.load(12));
    assert!(!tx.cancel());
}

#[test]
fn exec_transaction_cancellation_prevents_every_later_admission() {
    for stage in 0..4 {
        let mut tx = Transaction::new(10);
        if stage > 0 {
            assert!(tx.reserve(10));
        }
        if stage > 1 {
            assert!(tx.execute(10));
        }
        if stage > 2 {
            assert!(tx.load(11));
        }
        assert!(tx.cancel());
        assert!(tx.cancel());
        assert!(!tx.execute(10));
        assert!(!tx.reserve(10));
        assert!(!tx.load(11));
        assert!(!tx.consume(11));
        assert_eq!(tx.stage(), Stage::Cancelled);
    }
}
