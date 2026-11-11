// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Direct `CAMetalLayer` swapchain management.

use std::ffi::c_void;
use std::sync::Arc;

use aterm_objc::Bool as ObjcBool;

use super::ffi::{
    AutoreleasePool, ClassPtr, Device, Id, Obj, PixelFormat, Sel, checked_msg, class, sel,
};
use super::{encoder, loss};

#[link(name = "QuartzCore", kind = "framework")]
unsafe extern "C" {
    fn CACurrentMediaTime() -> f64;
}

/// `CACurrentMediaTime()`: the clock a drawable's `presentedTime` reads, seconds
/// (`present_glass::media_ns_of`).
///
/// # Safety
/// None beyond the call: it reads a monotonic clock.
pub(crate) unsafe fn media_time_s() -> f64 {
    // SAFETY: reading a monotonic clock.
    unsafe { CACurrentMediaTime() }
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGColorSpaceCreateWithName(name: *const c_void) -> *mut c_void;
    /// +0: a borrowed `CFStringRef` owned by the colorspace.
    #[cfg(test)]
    fn CGColorSpaceGetName(space: *mut c_void) -> *const c_void;
    fn CGColorSpaceRelease(space: *mut c_void);
    static kCGColorSpaceExtendedLinearSRGB: *const c_void;
    static kCGColorSpaceSRGB: *const c_void;
}

/// # Safety
/// `name` must be NULL or a live `CFString`.
#[cfg(test)]
unsafe fn cf_string_text(name: *const c_void) -> Option<String> {
    // SAFETY: a live CFString is toll-free bridged to NSString; the caller vouches for it, and it is copied out at once.
    (!name.is_null()).then(|| unsafe { super::ffi::ns_string_text(Id::from_ptr(name.cast_mut())) })
}

/// `CGSize`: two `double`s, passed and returned in `v0`/`v1` on
/// `aarch64-apple-darwin` like [`super::ffi::ClearColor`].
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CgSize {
    width: f64,
    height: f64,
}

// SAFETY: `#[repr(C)]` two `double`s (`CGSize`).
unsafe impl aterm_objc::Encode for CgSize {
    const ENCODING: &'static str = "{CGSize=dd}";
}

/// `CGPoint`/`CGRect`, the `setFrame:` argument.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CgPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CgRect {
    origin: CgPoint,
    size: CgSize,
}

// SAFETY: `#[repr(C)]` a `CGPoint` followed by a `CGSize` (`CGRect`).
unsafe impl aterm_objc::Encode for CgRect {
    const ENCODING: &'static str = "{CGRect={CGPoint=dd}{CGSize=dd}}";
}

/// Everything one swapchain configure sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SwapchainConfig {
    /// The layer `pixelFormat`: [`PixelFormat::Bgra8Unorm`] (SDR) or [`PixelFormat::Rgba16Float`] (EDR).
    pub(crate) format: PixelFormat,
    /// `drawableSize`, in pixels.
    pub(crate) width: usize,
    pub(crate) height: usize,
    /// `framebufferOnly`: `true` gives lossless drawable compression but no sampling or blit access.
    pub(crate) framebuffer_only: bool,
    /// `displaySyncEnabled`.
    pub(crate) display_sync: bool,
    /// `maximumDrawableCount`.
    pub(crate) maximum_drawables: usize,
    /// `CALayer.opaque`.
    pub(crate) opaque: bool,
}

impl SwapchainConfig {
    /// The production macOS values.
    #[cfg(test)]
    pub(crate) const fn aterm_present(format: PixelFormat, width: usize, height: usize) -> Self {
        Self {
            format,
            width,
            height,
            framebuffer_only: true,
            display_sync: false,
            maximum_drawables: 3,
            opaque: true,
        }
    }

    fn validate(&self) -> Result<(), String> {
        match self.format {
            PixelFormat::Bgra8Unorm | PixelFormat::Rgba16Float => {}
            other => {
                return Err(format!(
                    "{other:?} is not a CAMetalLayer pixelFormat aterm can present: the \
                     layer accepts the BGRA8/f16/XR set"
                ));
            }
        }
        if !(2..=3).contains(&self.maximum_drawables) {
            return Err(format!(
                "maximumDrawableCount {} is outside CAMetalLayer's legal [2, 3] \
                 (CAMetalLayer.h:104-108 — out-of-range values throw)",
                self.maximum_drawables
            ));
        }
        if self.width == 0 || self.height == 0 {
            return Err(format!(
                "a {}x{} drawableSize is an invalid drawable-property combination — \
                 nextDrawable would return nil forever (CAMetalLayer.h:95-97)",
                self.width, self.height
            ));
        }
        Ok(())
    }
}

