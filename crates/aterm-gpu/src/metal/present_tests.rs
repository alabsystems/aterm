// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::super::acquire_worker::tests::{WorkerSide, stepped};
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

/// A standalone surface whose drawable worker has no thread: the test runs each queued
/// acquisition where it says. `wakes` counts the wakes the worker's side posted.
struct SteppedSurface {
    surface: MetalWindowSurface,
    side: WorkerSide<LayerAcquire, AcquiredDrawable, Retirement>,
    wakes: Arc<AtomicUsize>,
}

type Retirement = aterm_objc::MainThreadBound<RetiredSwapchain>;

impl SteppedSurface {
    fn new(dev: &ffi::Device, config: SwapchainConfig) -> Self {
        let mut surface = MetalWindowSurface::standalone(dev, config, Arc::new(LossLatch::new()))
            .expect("surface");
        let wakes = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&wakes);
        let (worker, side) = stepped(Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        }));
        surface.worker = Some(worker);
        Self {
            surface,
            side,
            wakes,
        }
    }

    /// The worker's turn for the next queued request, on the live layer. False where none is
    /// queued.
    fn worker_turn(&self) -> bool {
        self.side.run_next(LayerAcquire::run)
    }
}

impl Drop for SteppedSurface {
    fn drop(&mut self) {
        // The surface retires a worker through the main thread, which no test is on: this
        // worker has no thread to retire.
        drop(self.surface.worker.take());
    }
}

/// The request rule of a reshape, on a live layer: a request of the current generation is
/// queued only against a layer that already has the wanted configuration. The request in
/// flight when the want changes completes in the gap between `reconcile`'s look and the
/// frame's, so the frame takes it as stale with the reshape still owed. It queues nothing
/// against the layer of the earlier configuration, and neither does a prefetch; the retry's
/// `reconcile` reshapes, and only then is a request queued, whose drawable is of the wanted
/// configuration. A change of size and one that keeps the size (a capture's readable
/// drawable) go the same way: the second has no texture size to give it away.
#[test]
fn a_frame_that_owes_a_reshape_queues_no_request_until_the_layer_is_reshaped() {
    let dev = device();
    let _test_pool = ffi::AutoreleasePool::new();
    let config = SwapchainConfig {
        format: PixelFormat::Bgra8Unorm,
        width: 16,
        height: 16,
        framebuffer_only: true,
        display_sync: false,
        maximum_drawables: 3,
        opaque: true,
    };
    let resized = SwapchainConfig {
        width: 24,
        ..config
    };
    let readable = SwapchainConfig {
        framebuffer_only: false,
        ..config
    };
    for (change, want) in [("a resize", resized), ("a readable drawable", readable)] {
        let mut s = SteppedSurface::new(&dev, config);
        s.surface.prefetch_drawable();
        assert!(
            !s.surface.reconcile(&dev, &want).expect("reconcile"),
            "{change}: the reshape waits for the request in flight"
        );
        assert_eq!(
            *s.surface.config(),
            config,
            "{change}: the layer is as it was"
        );

        // The request in flight completes after `reconcile` looked and before the frame does.
        assert!(s.worker_turn(), "{change}: the prefetch was queued");
        let refusal = s
            .surface
            .acquire()
            .expect_err("a frame that owes a reshape gets no drawable");
        assert!(
            matches!(refusal, AcquireRefusal::Drift),
            "{change}: the frame is sent back for the reshape: {refusal:?}"
        );
        s.surface.prefetch_drawable();
        assert!(
            !s.worker_turn(),
            "{change}: nothing is queued against the layer of the earlier configuration"
        );
        assert_eq!(
            s.wakes.load(Ordering::SeqCst),
            0,
            "{change}: no frame waits, so no wake is posted"
        );

        assert!(
            s.surface.reconcile(&dev, &want).expect("reconcile"),
            "{change}: with nothing in flight the retry reshapes the layer"
        );
        let refusal = s
            .surface
            .acquire()
            .expect_err("the frame waits for its own request");
        assert!(
            matches!(refusal, AcquireRefusal::Pending),
            "{change}: {refusal:?}"
        );
        assert!(
            s.worker_turn(),
            "{change}: the request is queued once the layer is reshaped"
        );
        assert_eq!(
            s.wakes.load(Ordering::SeqCst),
            1,
            "{change}: the waiting frame is woken"
        );
        assert!(!s.surface.reconcile(&dev, &want).expect("reconcile"));
        let frame = s
            .surface
            .acquire()
            .expect("the reshaped layer's drawable reaches the frame");
        assert_eq!(
            (
                ffi::texture_width(frame.texture()),
                ffi::texture_height(frame.texture()),
            ),
            (want.width, want.height),
            "{change}: the drawable has the wanted size"
        );
        drop(frame);
        assert!(!s.worker_turn(), "{change}: one request served the frame");
    }
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
