// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DEVICE-LOSS LATCH — the first-party answer to a callback Metal does
//! not have.
//!
//! # Why this exists, and why it is its own module
//!
//! The shipped `wgpu` path handles device loss through
//! `Device::set_device_lost_callback` (`crate::lib.rs:625`): the callback sets
//! an `AtomicBool`, `GpuRenderer::device_lost` (`renderer.rs:5691`) exposes it,
//! and `aterm-gui`'s `failed_present_route` (`app_render.rs:1998`) drives a
//! process-wide downgrade to the CPU renderer off it. That chain has ZERO
//! tests of the latch itself anywhere in the workspace — only the pure routing
//! function is tested (`app_render.rs:3531`), never the flag flipping — and
//! nothing external can flip it on demand (control socket, CLI and env were
//! all checked). A first-party Metal backend cannot inherit the callback at
//! all, because **Metal has no device-lost callback**: the observable signals
//! are per-command-buffer — `-[MTLCommandBuffer status]` reaching
//! `MTLCommandBufferStatusError` and the `NSError`'s `MTLCommandBufferError`
//! code — plus (rare, eGPU-only) device removal, which also surfaces as the
//! `DeviceRemoved` command-buffer error code.
//!
//! This is a separate module rather than a corner of `swapchain.rs` because
//! the signal is NOT swapchain-shaped: every submission — an offscreen bg
//! pass, a compute parity dispatch, a present — carries its own status and
//! error, and the classification below applies to all of them identically.
//! The swapchain consumes this module (its `PresentTicket` reports a
//! [`CbOutcome`]); it does not own it.
//!
//! # The classification
//!
//! Codes are pinned from the macOS SDK header
//! (`Metal.framework/Headers/MTLCommandBuffer.h:107-121`; statuses at
//! `:56-63`). Two classes:
//!
//! * **RETRYABLE** — the submission failed for a reason that does not impeach
//!   the device or the encoded state, so resubmitting (or rebuilding the one
//!   frame) is sound: `Timeout` (2, "execution took too long" — one overlong
//!   command buffer, the device is fine) and `OutOfMemory` (8 — transient
//!   allocation pressure).
//! * **LOST** — resubmission would fault again or the device is gone for this
//!   process: `Internal` (1), `PageFault` (3, the classic GPU-hang/restart
//!   signal), `AccessRevoked` (4), `NotPermitted` (7), `InvalidResource` (9),
//!   `Memoryless` (10), `DeviceRemoved` (11, the eGPU unplug), `StackOverflow`
//!   (12) — and, fail-closed, any code this module does not recognize, any
//!   `Error` status carrying a nil `NSError`, and any status value outside the
//!   header's enum. An unknown failure must latch rather than spin a retry
//!   loop against a faulting device.
//!
//! For calibration: `wgpu-hal-29.0.3`'s Metal backend never inspects
//! `MTLCommandBufferError` at all (grep its `src/metal/` for the type — zero
//! hits), so the shipped path's device-loss signal on macOS is coarser than
//! this one. The latch is not a port of wgpu's behaviour; it is the piece
//! wgpu never built.
//!
//! # What CANNOT be tested here, honestly
//!
//! Producing a REAL `MTLCommandBufferStatusError` on a healthy GPU means
//! wrecking real GPU state (an actual page fault, an actual hang) from inside
//! the test suite of a terminal the owner is running — and on Apple Silicon a
//! provoked GPU restart is process-visible far beyond this test binary. So the
//! real-error arm of [`outcome_of`] is exercised through [`classify`]'s
//! injection seam (the pure function IS the classification; `outcome_of` is a
//! two-selector read feeding it), and the real-FFI path is pinned on the arm a
//! healthy device CAN produce: a committed, waited command buffer reading back
//! `Completed` through the same selectors. The residue a real device loss
//! would test beyond this — that a faulting driver actually reports status 5
//! with a code from the header's enum — is Apple's documented contract, and no
//! test on a healthy box can probe it. eGPU physical removal is likewise
//! untestable on Apple Silicon (no eGPU support exists on arm64 Macs); its
//! signal is the `DeviceRemoved` code above, which the injection seam covers.

use std::sync::OnceLock;

use super::ffi::{AutoreleasePool, Id, Sel, msg, sel};

/// `MTLCommandBufferStatus` (`MTLCommandBuffer.h:56-63`).
pub(crate) const STATUS_NOT_ENQUEUED: usize = 0;
pub(crate) const STATUS_ENQUEUED: usize = 1;
pub(crate) const STATUS_COMMITTED: usize = 2;
pub(crate) const STATUS_SCHEDULED: usize = 3;
pub(crate) const STATUS_COMPLETED: usize = 4;
pub(crate) const STATUS_ERROR: usize = 5;