/// Seconds within which a nil `nextDrawable` is the layer refusing (an invalid configuration, a
/// drawable with no texture) rather than its one-second wait for a free drawable running out:
/// [`Swapchain::acquire_within`] asks again only after the wait.
const REFUSED_WITHIN_S: f64 = 0.5;

/// One configured `CAMetalLayer` and what every acquire needs.
#[derive(Debug)]
pub(crate) struct Swapchain {
    /// The +1 layer. Owned; released on drop.
    layer: Obj,
    /// Keeps the attachment parent alive through deferred retirement, even when
    /// NSView releases its layer during close.
    parent: Option<Obj>,
    /// Whether [`Self::attached`] parented the layer, and drop must unparent.
    is_attached: bool,
    /// Change gate for [`Self::sync_layer_geometry`]: the `(contentsScale,
    /// width, height)` last written, or `None` before the first sync (always
    /// on the standalone shape).
    applied_geometry: Option<(f64, usize, usize)>,
    format: PixelFormat,
    width: usize,
    height: usize,
    /// The process device-loss latch, shared with every other submitter.
    latch: Arc<loss::LossLatch>,
}

impl Swapchain {
    /// A standalone (headless) swapchain with no window or layer tree.
    #[cfg(test)]
    pub(crate) fn standalone(
        device: &Device,
        config: &SwapchainConfig,
        latch: Arc<loss::LossLatch>,
    ) -> Result<Self, String> {
        let layer = Self::new_layer()?;
        Self::configure(&layer, device, config)?;
        Ok(Self {
            layer,
            parent: None,
            is_attached: false,
            applied_geometry: None,
            format: config.format,
            width: config.width,
            height: config.height,
            latch,
        })
    }

    /// A swapchain parented under `parent` (any `CALayer`).
    pub(crate) fn attached(
        device: &Device,
        parent: &Obj,
        config: &SwapchainConfig,
        latch: Arc<loss::LossLatch>,
    ) -> Result<Self, String> {
        let layer = Self::new_layer()?;
        Self::configure(&layer, device, config)?;
        // SAFETY: `addSublayer:` is a void message on the live parent that retains the sublayer; our +1 stays ours for drop to balance.
        unsafe {
            let add: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
            add(parent.id(), sel(c"addSublayer:"), layer.id());
        }
        let mut sc = Self {
            layer,
            parent: Some(parent.clone_retained()),
            is_attached: true,
            applied_geometry: None,
            format: config.format,
            width: config.width,
            height: config.height,
            latch,
        };
        sc.sync_layer_geometry();
        Ok(sc)
    }

