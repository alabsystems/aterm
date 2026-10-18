// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The device-loss latch, standing in for the device-lost callback Metal does
//! not have.
//!
//! The wgpu path handles loss through `Device::set_device_lost_callback`,
//! which sets the flag behind `GpuContext::device_lost`, and `aterm-gui`
//! downgrades to the CPU renderer off it. Metal's signals are per command
//! buffer instead: `-[MTLCommandBuffer status]` reaching
//! `MTLCommandBufferStatusError` and the `NSError`'s `MTLCommandBufferError`
//! code (eGPU removal also surfaces as `DeviceRemoved`). Every submission
//! (offscreen pass, compute dispatch, present) carries its own status, so this
//! is its own module; the swapchain consumes it.
//!
//! # Classification
//!
//! Codes and statuses from `Metal.framework/Headers/MTLCommandBuffer.h`:
//!
//! * RETRYABLE (the device and encoded state are fine): `Timeout` (2) and
//!   `OutOfMemory` (8).
//! * LOST: `Internal` (1), `PageFault` (3), `AccessRevoked` (4),
//!   `NotPermitted` (7), `InvalidResource` (9), `Memoryless` (10),
//!   `DeviceRemoved` (11), `StackOverflow` (12), and, failing closed, any
//!   unknown code, an `Error` status with a nil `NSError`, and any status
//!   outside the enum, so an unknown failure latches instead of retrying
//!   against a faulting device.
//!
//! # Testing limits
//!
//! A real `MTLCommandBufferStatusError` cannot be produced on a healthy GPU
//! without wrecking GPU state. The error arms are exercised through
//! [`classify`] (the pure classification that [`outcome_of`] feeds), and the
//! real FFI path is pinned on the arm a healthy device produces: a committed,
//! waited command buffer reading `Completed`. eGPU removal cannot occur on
//! Apple silicon; its code is covered by the injection seam.

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

// `MTLCommandBufferError` codes (`MTLCommandBuffer.h`). `Blacklisted` is the
// deprecated spelling of `AccessRevoked` (4).
/// The header's "no error" code. An `Error` status carrying code 0 is
/// contradictory and stays LOST (fail closed), but is named.
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
    /// `MTLCommandBufferStatusCompleted` — the work ran.
    Completed,
    /// Status 0..=3: the command buffer has not finished. A call-site
    /// programming error, not a device signal; it neither retries nor latches.
    Unfinished { status: usize },
    /// The submission failed without impeaching the device — resubmit or
    /// rebuild the frame.
    Retryable { code: isize, name: &'static str },
    /// The device (or this process's access to it) is gone, or resubmission
    /// would fault again. Latch and downgrade.
    Lost {
        code: Option<isize>,
        name: &'static str,
    },
}

/// Classify one command buffer's `(status, error code)` pair.
///
/// The injection seam: every header code can be fed through here without
/// wrecking a GPU, and [`outcome_of`] is two property reads feeding it.
///
/// `error_code` is `-[NSError code]` when status is `Error` and the error is
/// non-nil, else `None`. The error is documented to be in
/// `MTLCommandBufferErrorDomain`, so the code is read against that enum
/// without comparing the domain string.
pub(crate) const fn classify(status: usize, error_code: Option<isize>) -> CbOutcome {
    match status {
        STATUS_COMPLETED => CbOutcome::Completed,
        STATUS_NOT_ENQUEUED..=STATUS_SCHEDULED => CbOutcome::Unfinished { status },
        STATUS_ERROR => match error_code {
            // No local retry budget: the OS escalates repeated timeouts and
            // hangs to AccessRevoked, which latches.
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
                    // Fail closed: a code this module has never heard of must
                    // not spin a retry loop against a faulting device.
                    _ => "unrecognized MTLCommandBufferError code",
                },
            },
            // Status says Error but no NSError was attached. Fail closed.
            None => CbOutcome::Lost {
                code: None,
                name: "MTLCommandBufferStatusError with nil error",
            },
        },
        // A status outside the header's enum means the FFI read garbage;
        // trusting it with the frame loop would be worse than latching.
        _ => CbOutcome::Lost {
            code: None,
            name: "status outside the MTLCommandBufferStatus enum",
        },
    }
}

/// `-[MTLCommandBuffer status]`, raw. Safe: an `NSUInteger` property readable
/// from any thread at any time.
pub(crate) fn command_buffer_status(cb: Id) -> usize {
    // SAFETY: `-status` is an `NSUInteger` getter on a live `MTLCommandBuffer`.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
        f(cb, sel(c"status"))
    }
}