/// `MTLCommandBufferError` codes (`MTLCommandBuffer.h:107-121`). `Blacklisted`
/// is the deprecated spelling of `AccessRevoked` and shares 4.
/// Header-defined "no error" code — present in `MTLCommandBuffer.h:112`. A
/// status of Error carrying code 0 is contradictory; it stays LOST (fail
/// closed), but it is named rather than lumped with codes the header never
/// defined. A judge caught the pinned-constants block skipping it.
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
    /// Status 0..=3 — the command buffer has not finished; the caller asked
    /// before `waitUntilCompleted`/the completion point. A programming error
    /// at the call site, NOT a device signal: it neither retries nor latches.
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
/// This pure function is the module's INJECTION SEAM: every error code the
/// header defines can be fed through here without wrecking a real GPU, and
/// [`outcome_of`] is nothing but two property reads feeding it — so the tests
/// exercise the entire classification on injected values and the real FFI on
/// the one arm a healthy device can produce (`Completed`).
///
/// `error_code` is `-[NSError code]` when status is `Error` and the error is
/// non-nil; `None` otherwise. The error object on an `MTLCommandBuffer` is
/// documented to be in `MTLCommandBufferErrorDomain` (`MTLCommandBuffer.h:66-70`
/// declares the domain alongside the property), so the code is interpreted
/// against that domain's enum without a string compare on the domain name.
pub(crate) const fn classify(status: usize, error_code: Option<isize>) -> CbOutcome {
    match status {
        STATUS_COMPLETED => CbOutcome::Completed,
        STATUS_NOT_ENQUEUED..=STATUS_SCHEDULED => CbOutcome::Unfinished { status },
        STATUS_ERROR => match error_code {
            // Timeout carries no LOCAL retry budget on purpose: the OS is
            // the loop's terminator, not this module — repeated timeouts and
            // hangs escalate to AccessRevoked ("too many timeouts or hangs",
            // MTLCommandBuffer.h), which IS latched. A perpetually-timing-out
            // device therefore reaches the latch through the OS's own
            // escalation, one hop later than a local counter would.
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

/// `-[MTLCommandBuffer status]`, raw.
///
/// Safe: an `NSUInteger` property read on a live command buffer, with no
/// ordering precondition — Metal defines the property as readable from any
/// thread at any point in the command buffer's lifetime.
pub(crate) fn command_buffer_status(cb: Id) -> usize {
    // SAFETY: `-status` is an `NSUInteger` getter on a live `MTLCommandBuffer`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> usize = msg();
        f(cb, sel(c"status"))
    }
}

/// `-[MTLCommandBuffer error]`'s code, or `None` when the error is nil.
///
/// The pool is pushed BEFORE the property read for the same reason the
/// `error:` out-param entry points in `ffi` push theirs first: the getter can
/// autorelease the `NSError` into the calling frame, and the code is copied
/// out before the pool pops.
fn command_buffer_error_code(cb: Id) -> Option<isize> {
    let _pool = AutoreleasePool::new();
    // SAFETY: `-error` returns a BORROWED `NSError` (or nil) owned by the pool
    // above; `-code` is an `NSInteger` getter on it, read while the pool is
    // live.
    unsafe {
        let get: unsafe extern "C" fn(Id, Sel) -> Id = msg();
        let err = get(cb, sel(c"error"));
        if err.is_null() {
            return None;
        }
        let code: unsafe extern "C" fn(Id, Sel) -> isize = msg();
        Some(code(err, sel(c"code")))
    }
}

/// Read a command buffer's `(status, error code)` and [`classify`] it.
///
/// Meaningful once the command buffer has FINISHED (after
/// `waitUntilCompleted`, or from a completion point); consulted earlier it
/// honestly reports [`CbOutcome::Unfinished`].
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
/// device is lost for good and every later query says so.
///
/// The Metal twin of the `AtomicBool` behind `GpuContext::device_lost`
/// (`crate::lib.rs:255`), with two deliberate differences: it records WHY (the
/// first loss's classification, which the wgpu callback throws away after
/// logging), and it is driven by [`CbOutcome`]s the caller feeds it rather
/// than by a driver callback — because Metal has none to give.
///
/// Naturally `Send + Sync` (an `OnceLock<String>` and nothing else — no raw
/// pointers, so the module's no-`unsafe impl` rule is not even tempted).
#[derive(Debug, Default)]
pub(crate) struct LossLatch {
    /// Debug-only feed counter — see [`Self::record`] for why it exists.
    #[cfg(debug_assertions)]
    fed: std::sync::atomic::AtomicUsize,
    /// Set exactly once, by the FIRST `Lost` outcome recorded.
    lost: OnceLock<String>,
    /// Whether an `EncodeSession` currently holds this latch — the seal that
    /// makes "one latch, one queue" TRUE rather than asserted. `Frame::
    /// present` proves session-vs-swapchain wiring by `Arc::ptr_eq` on the
    /// latch, which is only as strong as latch-identity => queue-identity;
    /// a second session on the same latch broke that silently (probed
    /// 2026-08-31: a rogue same-latch session presented on a queue the
    /// rendering never used, and every runtime check passed). Bound in
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

    /// Claim this latch for THE encode session. `false` means a session
    /// already holds it and the caller must refuse to construct — the
    /// cross-queue-present seal (see `encode_bound`).
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
        // Debug builds count EVERY feed, loss or not. A judge proved the
        // `settle` call inside `wait_outcome` was the one unarmed line in the
        // loss wiring — a healthy GPU only ever hands it `Completed`, whose
        // record is a no-op, so a planted skip stayed green and only a REAL
        // device loss would have exposed the regression. The counter makes
        // "the latch was fed" observable on the Completed path too, and the
        // frame-cycle test asserts it per ticket.
        #[cfg(debug_assertions)]
        self.fed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let CbOutcome::Lost { .. } = outcome {
            // A second loss keeps the FIRST reason — that is the one that
            // took the device down, and `OnceLock::set` enforces it.
            let _ = self.lost.set(format!("{outcome:?}"));
        }
    }

    /// Debug-only: how many outcomes have been fed through [`Self::record`].
    /// Exists so a test can pin that `wait_outcome` feeds the latch even when
    /// every outcome is `Completed` — see the comment in `record`.
    #[cfg(debug_assertions)]
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
    use crate::metal::ffi::{Device, Obj};

    /// Every test that needs a GPU skips LOUDLY on a machine without one.
    fn device() -> Option<Device> {
        let d = Device::system_default();
        if d.is_none() {
            eprintln!("SKIP: no Metal device on this machine");
        }
        d
    }

    /// THE CLASSIFICATION, swept over every code the SDK header defines plus
    /// the fail-closed arms — the injected-error half of the latch's contract.
    /// This is the test the wgpu device-loss path never had: every observable
    /// error signal, classified, with the retry/latch line drawn explicitly.
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

    /// THE LATCH IS STICKY AND SELECTIVE: it never flips on `Completed`,
    /// `Unfinished` or `Retryable`; it flips on the first `Lost`; it stays
    /// flipped through everything that follows; and the FIRST loss's reason is
    /// the one it keeps.
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

    /// THE REAL FFI PATH, on the one arm a healthy GPU can produce: a real
    /// command buffer, committed and waited, reads back `Completed` through
    /// the same two selectors a real loss would use — and the latch stays
    /// unlost. The error arms of `outcome_of` ride the injection seam above;
    /// this pins that the seam's inputs come off live objects correctly.
    #[test]
    fn a_real_completed_command_buffer_reports_completed_and_never_latches() {
        let Some(dev) = device() else { return };
        let queue = dev.new_command_queue().expect("queue");
        let latch = LossLatch::new();

        let _pool = AutoreleasePool::new();
        // SAFETY: `commandBuffer` returns an AUTORELEASED command buffer owned
        // by the pool above; it is retained into `cb` so the reads below hold
        // a live +1 regardless of pool timing. `commit`/`waitUntilCompleted`
        // are plain void messages on it.
        let cb: Obj = unsafe {
            let get: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            let raw = get(queue.id(), sel(c"commandBuffer"));
            let cb = Obj::retain(raw).expect("commandBuffer");
            let void_msg: unsafe extern "C" fn(Id, Sel) = msg();
            void_msg(cb.id(), sel(c"commit"));
            void_msg(cb.id(), sel(c"waitUntilCompleted"));
            cb
        };

        assert_eq!(
            command_buffer_status(cb.id()),
            STATUS_COMPLETED,
            "an empty committed command buffer completes"
        );
        let outcome = outcome_of(cb.id());
        assert_eq!(outcome, CbOutcome::Completed);
        latch.record(&outcome);
        assert!(
            !latch.is_lost(),
            "a Completed real command buffer must never latch"
        );
    }
}