    /// Adopt the parent layer's `contentsScale`, so a drawable pixel is a display pixel.
    pub(crate) fn sync_layer_geometry(&mut self) {
        if !self.is_attached {
            return;
        }
        let _pool = AutoreleasePool::new();
        // SAFETY: `superlayer` answers a borrowed `CALayer` or nil, never released here; `contentsScale` and `setContentsScale:` pass a `CGFloat` by value, scalar on every target, so no `objc_msgSend_stret`.
        unsafe {
            let superlayer: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let parent = superlayer(self.layer.id(), sel(c"superlayer"));
            if parent.is_null() {
                return;
            }
            let get_scale: unsafe extern "C-unwind" fn(Id, Sel) -> f64 = checked_msg();
            let scale = get_scale(parent, sel(c"contentsScale"));
            if scale > 0.0 && self.applied_geometry != Some((scale, self.width, self.height)) {
                let txn = class(c"CATransaction");
                let cls_void: unsafe extern "C-unwind" fn(ClassPtr, Sel) = checked_msg();
                let cls_bool: unsafe extern "C-unwind" fn(ClassPtr, Sel, ObjcBool) = checked_msg();
                cls_void(txn, sel(c"begin"));
                cls_bool(txn, sel(c"setDisableActions:"), true.into());
                let set_scale: unsafe extern "C-unwind" fn(Id, Sel, f64) = checked_msg();
                set_scale(self.layer.id(), sel(c"setContentsScale:"), scale);
                let set_frame: unsafe extern "C-unwind" fn(Id, Sel, CgRect) = checked_msg();
                set_frame(
                    self.layer.id(),
                    sel(c"setFrame:"),
                    CgRect {
                        origin: CgPoint { x: 0.0, y: 0.0 },
                        size: CgSize {
                            #[expect(
                                clippy::cast_precision_loss,
                                reason = "drawable extents are far below 2^53"
                            )]
                            width: self.width as f64 / scale,
                            #[expect(clippy::cast_precision_loss, reason = "as above")]
                            height: self.height as f64 / scale,
                        },
                    },
                );
                cls_void(txn, sel(c"commit"));
                self.applied_geometry = Some((scale, self.width, self.height));
            }
        }
    }

    /// The `contentsScale` read back off the live layer, not echoed from [`Self::applied_geometry`].
    #[cfg(test)]
    pub(crate) fn contents_scale(&self) -> f64 {
        let _pool = AutoreleasePool::new();
        // SAFETY: scalar `CGFloat` getter on our live +1 layer.
        unsafe {
            let get_scale: unsafe extern "C-unwind" fn(Id, Sel) -> f64 = checked_msg();
            get_scale(self.layer.id(), sel(c"contentsScale"))
        }
    }

    fn new_layer() -> Result<Obj, String> {
        let _pool = AutoreleasePool::new();
        let cls = class(c"CAMetalLayer");
        if cls.is_null() {
            return Err(
                "CAMetalLayer class not found — QuartzCore did not load, which this \
                 file's own #[link] is supposed to guarantee"
                    .to_owned(),
            );
        }
        // SAFETY: `alloc` and `init` return an initialized +1 object or nil.
        unsafe {
            let alloc: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id = checked_msg();
            let raw = alloc(cls, sel(c"alloc"));
            let init: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            Obj::from_owned(init(raw, sel(c"init")))
                .ok_or_else(|| "CAMetalLayer init returned nil".to_owned())
        }
    }

    fn configure(layer: &Obj, device: &Device, config: &SwapchainConfig) -> Result<(), String> {
        config.validate()?;
        let _pool = AutoreleasePool::new();
        // SAFETY: documented property setters on the live layer, each prototype written out; the colorspace is created at +1 and released after `setColorspace:`, which retains it.
        unsafe {
            let set_obj: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
            let set_usize: unsafe extern "C-unwind" fn(Id, Sel, usize) = checked_msg();
            let set_bool: unsafe extern "C-unwind" fn(Id, Sel, ObjcBool) = checked_msg();
            let set_size: unsafe extern "C-unwind" fn(Id, Sel, CgSize) = checked_msg();

            let wants_edr = config.format == PixelFormat::Rgba16Float;
            let space_name = if wants_edr {
                kCGColorSpaceExtendedLinearSRGB
            } else {
                kCGColorSpaceSRGB
            };
            let space = CGColorSpaceCreateWithName(space_name);
            if space.is_null() {
                return Err("CGColorSpaceCreateWithName failed".to_owned());
            }

            set_obj(layer.id(), sel(c"setDevice:"), device.id());
            set_usize(layer.id(), sel(c"setPixelFormat:"), config.format as usize);
            set_size(
                layer.id(),
                sel(c"setDrawableSize:"),
                CgSize {
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "drawable extents are far below 2^53"
                    )]
                    width: config.width as f64,
                    #[expect(clippy::cast_precision_loss, reason = "as above")]
                    height: config.height as f64,
                },
            );
            set_bool(
                layer.id(),
                sel(c"setFramebufferOnly:"),
                config.framebuffer_only.into(),
            );
            set_bool(
                layer.id(),
                sel(c"setDisplaySyncEnabled:"),
                config.display_sync.into(),
            );
            set_usize(
                layer.id(),
                sel(c"setMaximumDrawableCount:"),
                config.maximum_drawables,
            );
            set_bool(layer.id(), sel(c"setOpaque:"), config.opaque.into());
            set_bool(
                layer.id(),
                sel(c"setAllowsNextDrawableTimeout:"),
                true.into(),
            );

            set_bool(
                layer.id(),
                sel(c"setWantsExtendedDynamicRangeContent:"),
                wants_edr.into(),
            );
            let set_space: unsafe extern "C-unwind" fn(Id, Sel, *mut c_void) = checked_msg();
            set_space(layer.id(), sel(c"setColorspace:"), space);
            CGColorSpaceRelease(space);
        }
        Ok(())
    }

    /// Apply a new config to the live layer instead of rebuilding the `Swapchain` (resize, HDR flip, `COPY_SRC` arming).
    pub(crate) fn reconfigure(
        &mut self,
        device: &Device,
        config: &SwapchainConfig,
    ) -> Result<(), String> {
        if let Some(reason) = self.latch.reason() {
            return Err(format!(
                "reconfigure refused: the device-loss latch is set ({reason}); \
                 rebuild on a fresh device instead of reshaping a dead layer"
            ));
        }
        Self::configure(&self.layer, device, config)?;
        self.format = config.format;
        self.width = config.width;
        self.height = config.height;
        Ok(())
    }

    /// Acquire the next drawable as a [`Frame`], or fail within a bound.
    pub(crate) fn acquire(&mut self) -> Result<Frame<'_>, String> {
        self.acquire_within(std::time::Duration::ZERO)
    }

    /// [`Self::acquire`], asked again until `hang` has passed while the pool stays empty: a nil
    /// that took the layer's own wait (`allowsNextDrawableTimeout`, a second) asks again, so a
    /// caller whose earlier presents must retire waits `hang` for one of their drawables rather
    /// than that second; a nil sooner ([`REFUSED_WITHIN_S`]) is the layer refusing, and fails at
    /// once.
    pub(crate) fn acquire_within(
        &mut self,
        hang: std::time::Duration,
    ) -> Result<Frame<'_>, String> {
        // SAFETY: reading a monotonic clock.
        let started = unsafe { CACurrentMediaTime() };
        let mut asks = 0u32;
        loop {
            if let Some(reason) = self.latch.reason() {
                return Err(format!(
                    "acquire refused: the device-loss latch is set ({reason}); \
                     not touching the layer or its queue — downgrade instead"
                ));
            }
            // SAFETY: reading a monotonic clock.
            let asked = unsafe { CACurrentMediaTime() };
            asks = asks.saturating_add(1);
            if let Some((drawable, texture)) = self.raw_next_drawable() {
                return Ok(Frame {
                    drawable,
                    texture,
                    swapchain: self,
                });
            }
            // SAFETY: reading a monotonic clock.
            let now = unsafe { CACurrentMediaTime() };
            let waited = now - started;
            if waited >= hang.as_secs_f64() || now - asked < REFUSED_WITHIN_S {
                return Err(format!(
                    "nextDrawable returned nil after {waited:.2}s (asked {asks}x) — all \
                     {} drawables in flight (or an invalid layer configuration); \
                     the bounded timeout is allowsNextDrawableTimeout=YES doing \
                     its job instead of parking the thread",
                    self.maximum_drawable_count(),
                ));
            }
        }
    }

    fn raw_next_drawable(&self) -> Option<(Obj, Obj)> {
        acquire_drawable(&self.layer).map(OwnedDrawable::into_parts)
    }

    /// Retain only the layer needed by the worker, never the attached surface.
    pub(crate) fn acquire_request(&self) -> LayerAcquire {
        LayerAcquire {
            layer: Some(self.layer.clone_retained()),
        }
    }

    pub(crate) fn frame_from_acquired(
        &mut self,
        acquired: OwnedDrawable,
    ) -> Result<Frame<'_>, String> {
        if acquired.source_layer.as_ref().map(Obj::id) != Some(self.layer.id()) {
            return Err("drawable belongs to a different layer/loss domain".into());
        }
        let (drawable, texture) = acquired.into_parts();
        Ok(Frame {
            drawable,
            texture,
            swapchain: self,
        })
    }

    /// The raw `MTLPixelFormat` off the layer.
    #[cfg(test)]
    pub(crate) fn raw_pixel_format(&self) -> usize {
        // SAFETY: `NSUInteger` getter on the live layer.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
            f(self.layer.id(), sel(c"pixelFormat"))
        }
    }

    #[cfg(test)]
    pub(crate) fn drawable_size(&self) -> (f64, f64) {
        // SAFETY: `drawableSize` returns `CGSize` by value.
        let s = unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> CgSize = checked_msg();
            f(self.layer.id(), sel(c"drawableSize"))
        };
        (s.width, s.height)
    }

    #[cfg(test)]
    pub(crate) fn framebuffer_only(&self) -> bool {
        // SAFETY: `BOOL` getter on the live layer.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> ObjcBool = checked_msg();
            f(self.layer.id(), sel(c"framebufferOnly")).as_bool()
        }
    }

    #[cfg(test)]
    pub(crate) fn display_sync_enabled(&self) -> bool {
        // SAFETY: `BOOL` getter on the live layer.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> ObjcBool = checked_msg();
            f(self.layer.id(), sel(c"displaySyncEnabled")).as_bool()
        }
    }

    pub(crate) fn maximum_drawable_count(&self) -> usize {
        // SAFETY: `NSUInteger` getter on the live layer.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
            f(self.layer.id(), sel(c"maximumDrawableCount"))
        }
    }

    #[cfg(test)]
    pub(crate) fn wants_extended_dynamic_range(&self) -> bool {
        // SAFETY: `BOOL` getter on the live layer.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> ObjcBool = checked_msg();
            f(self.layer.id(), sel(c"wantsExtendedDynamicRangeContent")).as_bool()
        }
    }

    #[cfg(test)]
    pub(crate) fn is_opaque(&self) -> bool {
        // SAFETY: `BOOL` getter on the live layer.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> ObjcBool = checked_msg();
            f(self.layer.id(), sel(c"isOpaque")).as_bool()
        }
    }

    /// The layer colorspace's CoreGraphics name, or `None` when nil or unnamed.
    #[cfg(test)]
    pub(crate) fn colorspace_name(&self) -> Option<String> {
        // SAFETY: `colorspace` answers a +0 `CGColorSpaceRef` or NULL, and `CGColorSpaceGetName` a CFString (or NULL) the colorspace owns, copied out at once; nothing is retained or released.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> *mut c_void = checked_msg();
            let space = f(self.layer.id(), sel(c"colorspace"));
            if space.is_null() {
                return None;
            }
            cf_string_text(CGColorSpaceGetName(space))
        }
    }

    /// The raw layer pointer (for superlayer/sublayer identity checks).
    #[cfg(test)]
    pub(crate) fn layer_ptr(&self) -> Id {
        self.layer.id()
    }

    /// The device-loss latch this swapchain answers to.
    #[inline]
    pub(crate) const fn latch(&self) -> &Arc<loss::LossLatch> {
        &self.latch
    }
}

