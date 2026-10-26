// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Startup of the vault subscriber's mend and join-notice workers.

use std::sync::atomic::{AtomicBool, Ordering};

/// The process's independent once-only starts of its two vault workers.
pub(crate) struct Lanes {
    mending: AtomicBool,
    asking: AtomicBool,
}

impl Lanes {
    pub(crate) const fn new() -> Self {
        Self {
            mending: AtomicBool::new(false),
            asking: AtomicBool::new(false),
        }
    }

    /// An active request starts each worker once its spawner accepts it. A headless or
    /// unsupported-platform request (`inactive`) leaves both starts available.
    pub(crate) fn start(
        &self,
        inactive: bool,
        mend: impl FnOnce() -> bool,
        asking: impl FnOnce() -> bool,
    ) {
        if !inactive {
            Self::start_one(&self.mending, mend);
            Self::start_one(&self.asking, asking);
        }
    }

    /// Claim before calling the spawner so overlapping requests coalesce. A refused start
    /// remains available; a worker that started and later ends keeps its once-per-process bound.
    fn start_one(started: &AtomicBool, spawn: impl FnOnce() -> bool) {
        if started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            && !spawn()
        {
            started.store(false, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
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
}