/// `-[MTLCommandBuffer error]`'s code, or `None` when nil. The pool is pushed
/// before the read because the getter can autorelease the `NSError`; the code
/// is copied out before the pool pops.
fn command_buffer_error_code(cb: Id) -> Option<isize> {
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

/// Read a command buffer's `(status, error code)` and [`classify`] it.
/// Meaningful once finished; earlier it reports [`CbOutcome::Unfinished`].
pub(crate) fn outcome_of(cb: Id) -> CbOutcome {
    let status = command_buffer_status(cb);
    let code = if status == STATUS_ERROR {
        command_buffer_error_code(cb)
    } else {
        None
    };
    classify(status, code)
}

/// The sticky latch: once any submission classifies [`CbOutcome::Lost`], the
/// device is lost for good.
///
/// The Metal twin of the flag behind `GpuContext::device_lost`, except that it
/// records why (the first loss's classification) and is fed [`CbOutcome`]s by
/// callers rather than a driver callback. `Send + Sync` by construction.
#[derive(Debug, Default)]
pub(crate) struct LossLatch {
    /// Debug-only feed counter — see [`Self::record`] for why it exists.
    #[cfg(debug_assertions)]
    fed: std::sync::atomic::AtomicUsize,
    /// Set exactly once, by the FIRST `Lost` outcome recorded.
    lost: OnceLock<String>,
    /// Whether an `EncodeSession` holds this latch, so one latch means one
    /// queue (which `Frame::present`'s `Arc::ptr_eq` check relies on). Bound in
    /// [`crate::metal::encoder::EncodeSession::new`], released on its drop.
    encode_bound: std::sync::atomic::AtomicBool,
}

impl LossLatch {
    pub(crate) const fn new() -> Self {
        Self {
            #[cfg(debug_assertions)]
            fed: std::sync::atomic::AtomicUsize::new(0),
            lost: OnceLock::new(),
            encode_bound: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Claim this latch for the encode session. `false` means one already holds
    /// it and the caller must refuse to construct.
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

    /// Release the encode-session claim — called from `EncodeSession::drop`
    /// only, so a torn-down session (device-loss rebuild) frees the slot.
    pub(crate) fn unbind_encode_session(&self) {
        self.encode_bound
            .store(false, std::sync::atomic::Ordering::Release);
    }

    /// Feed one finished command buffer's outcome through the latch.
    /// `Completed`, `Unfinished` and `Retryable` never move it; the first
    /// `Lost` sets it forever.
    pub(crate) fn record(&self, outcome: &CbOutcome) {
        // Debug builds count every feed, including `Completed`, so tests can
        // check the latch is fed even when nothing is lost.
        #[cfg(debug_assertions)]
        self.fed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let CbOutcome::Lost { .. } = outcome {
            // A second loss keeps the FIRST reason — that is the one that
            // took the device down, and `OnceLock::set` enforces it.
            let _ = self.lost.set(format!("{outcome:?}"));
        }
    }

    /// Debug-only: outcomes fed through [`Self::record`], so a test can check
    /// `wait_outcome` feeds the latch on `Completed` too.
    #[cfg(debug_assertions)]
    #[cfg(test)]
    pub(crate) fn fed_count(&self) -> usize {
        self.fed.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Sticky: `true` from the first recorded `Lost` onward.
    pub(crate) fn is_lost(&self) -> bool {
        self.lost.get().is_some()
    }

    /// The first loss's classification, for the downgrade path's diagnostics.
    pub(crate) fn reason(&self) -> Option<&str> {
        self.lost.get().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every header code plus the fail-closed arms, classified, with the
    /// retry/latch line drawn explicitly.
    #[test]
    fn every_error_code_classifies_as_declared() {
        // The one healthy terminal state.
        assert_eq!(classify(STATUS_COMPLETED, None), CbOutcome::Completed);
        // A code alongside a non-Error status is ignored, not trusted: the
        // pair cannot legally exist, and status is the primary signal.
        assert_eq!(
            classify(STATUS_COMPLETED, Some(ERROR_PAGE_FAULT)),
            CbOutcome::Completed
        );

        // Pre-completion statuses are the caller's bug, never a device signal.
        for status in [
            STATUS_NOT_ENQUEUED,
            STATUS_ENQUEUED,
            STATUS_COMMITTED,
            STATUS_SCHEDULED,
        ] {
            assert_eq!(classify(status, None), CbOutcome::Unfinished { status });
        }

        // The two transient failures retry.
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

        // Every other header code is LOST — including the eGPU removal, which
        // is the rare signal the task brief names.
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

        // The fail-closed arms: unknown code, nil error, garbage status.
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

    /// The latch never flips on `Completed`, `Unfinished` or `Retryable`,
    /// flips on the first `Lost`, stays flipped, and keeps the first reason.
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

        // The injected loss — the signal a real page fault would produce,
        // classified through the same seam `outcome_of` feeds.
        latch.record(&classify(STATUS_ERROR, Some(ERROR_PAGE_FAULT)));
        assert!(latch.is_lost(), "the first Lost must latch");
        let first = latch
            .reason()
            .expect("a latched latch has a reason")
            .to_owned();
        assert!(
            first.contains("PageFault"),
            "the reason names the classification: {first}"
        );

        // Sticky: neither success nor a DIFFERENT loss moves it.
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