impl Drop for Swapchain {
    fn drop(&mut self) {
        if self.is_attached {
            // SAFETY: `removeFromSuperlayer` detaches the live layer, a no-op if it is already detached; the parent's retain drops and our +1 drops with `Obj`.
            unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel) = checked_msg();
                f(self.layer.id(), sel(c"removeFromSuperlayer"));
            }
        }
        let _pool = AutoreleasePool::new();
        drop(self.parent.take());
    }
}

/// A retained `CAMetalLayer` used only for `nextDrawable`.
pub(crate) struct LayerAcquire {
    layer: Option<Obj>,
}

// SAFETY: `LayerAcquire` holds a retained `CAMetalLayer`, whose `nextDrawable` may run on another thread; its owner defers every layer mutation until the request completes.
unsafe impl Send for LayerAcquire {}

impl LayerAcquire {
    pub(crate) fn run(self) -> AcquiredDrawable {
        let started = aterm_time::Instant::now();
        let drawable = acquire_drawable(self.layer.as_ref().expect("live acquisition layer"));
        let wait_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        AcquiredDrawable { drawable, wait_ns }
    }
}

impl Drop for LayerAcquire {
    fn drop(&mut self) {
        let _pool = AutoreleasePool::new();
        drop(self.layer.take());
    }
}

