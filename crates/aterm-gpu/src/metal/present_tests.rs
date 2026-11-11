// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

use super::super::loss::{CbOutcome, LossLatch};
use super::*;
use crate::metal::test_support::{device, plain_calayer};

/// A bare `CALayer` at a caller-chosen `contentsScale` — the headless
/// stand-in for the winit view's backing layer on a Retina display.
fn parent_at_scale(scale: f64) -> Obj {
    let layer = plain_calayer();
    let _pool = ffi::AutoreleasePool::new();
    // SAFETY: sets `contentsScale`, a scalar `CGFloat`, on a live `CALayer`.
    unsafe {
        let set: unsafe extern "C-unwind" fn(ffi::Id, ffi::Sel, f64) = ffi::checked_msg();
        set(layer.id(), ffi::sel(c"setContentsScale:"), scale);
    }
    layer
}

/// `contentsScale` is not a config axis, so the sync must run before
/// `reconcile`'s no-drift early-out: with a constant config, `reconcile`
/// returns `false` every time and the scale must still follow the parent.
#[test]
fn the_no_drift_reconcile_still_tracks_the_parents_contents_scale() {
    let dev = device();
    let _test_pool = ffi::AutoreleasePool::new();
    let parent = parent_at_scale(2.0);
    let config = SwapchainConfig {
        format: PixelFormat::Bgra8Unorm,
        width: 16,
        height: 16,
        framebuffer_only: false,
        display_sync: false,
        maximum_drawables: 2,
        opaque: true,
    };
    let mut surface =
        MetalWindowSurface::attached(&dev, &parent, config, Arc::new(LossLatch::new()))
            .expect("attached surface");
    assert!(
        (surface.contents_scale() - 2.0).abs() < f64::EPSILON,
        "attach adopts the Retina parent's scale — got {}",
        surface.contents_scale()
    );

    let _pool = ffi::AutoreleasePool::new();
    // SAFETY: sets `contentsScale`, a scalar `CGFloat`, on the live parent `CALayer`.
    unsafe {
        let set: unsafe extern "C-unwind" fn(ffi::Id, ffi::Sel, f64) = ffi::checked_msg();
        set(parent.id(), ffi::sel(c"setContentsScale:"), 1.0);
    }
    let reconfigured = surface
        .reconcile(&dev, &config)
        .expect("a no-drift reconcile succeeds");
    assert!(
        !reconfigured,
        "nothing in the drift set moved — this is the steady-present path"
    );
    assert!(
        (surface.contents_scale() - 1.0).abs() < f64::EPSILON,
        "the steady present must still adopt the parent's new scale: \
         contentsScale is not a drift axis and cannot be one, so the poll \
         has to run ahead of the early-out — got {}",
        surface.contents_scale()
    );
}

#[test]
fn terminal_worker_failure_enters_permanent_backend_recovery() {
    let latch = LossLatch::new();
    assert!(latch.reason().is_none());
    let refusal = terminal_worker_refusal(&latch);
    assert!(
        latch.reason().is_some(),
        "a Validation-only mutant would park forever"
    );
    assert!(matches!(refusal, AcquireRefusal::LatchLost));
    assert_eq!(
        surface_present_failure(&refusal, false),
        SurfacePresentFailure::Validation
    );
}

/// Live drift, deviceless-nil and latch refusals map to `Reconfigured`,
/// `Timeout`/`Occluded` (by the hint) and `Validation`.
#[test]
fn acquire_refusals_map_to_the_surface_failure_contract() {
    let dev = device();
    let latch = Arc::new(LossLatch::new());
    let config = SwapchainConfig {
        format: PixelFormat::Bgra8Unorm,
        width: 16,
        height: 16,
        framebuffer_only: false,
        display_sync: false,
        maximum_drawables: 3,
        opaque: true,
    };
    let mut surface =
        MetalWindowSurface::standalone(&dev, config, Arc::clone(&latch)).expect("surface");

    surface.config.width = 24;
    let refusal = surface.acquire().expect_err("a drifted acquire refuses");
    assert!(
        matches!(refusal, AcquireRefusal::Drift),
        "a drifted acquire refuses as Drift: {refusal:?}"
    );
    assert_eq!(
        surface_present_failure(&refusal, false),
        SurfacePresentFailure::Reconfigured
    );
    surface.config.width = 16;

    // SAFETY: `setDevice:` accepts nil on a live `CAMetalLayer`.
    unsafe {
        let set_obj: unsafe extern "C-unwind" fn(ffi::Id, ffi::Sel, ffi::Id) = ffi::checked_msg();
        set_obj(
            surface.swapchain.layer_ptr(),
            ffi::sel(c"setDevice:"),
            ffi::Id::NIL,
        );
    }
    let t0 = std::time::Instant::now();
    let nil = surface
        .acquire()
        .expect_err("a deviceless layer must not vend");
    let waited = t0.elapsed();
    assert!(
        matches!(nil, AcquireRefusal::AcquireNil),
        "the deviceless refusal is the nil arm, not drift/latch: {nil:?}"
    );
    assert!(
        waited < std::time::Duration::from_secs(60),
        "the live nil is BOUNDED — {waited:?} is not a parked thread"
    );
    assert_eq!(
        surface_present_failure(&nil, false),
        SurfacePresentFailure::Timeout
    );
    assert_eq!(
        surface_present_failure(&nil, true),
        SurfacePresentFailure::Occluded
    );
    // SAFETY: `setDevice:` restores the valid `MTLDevice` on the live `CAMetalLayer`.
    unsafe {
        let set_obj: unsafe extern "C-unwind" fn(ffi::Id, ffi::Sel, ffi::Id) = ffi::checked_msg();
        set_obj(
            surface.swapchain.layer_ptr(),
            ffi::sel(c"setDevice:"),
            dev.id(),
        );
    }
    drop(
        surface
            .acquire()
            .expect("restoring the device must restore acquire"),
    );

    latch.record(&CbOutcome::Lost {
        code: None,
        name: "injected",
    });
    let refusal = surface.acquire().expect_err("a latched acquire refuses");
    assert!(matches!(refusal, AcquireRefusal::LatchLost));
    assert_eq!(
        surface_present_failure(&refusal, false),
        SurfacePresentFailure::Validation
    );
    crate::stderr_line!(
        "failure mapping on {}: drift->Reconfigured (live), deviceless \
         nil->Timeout/Occluded (live, hint-split), latch->Validation (live)",
        dev.name()
    );
}
