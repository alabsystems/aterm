// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::Cell;

fn retry_refused(mend_refused: bool, asking_refused: bool) {
    let lanes = Lanes::new();
    let (mends, notices) = (Cell::new(0), Cell::new(0));
    let request = || {
        lanes.start(
            false,
            || {
                mends.set(mends.get() + 1);
                !mend_refused || mends.get() > 1
            },
            || {
                notices.set(notices.get() + 1);
                !asking_refused || notices.get() > 1
            },
        );
    };
    request();
    assert_eq!((mends.get(), notices.get()), (1, 1));
    request();
    let started = (1 + u32::from(mend_refused), 1 + u32::from(asking_refused));
    assert_eq!(
        (mends.get(), notices.get()),
        started,
        "a refused worker retries without duplicating one that started"
    );
    request();
    assert_eq!(
        (mends.get(), notices.get()),
        started,
        "both workers have now started, once each"
    );
}

#[test]
fn a_refused_mend_retries_without_restarting_join_notices() {
    retry_refused(true, false);
}

#[test]
fn refused_join_notices_retry_without_restarting_the_mend() {
    retry_refused(false, true);
}

#[test]
fn both_refused_workers_retry_then_stay_started() {
    retry_refused(true, true);
}

#[test]
fn a_request_during_a_start_coalesces_both_workers_and_keeps_a_refusal_retryable() {
    let lanes = Lanes::new();
    let (mends, notices) = (Cell::new(0), Cell::new(0));
    lanes.start(
        false,
        || {
            mends.set(mends.get() + 1);
            lanes.start(
                false,
                || panic!("the in-progress mend is already claimed"),
                || {
                    notices.set(notices.get() + 1);
                    true
                },
            );
            false
        },
        || panic!("the nested request already started join notices"),
    );
    assert_eq!((mends.get(), notices.get()), (1, 1));
    lanes.start(
        false,
        || {
            mends.set(mends.get() + 1);
            true
        },
        || panic!("the successful notices worker stays started"),
    );
    assert_eq!((mends.get(), notices.get()), (2, 1));
    lanes.start(
        false,
        || panic!("the successful mend stays started"),
        || panic!("the successful notices worker stays started"),
    );
}

/// The converge lane starts once, on its own start: the mend's start never starts it, an
/// inactive request leaves it available, and a refused start retries.
#[test]
fn the_converge_lane_starts_once_on_its_own_start() {
    let lanes = Lanes::new();
    let converges = Cell::new(0);
    let converge = || {
        converges.set(converges.get() + 1);
        converges.get() > 1
    };
    lanes.start(false, || true, || true);
    assert_eq!(
        converges.get(),
        0,
        "the mend's start starts no converge lane"
    );
    lanes.start_converge(true, || panic!("an inactive request starts none"));
    lanes.start_converge(false, converge);
    lanes.start_converge(false, converge);
    assert_eq!(converges.get(), 2, "a refused start retries");
    lanes.start_converge(false, || panic!("started once"));
    lanes.start(
        false,
        || panic!("the mend stays started"),
        || panic!("so do notices"),
    );
}

#[test]
fn a_headless_request_leaves_both_windowed_starts_available() {
    let lanes = Lanes::new();
    lanes.start(
        true,
        || panic!("headless starts no mend"),
        || panic!("headless starts no join notices"),
    );
    let (mends, notices) = (Cell::new(0), Cell::new(0));
    lanes.start(
        false,
        || {
            mends.set(mends.get() + 1);
            true
        },
        || {
            notices.set(notices.get() + 1);
            true
        },
    );
    assert_eq!((mends.get(), notices.get()), (1, 1));
    lanes.start(
        false,
        || panic!("the successful mend is not duplicated"),
        || panic!("the successful notices worker is not duplicated"),
    );
}
