// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Metal command-buffer status classification and sticky device-loss latch.

use std::sync::OnceLock;

use super::ffi::{AutoreleasePool, Id, Sel, checked_msg, sel};

/// `MTLCommandBufferStatus` (`MTLCommandBuffer.h:56-63`).
pub(crate) const STATUS_NOT_ENQUEUED: usize = 0;
#[cfg(test)]
pub(crate) const STATUS_ENQUEUED: usize = 1;
#[cfg(test)]
pub(crate) const STATUS_COMMITTED: usize = 2;
pub(crate) const STATUS_SCHEDULED: usize = 3;
pub(crate) const STATUS_COMPLETED: usize = 4;
pub(crate) const STATUS_ERROR: usize = 5;

/// Header "no error" code (`MTLCommandBufferErrorNone`).
pub(crate) const ERROR_NONE: isize = 0;
pub(crate) const ERROR_INTERNAL: isize = 1;
pub(crate) const ERROR_TIMEOUT: isize = 2;
pub(crate) const ERROR_PAGE_FAULT: isize = 3;
pub(crate) const ERROR_ACCESS_REVOKED: isize = 4;
pub(crate) const ERROR_NOT_PERMITTED: isize = 7;
pub(crate) const ERROR_OUT_OF_MEMORY: isize = 8;
pub(crate) const ERROR_INVALID_RESOURCE: isize = 9;
pub(crate) const ERROR_MEMORYLESS: isize = 10;
pub(crate) const ERROR_DEVICE_REMOVED: isize = 11;
pub(crate) const ERROR_STACK_OVERFLOW: isize = 12;

/// What one finished command buffer means for the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CbOutcome {
    /// `MTLCommandBufferStatusCompleted`.
    Completed,
    /// Command buffer has not finished executing (statuses `0..=3`).
    Unfinished { status: usize },
    /// Transient submission failure that can be retried without latching device loss.
    Retryable { code: isize, name: &'static str },
    /// Permanent device or access failure that latches device loss.
    Lost {
        code: Option<isize>,
        name: &'static str,
    },
}

/// Classifies a command buffer's `(status, error_code)` pair into a [`CbOutcome`].
#[inline]
pub(crate) const fn classify(status: usize, error_code: Option<isize>) -> CbOutcome {
    match status {
        STATUS_COMPLETED => CbOutcome::Completed,
        STATUS_NOT_ENQUEUED..=STATUS_SCHEDULED => CbOutcome::Unfinished { status },
        STATUS_ERROR => match error_code {
            Some(ERROR_TIMEOUT) => CbOutcome::Retryable {
                code: ERROR_TIMEOUT,
                name: "MTLCommandBufferErrorTimeout",
            },
            Some(ERROR_OUT_OF_MEMORY) => CbOutcome::Retryable {
                code: ERROR_OUT_OF_MEMORY,
                name: "MTLCommandBufferErrorOutOfMemory",
            },
            Some(code) => CbOutcome::Lost {
                code: Some(code),
                name: match code {
                    ERROR_INTERNAL => "MTLCommandBufferErrorInternal",
                    ERROR_PAGE_FAULT => "MTLCommandBufferErrorPageFault",
                    ERROR_ACCESS_REVOKED => "MTLCommandBufferErrorAccessRevoked",
                    ERROR_NOT_PERMITTED => "MTLCommandBufferErrorNotPermitted",
                    ERROR_INVALID_RESOURCE => "MTLCommandBufferErrorInvalidResource",
                    ERROR_MEMORYLESS => "MTLCommandBufferErrorMemoryless",
                    ERROR_DEVICE_REMOVED => "MTLCommandBufferErrorDeviceRemoved",
                    ERROR_STACK_OVERFLOW => "MTLCommandBufferErrorStackOverflow",
                    ERROR_NONE => "MTLCommandBufferErrorNone (contradictory with status Error)",
                    _ => "unrecognized MTLCommandBufferError code",
                },
            },
            None => CbOutcome::Lost {
                code: None,
                name: "MTLCommandBufferStatusError with nil error",
            },
        },
        _ => CbOutcome::Lost {
            code: None,
            name: "status outside the MTLCommandBufferStatus enum",
        },
    }
}

/// Reads `-[MTLCommandBuffer status]`.
#[inline]
pub(crate) fn command_buffer_status(cb: Id) -> usize {
    // SAFETY: `-status` is an `NSUInteger` getter on a live `MTLCommandBuffer`.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
        f(cb, sel(c"status"))
    }
}

/// `-[cb error].code`, or `None` when `cb`, a live `MTLCommandBuffer`, has no error.
pub(crate) fn command_buffer_error_code(cb: Id) -> Option<isize> {
    let _pool = AutoreleasePool::new();
    // SAFETY: `-error` returns a borrowed `NSError` (or nil) owned by the pool;
    // `-code` is read while the pool lives.
    unsafe {
        let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
        let err = get(cb, sel(c"error"));
        if err.is_null() {
            return None;
        }
        let code: unsafe extern "C-unwind" fn(Id, Sel) -> isize = checked_msg();
        Some(code(err, sel(c"code")))
    }
}

/// Reads `cb`'s status and error code and classifies the outcome.
#[inline]
pub(crate) fn outcome_of(cb: Id) -> CbOutcome {
    let status = command_buffer_status(cb);
    let code = if status == STATUS_ERROR {
        command_buffer_error_code(cb)
    } else {
        None
    };
    classify(status, code)
}

