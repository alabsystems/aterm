// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::{Condvar, Mutex, PoisonError};

static IN_FLIGHT: Mutex<usize> = Mutex::new(0);
static LANDED: Condvar = Condvar::new();

/// One writer in flight, until it is dropped.
pub(crate) struct InFlight(());

impl InFlight {
    pub(crate) fn enter() -> Self {
        *IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        Self(())
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        let mut in_flight = IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner);
        *in_flight -= 1;
        drop(in_flight);
        LANDED.notify_all();
    }
}

/// Block until no detached ledger writer is in flight.
pub(crate) fn wait_until_landed() {
    let in_flight = IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner);
    let (_in_flight, waited) = LANDED
        .wait_timeout_while(in_flight, std::time::Duration::from_secs(60), |n| *n > 0)
        .unwrap_or_else(PoisonError::into_inner);
    assert!(
        !waited.timed_out(),
        "a detached update-ledger writer never finished"
    );
}