pub(crate) struct AcquiredDrawable {
    pub(crate) drawable: Option<OwnedDrawable>,
    pub(crate) wait_ns: u64,
}

/// Exclusive +1 ownership, released in a pool even for stale/closed results.
pub(crate) struct OwnedDrawable {
    source_layer: Option<Obj>,
    objects: Option<(Obj, Obj)>,
}

// SAFETY: Metal drawable and texture references support cross-thread handoff; ownership moves once and neither reference is accessed concurrently.
unsafe impl Send for OwnedDrawable {}

impl OwnedDrawable {
    fn into_parts(mut self) -> (Obj, Obj) {
        self.objects.take().expect("owned drawable consumed once")
    }
}

impl Drop for OwnedDrawable {
    fn drop(&mut self) {
        let _pool = AutoreleasePool::new();
        drop(self.objects.take());
        drop(self.source_layer.take());
    }
}

fn acquire_drawable(layer: &Obj) -> Option<OwnedDrawable> {
    let _pool = AutoreleasePool::new();
    #[cfg(test)]
    encoder::queue_ffi_census::note();
    // SAFETY: `layer` is a retained `CAMetalLayer`; both +0 returns are retained under the pool and nil refuses. The caller serializes this with layer mutation and other acquisitions.
    unsafe {
        let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
        let drawable = Obj::retain(get(layer.id(), sel(c"nextDrawable")))?;
        let texture = Obj::retain(get(drawable.id(), sel(c"texture")))?;
        Some(OwnedDrawable {
            source_layer: Some(layer.clone_retained()),
            objects: Some((drawable, texture)),
        })
    }
}