/// Sticky device-loss latch fed by completed Metal command-buffer outcomes.
#[derive(Debug, Default)]
pub(crate) struct LossLatch {
    /// Debug-only feed counter ([`Self::record`]).
    #[cfg(debug_assertions)]
    fed: std::sync::atomic::AtomicUsize,
    /// Set exactly once, by the first `Lost` outcome recorded.
    lost: OnceLock<String>,
    /// Whether an `EncodeSession` holds this latch ([`crate::metal::encoder::EncodeSession::new`]).
    encode_bound: std::sync::atomic::AtomicBool,
}

impl LossLatch {
    #[inline]
    pub(crate) const fn new() -> Self {
        Self {
            #[cfg(debug_assertions)]
            fed: std::sync::atomic::AtomicUsize::new(0),
            lost: OnceLock::new(),
            encode_bound: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Claims this latch for an `EncodeSession`, returning `false` if already bound.
    #[inline]
    pub(crate) fn try_bind_encode_session(&self) -> bool {
        self.encode_bound
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
    }

    /// Releases the `EncodeSession` claim on drop.
    #[inline]
    pub(crate) fn unbind_encode_session(&self) {
        self.encode_bound
            .store(false, std::sync::atomic::Ordering::Release);
    }

    /// Records a command-buffer `outcome`, latching on the first [`CbOutcome::Lost`].
    #[inline]
    pub(crate) fn record(&self, outcome: &CbOutcome) {
        #[cfg(debug_assertions)]
        self.fed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let CbOutcome::Lost { .. } = outcome {
            let _ = self.lost.set(format!("{outcome:?}"));
        }
    }

    /// Debug-only: outcomes fed through [`Self::record`].
    #[cfg(debug_assertions)]
    #[cfg(test)]
    pub(crate) fn fed_count(&self) -> usize {
        self.fed.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Sticky: `true` from the first recorded `Lost` onward.
    #[inline]
    pub(crate) fn is_lost(&self) -> bool {
        self.lost.get().is_some()
    }

    /// The first loss's classification, for the downgrade path's diagnostics.
    #[inline]
    pub(crate) fn reason(&self) -> Option<&str> {
        self.lost.get().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_code_classifies_as_declared() {
        assert_eq!(classify(STATUS_COMPLETED, None), CbOutcome::Completed);
        assert_eq!(
            classify(STATUS_COMPLETED, Some(ERROR_PAGE_FAULT)),
            CbOutcome::Completed
        );

        for status in [
            STATUS_NOT_ENQUEUED,
            STATUS_ENQUEUED,
            STATUS_COMMITTED,
            STATUS_SCHEDULED,
        ] {
            assert_eq!(classify(status, None), CbOutcome::Unfinished { status });
        }

        for (code, name) in [
            (ERROR_TIMEOUT, "MTLCommandBufferErrorTimeout"),
            (ERROR_OUT_OF_MEMORY, "MTLCommandBufferErrorOutOfMemory"),
        ] {
            assert_eq!(
                classify(STATUS_ERROR, Some(code)),
                CbOutcome::Retryable { code, name },
                "code {code} must be RETRYABLE"
            );
        }

        for (code, name) in [
            (ERROR_INTERNAL, "MTLCommandBufferErrorInternal"),
            (ERROR_PAGE_FAULT, "MTLCommandBufferErrorPageFault"),
            (ERROR_ACCESS_REVOKED, "MTLCommandBufferErrorAccessRevoked"),
            (ERROR_NOT_PERMITTED, "MTLCommandBufferErrorNotPermitted"),
            (
                ERROR_INVALID_RESOURCE,
                "MTLCommandBufferErrorInvalidResource",
            ),
            (ERROR_MEMORYLESS, "MTLCommandBufferErrorMemoryless"),
            (ERROR_DEVICE_REMOVED, "MTLCommandBufferErrorDeviceRemoved"),
            (ERROR_STACK_OVERFLOW, "MTLCommandBufferErrorStackOverflow"),
        ] {
            assert_eq!(
                classify(STATUS_ERROR, Some(code)),
                CbOutcome::Lost {
                    code: Some(code),
                    name
                },
                "code {code} must be LOST"
            );
        }

        assert!(matches!(
            classify(STATUS_ERROR, Some(999)),
            CbOutcome::Lost {
                code: Some(999),
                ..
            }
        ));
        assert!(matches!(
            classify(STATUS_ERROR, None),
            CbOutcome::Lost { code: None, .. }
        ));
        assert!(matches!(
            classify(6, None),
            CbOutcome::Lost { code: None, .. }
        ));
    }

    #[test]
    fn the_latch_flips_only_on_lost_and_stays_flipped() {
        let latch = LossLatch::new();
        assert!(!latch.is_lost(), "a fresh latch is not lost");

        latch.record(&classify(STATUS_COMPLETED, None));
        latch.record(&classify(STATUS_SCHEDULED, None));
        latch.record(&classify(STATUS_ERROR, Some(ERROR_TIMEOUT)));
        latch.record(&classify(STATUS_ERROR, Some(ERROR_OUT_OF_MEMORY)));
        assert!(
            !latch.is_lost(),
            "Completed/Unfinished/Retryable must never latch"
        );
        assert_eq!(latch.reason(), None);

        latch.record(&classify(STATUS_ERROR, Some(ERROR_PAGE_FAULT)));
        assert!(latch.is_lost(), "the first Lost must latch");
        let first = latch
            .reason()
            .expect("a latched latch has a reason")
            .to_owned();
        assert!(!first.is_empty(), "a latched reason must not be empty");

        latch.record(&classify(STATUS_COMPLETED, None));
        assert!(latch.is_lost(), "Completed after a loss must not unlatch");
        latch.record(&classify(STATUS_ERROR, Some(ERROR_DEVICE_REMOVED)));
        assert!(latch.is_lost());
        assert_eq!(
            latch.reason(),
            Some(first.as_str()),
            "the FIRST loss's reason survives later ones"
        );
    }
}