/// One acquired drawable, from acquire to present or discard.
#[derive(Debug)]
pub(crate) struct Frame<'sc> {
    /// +1 on the `CAMetalDrawable` (retained out of the acquire pool).
    drawable: Obj,
    /// +1 on the drawable's texture — the colour attachment this frame
    /// renders into.
    texture: Obj,
    /// The exclusive borrow that is the frame boundary.
    swapchain: &'sc mut Swapchain,
}

impl Frame<'_> {
    /// The drawable's texture: the render-pass colour attachment (and, when the layer is not `framebufferOnly`, a legal blit source for readback).
    #[inline]
    pub(crate) const fn texture(&self) -> &Obj {
        &self.texture
    }

    /// The drawable's texture as a sealed render target stamped with this swapchain's loss domain, the only form the encoder's write verbs accept.
    pub(crate) fn render_target(&self) -> super::resources::SealedTexture {
        // SAFETY: retaining a live +1 texture; the sealed handle releases its own +1 on drop, independently of this frame's.
        let obj =
            unsafe { Obj::retain(self.texture.id()) }.expect("retain of a live drawable texture");
        super::resources::SealedTexture::from_parts(obj, Arc::clone(&self.swapchain.latch))
    }

    /// The swapchain this frame came from (for format/extent queries mid-frame).
    #[cfg(test)]
    pub(crate) fn swapchain(&self) -> &Swapchain {
        self.swapchain
    }

    /// `-[CAMetalDrawable layer]`, the layer this drawable came from; the stacking test compares it with [`Swapchain::layer_ptr`].
    #[cfg(test)]
    pub(crate) fn drawable_layer_ptr(&self) -> Id {
        // SAFETY: object getter (+0 borrow, compared by pointer only).
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            f(self.drawable.id(), sel(c"layer"))
        }
    }

    /// Present the frame: a fresh command buffer from the session's queue with `presentDrawable:` registered before `commit` (wgpu-hal's shape).
    pub(crate) fn present(self, session: &encoder::EncodeSession) -> Result<PresentTicket, String> {
        // The loss may land between acquire and present. Refuse before touching
        // the queue; dropping `self` discards the drawable.
        if let Some(reason) = self.swapchain.latch.reason() {
            return Err(format!(
                "present refused: the device-loss latch is set ({reason}); \
                 the frame is discarded, not committed to the dead queue"
            ));
        }
        if !Arc::ptr_eq(session.latch(), &self.swapchain.latch) {
            return Err(
                "present refused: the encode session's loss latch is not this \
                 swapchain's latch — one loss domain per present path"
                    .to_owned(),
            );
        }
        #[cfg(test)]
        encoder::queue_ffi_census::note();
        let _pool = AutoreleasePool::new();
        // SAFETY: `commandBuffer` returns a +0 buffer, retained into the ticket so callers can wait after the pool pops. `presentDrawable:` then `commit`, in that order.
        unsafe {
            let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let raw = get(session.one_shot_queue().id(), sel(c"commandBuffer"));
            let cb = Obj::retain(raw)
                .ok_or_else(|| "commandBuffer returned nil for the present".to_owned())?;
            if crate::present_glass::sink_installed() {
                register_presented_handler(self.drawable.id());
            }
            let present: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
            present(cb.id(), sel(c"presentDrawable:"), self.drawable.id());
            let commit: unsafe extern "C-unwind" fn(Id, Sel) = checked_msg();
            commit(cb.id(), sel(c"commit"));
            Ok(PresentTicket {
                cb,
                latch: Arc::clone(&self.swapchain.latch),
                settled: std::cell::Cell::new(None),
            })
        }
    }
}

/// # Safety
/// `drawable` must be a live `id<MTLDrawable>` not yet presented.
unsafe fn register_presented_handler(drawable: Id) {
    // SAFETY: reading a monotonic clock.
    let registered_s = unsafe { CACurrentMediaTime() };
    let (serial, tag) = crate::present_glass::registration_begin(registered_s);
    let handler = move |presented: Id| {
        // SAFETY: Metal passes the live drawable being reported; `presentedTime` is a `CFTimeInterval` (double) getter, 0 when the drawable was never shown.
        let presented_s = unsafe {
            let get: unsafe extern "C-unwind" fn(Id, Sel) -> f64 = checked_msg();
            get(presented, sel(c"presentedTime"))
        };
        crate::present_glass::deliver_presented(serial, tag, registered_s, presented_s);
    };
    // SAFETY: `MTLDrawablePresentedHandler` is `void (^)(id<MTLDrawable>)`, exactly `(Id) -> ()`, and the closure captures only `Copy` values.
    let block = unsafe { aterm_objc::RcBlock::new1(handler) };
    let Some(block) = block else { return };
    // SAFETY: `addPresentedHandler:` copies the block, so ours is released independently when `block` drops.
    unsafe {
        let add: unsafe extern "C-unwind" fn(Id, Sel, aterm_objc::BlockPtr) = checked_msg();
        add(drawable, sel(c"addPresentedHandler:"), block.as_block_ptr());
    }
}

/// The committed present's command buffer, retained so its status can be awaited and classified, plus the latch handle so the classification always reaches the latch.
#[derive(Debug)]
pub(crate) struct PresentTicket {
    cb: Obj,
    latch: Arc<loss::LossLatch>,
    /// The classified terminal outcome once one exists: the first terminal
    /// wait or poll feeds the latch and fills this; later ones answer from it
    /// (as in [`encoder::Submitted`]).
    settled: std::cell::Cell<Option<loss::CbOutcome>>,
}

impl PresentTicket {
    /// Block until the present's command buffer completes, classify it via [`loss::outcome_of`] and feed the latch.
    pub(crate) fn wait_outcome(&self) -> loss::CbOutcome {
        if let Some(done) = self.settled.get() {
            return done;
        }
        // SAFETY: `waitUntilCompleted` is a void message on the owned +1 command buffer.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) = checked_msg();
            f(self.cb.id(), sel(c"waitUntilCompleted"));
        }
        self.settle_terminal(loss::outcome_of(self.cb.id()))
    }

    /// Poll: `None` while in flight, `Some(outcome)` once terminal.
    pub(crate) fn try_outcome(&self) -> Option<loss::CbOutcome> {
        if let Some(done) = self.settled.get() {
            return Some(done);
        }
        match loss::outcome_of(self.cb.id()) {
            loss::CbOutcome::Unfinished { .. } => None,
            terminal => Some(self.settle_terminal(terminal)),
        }
    }

    fn settle_terminal(&self, outcome: loss::CbOutcome) -> loss::CbOutcome {
        self.settled.set(Some(outcome));
        self.settle(outcome)
    }

    /// Record one classified outcome on the process latch and return it.
    pub(crate) fn settle(&self, outcome: loss::CbOutcome) -> loss::CbOutcome {
        self.latch.record(&outcome);
        outcome
    }
}

#[cfg(test)]
#[path = "swapchain_tests.rs"]
mod tests;
