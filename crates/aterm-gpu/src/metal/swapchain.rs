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
                     layer accepts the BGRA8/f16/XR set, and pick_surface_format's \
                     Rgba8Unorm fallback exists for non-Metal backends only"
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
        if let Some(reason) = self.latch.reason() {
            return Err(format!(
                "acquire refused: the device-loss latch is set ({reason}); \
                 not touching the layer or its queue — downgrade instead"
            ));
        }
        // SAFETY: reading a monotonic clock.
        let started = unsafe { CACurrentMediaTime() };
        match self.raw_next_drawable() {
            Some((drawable, texture)) => Ok(Frame {
                drawable,
                texture,
                swapchain: self,
            }),
            None => {
                // SAFETY: reading a monotonic clock.
                let waited = unsafe { CACurrentMediaTime() } - started;
                Err(format!(
                    "nextDrawable returned nil after {waited:.2}s — all \
                     {} drawables in flight (or an invalid layer configuration); \
                     the bounded timeout is allowsNextDrawableTimeout=YES doing \
                     its job instead of parking the thread",
                    self.maximum_drawable_count(),
                ))
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
        crate::present_glass::deliver_report(
            serial,
            tag,
            crate::present_glass::classify(registered_s, presented_s),
        );
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
mod tests {
    use super::*;
    use crate::metal::ffi;
    use crate::metal::test_support::{
        C_STRING_HOLES, CellUniforms, as_bytes, device, ns_string_of_units, plain_calayer,
        sublayer_count,
    };

    #[test]
    fn a_colorspace_name_is_read_whole() {
        let _pool = ffi::AutoreleasePool::new();
        // SAFETY: each `NSString` is live for its read and toll-free bridged to `CFString`.
        let read = C_STRING_HOLES
            .map(|(units, _)| unsafe { cf_string_text(ns_string_of_units(units).id().as_ptr()) });
        assert_eq!(read, C_STRING_HOLES.map(|(_, want)| Some(want.to_owned())));
        // SAFETY: NULL is an accepted input.
        assert_eq!(unsafe { cf_string_text(std::ptr::null()) }, None);
    }

    #[test]
    fn the_config_rejects_what_the_layer_would_throw_on() {
        let ok = SwapchainConfig::aterm_present(PixelFormat::Bgra8Unorm, 16, 16);
        assert!(ok.validate().is_ok());

        let mut bad = ok;
        bad.format = PixelFormat::Rgba8Unorm;
        let err = bad
            .validate()
            .expect_err("Rgba8Unorm is not a layer format");
        assert!(err.contains("Rgba8Unorm"), "got: {err}");

        for n in [0, 1, 4] {
            let mut bad = ok;
            bad.maximum_drawables = n;
            let err = bad.validate().expect_err("out-of-range drawable count");
            assert!(err.contains("[2, 3]"), "got: {err}");
        }

        let mut bad = ok;
        bad.width = 0;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn a_standalone_swapchain_has_no_parent_to_adopt_a_scale_from() {
        let dev = device();
        let _test_pool = AutoreleasePool::new();
        let config = SwapchainConfig {
            format: PixelFormat::Bgra8Unorm,
            width: 8,
            height: 8,
            framebuffer_only: false,
            display_sync: false,
            maximum_drawables: 2,
            opaque: true,
        };
        let mut sc = Swapchain::standalone(&dev, &config, Arc::new(loss::LossLatch::new()))
            .expect("standalone swapchain");
        sc.sync_layer_geometry();
        assert!(
            sc.contents_scale() > 0.0,
            "the standalone layer keeps its own default scale"
        );
        assert!(sc.acquire().is_ok(), "and still vends after the no-op sync");
    }

    /// The 112-byte `Blit` uniform (`shaders/blit.metal`'s `struct Blit`) as
    /// `repr(C)`; size checked against
    /// [`crate::metal::blit::UNIFORM_BYTES`] at the fill site.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct BlitUniformBytes {
        flag: u32,
        overlay: u32,
        border_px: f32,
        encode_srgb: f32,
        accent: [f32; 3],
        chrome_y1: f32,
        dims: [f32; 2],
        wash_a: f32,
        border_a: f32,
        band: [f32; 4],
        content_off: [f32; 2],
        hdr: f32,
        translucent: f32,
        sdr_white_scale: f32,
        visible_y: f32,
        visible_h: f32,
        premult: f32,
        chrome_y0: f32,
        chrome_pad: [f32; 3],
    }

    #[test]
    fn an_injected_loss_latches_and_every_later_acquire_and_present_refuses_fast() {
        use crate::metal::loss::{
            CbOutcome, ERROR_PAGE_FAULT, ERROR_TIMEOUT, LossLatch, STATUS_ERROR, classify,
        };

        let dev = device();
        let _test_pool = AutoreleasePool::new();
        let latch = Arc::new(LossLatch::new());
        let session = encoder::EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");
        let config = SwapchainConfig {
            format: PixelFormat::Bgra8Unorm,
            width: 8,
            height: 8,
            framebuffer_only: false,
            display_sync: false,
            maximum_drawables: 2,
            opaque: true,
        };
        let mut sc = Swapchain::standalone(&dev, &config, Arc::clone(&latch)).expect("swapchain");

        let calls = encoder::queue_ffi_census::calls();
        let ticket = sc
            .acquire()
            .expect("healthy acquire")
            .present(&session)
            .expect("healthy present");
        assert_eq!(
            encoder::queue_ffi_census::calls(),
            calls + 2,
            "a healthy acquire + present is one nextDrawable and one commandBuffer"
        );
        assert_eq!(ticket.wait_outcome(), CbOutcome::Completed);
        assert!(!latch.is_lost(), "a completed present must not latch");

        #[cfg(debug_assertions)]
        let fed = latch.fed_count();
        assert_eq!(
            ticket.try_outcome(),
            Some(CbOutcome::Completed),
            "a waited ticket must poll terminal"
        );
        #[cfg(debug_assertions)]
        assert_eq!(
            latch.fed_count(),
            fed,
            "a poll after the wait answers from the settled cache and must \
             not inflate the per-present feed count"
        );

        let outcome = ticket.settle(classify(STATUS_ERROR, Some(ERROR_TIMEOUT)));
        assert!(matches!(outcome, CbOutcome::Retryable { .. }));
        assert!(!latch.is_lost(), "Retryable must never latch");
        drop(
            sc.acquire()
                .expect("a retryable failure must not stop acquire"),
        );

        let frame = sc.acquire().expect("acquired before the loss lands");
        let outcome = ticket.settle(classify(STATUS_ERROR, Some(ERROR_PAGE_FAULT)));
        assert!(matches!(outcome, CbOutcome::Lost { .. }));
        assert!(latch.is_lost(), "the injected PageFault must latch");

        let calls = encoder::queue_ffi_census::calls();
        let err = frame
            .present(&session)
            .expect_err("presenting after the loss must refuse");
        assert!(
            err.contains("present refused") && err.contains("PageFault"),
            "the refusal names itself and the first loss: {err}"
        );
        assert_eq!(
            encoder::queue_ffi_census::calls(),
            calls,
            "the present refusal minted a command buffer — it must not touch the queue"
        );

        let err = sc
            .reconfigure(&dev, &config)
            .expect_err("reconfigure after the loss must refuse");
        assert!(
            err.contains("reconfigure refused") && err.contains("PageFault"),
            "the refusal names the first loss: {err}"
        );

        for attempt in 0..2 {
            let calls = encoder::queue_ffi_census::calls();
            let err = sc
                .acquire()
                .expect_err("acquire after the loss must refuse");
            assert!(
                err.contains("acquire refused") && err.contains("PageFault"),
                "attempt {attempt}: the refusal names the first loss: {err}"
            );
            assert_eq!(
                encoder::queue_ffi_census::calls(),
                calls,
                "attempt {attempt}: the refusal called nextDrawable — against a \
                 dead queue that eats ~1s per frame"
            );
        }
    }

    #[test]
    fn a_reconfigure_storm_lands_every_axis_live_and_the_pool_follows() {
        use crate::metal::encoder::EncodeSession;
        use crate::metal::loss::{CbOutcome, LossLatch};

        let dev = device();
        let _test_pool = AutoreleasePool::new();
        let latch = Arc::new(LossLatch::new());
        let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");

        let base = SwapchainConfig {
            format: PixelFormat::Bgra8Unorm,
            width: 16,
            height: 16,
            framebuffer_only: false,
            display_sync: false,
            maximum_drawables: 3,
            opaque: true,
        };
        let mut sc = Swapchain::standalone(&dev, &base, Arc::clone(&latch)).expect("swapchain");

        let steps: Vec<SwapchainConfig> = vec![
            SwapchainConfig {
                width: 32,
                height: 8,
                ..base
            },
            SwapchainConfig {
                format: PixelFormat::Rgba16Float,
                width: 32,
                height: 8,
                ..base
            },
            SwapchainConfig {
                format: PixelFormat::Bgra8Unorm,
                opaque: false,
                ..base
            },
            SwapchainConfig {
                framebuffer_only: true,
                display_sync: true,
                maximum_drawables: 2,
                ..base
            },
            SwapchainConfig {
                format: PixelFormat::Rgba16Float,
                width: 8,
                height: 32,
                opaque: false,
                ..base
            },
            base,
        ];

        for cycle in 0..2 {
            for (i, cfg) in steps.iter().enumerate() {
                sc.reconfigure(&dev, cfg)
                    .unwrap_or_else(|e| panic!("cycle {cycle} step {i}: reconfigure: {e}"));

                assert_eq!(
                    PixelFormat::from_raw(sc.raw_pixel_format()),
                    Some(cfg.format),
                    "cycle {cycle} step {i}: pixelFormat"
                );
                #[expect(clippy::cast_precision_loss, reason = "small extents")]
                let want_size = (cfg.width as f64, cfg.height as f64);
                assert_eq!(
                    sc.drawable_size(),
                    want_size,
                    "cycle {cycle} step {i}: drawableSize"
                );
                assert_eq!(
                    sc.framebuffer_only(),
                    cfg.framebuffer_only,
                    "cycle {cycle} step {i}: framebufferOnly"
                );
                assert_eq!(
                    sc.display_sync_enabled(),
                    cfg.display_sync,
                    "cycle {cycle} step {i}: displaySyncEnabled"
                );
                assert_eq!(
                    sc.maximum_drawable_count(),
                    cfg.maximum_drawables,
                    "cycle {cycle} step {i}: maximumDrawableCount"
                );
                assert_eq!(sc.is_opaque(), cfg.opaque, "cycle {cycle} step {i}: opaque");
                let wants_edr = cfg.format == PixelFormat::Rgba16Float;
                assert_eq!(
                    sc.wants_extended_dynamic_range(),
                    wants_edr,
                    "cycle {cycle} step {i}: wantsEDR"
                );
                assert_eq!(
                    sc.colorspace_name().as_deref(),
                    Some(if wants_edr {
                        "kCGColorSpaceExtendedLinearSRGB"
                    } else {
                        "kCGColorSpaceSRGB"
                    }),
                    "cycle {cycle} step {i}: colorspace after the flip"
                );

                // SAFETY: reading a monotonic clock.
                let t0 = unsafe { CACurrentMediaTime() };
                let frame = sc
                    .acquire()
                    .unwrap_or_else(|e| panic!("cycle {cycle} step {i}: acquire: {e}"));
                // SAFETY: reading a monotonic clock.
                let waited = unsafe { CACurrentMediaTime() } - t0;
                assert!(
                    waited < 60.0,
                    "cycle {cycle} step {i}: acquire took {waited:.2}s — the storm \
                     starved the pool"
                );
                assert_eq!(
                    (
                        ffi::texture_width(frame.texture()),
                        ffi::texture_height(frame.texture()),
                    ),
                    (cfg.width, cfg.height),
                    "cycle {cycle} step {i}: the vended drawable must carry the NEW \
                     extent — the pool did not rebuild"
                );
                assert_eq!(
                    PixelFormat::from_raw(ffi::texture_pixel_format_raw(frame.texture())),
                    Some(cfg.format),
                    "cycle {cycle} step {i}: the vended drawable must carry the NEW \
                     format — the pool did not rebuild"
                );

                let outcome = frame
                    .present(&session)
                    .unwrap_or_else(|e| panic!("cycle {cycle} step {i}: present: {e}"))
                    .wait_outcome();
                assert_eq!(
                    outcome,
                    CbOutcome::Completed,
                    "cycle {cycle} step {i}: the post-reconfigure present completes"
                );
                assert!(!latch.is_lost(), "cycle {cycle} step {i}: no loss");
            }
        }
        crate::stderr_line!(
            "reconfigure storm on {}: 12 live reconfigures over 6 axes, every axis \
             read back, every post-step drawable vended with the new config, every \
             present clean",
            dev.name()
        );
    }

    #[test]
    fn six_two_submit_frames_encode_compose_present_and_verify() {
        use crate::metal::encoder::{
            CommandBuffer, EncodeSession, RenderPassDesc, StoreAction, Submitted,
        };
        use crate::metal::loss::{CbOutcome, LossLatch};
        use crate::pipeline_table::Pipeline;

        let dev = device();
        let _test_pool = AutoreleasePool::new();

        const W: usize = 16;
        const H: usize = 16;
        const FRAMES: usize = 6;

        let latch = Arc::new(LossLatch::new());
        let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");
        let mut sc = Swapchain::standalone(
            &dev,
            &SwapchainConfig {
                format: PixelFormat::Bgra8Unorm,
                width: W,
                height: H,
                framebuffer_only: false,
                display_sync: false,
                maximum_drawables: 3,
                opaque: true,
            },
            Arc::clone(&latch),
        )
        .expect("swapchain");

        let bg_spec = Pipeline::Bg.spec();
        let bg_lib = crate::metal::pipelines::compile(&dev, bg_spec).expect("cell.metal");
        let bg_pso =
            crate::metal::pipelines::build(&dev, &bg_lib, bg_spec, PixelFormat::Bgra8Unorm)
                .expect("bg row");
        let blit_spec = Pipeline::Blit.spec();
        let blit_lib = crate::metal::pipelines::compile(&dev, blit_spec).expect("blit.metal");
        let blit_pso =
            crate::metal::pipelines::build(&dev, &blit_lib, blit_spec, PixelFormat::Bgra8Unorm)
                .expect("blit row");

        let mint = crate::metal::resources::MetalResourceDevice::new(&dev, Arc::clone(&latch));
        let offscreen = mint
            .texture_2d(
                PixelFormat::Rgba8Unorm,
                W,
                H,
                ffi::TEXTURE_USAGE_RENDER_TARGET
                    | ffi::TEXTURE_USAGE_SHADER_READ
                    | ffi::TEXTURE_USAGE_PIXEL_FORMAT_VIEW,
            )
            .expect("offscreen");
        let srgb_view = offscreen
            .alias_view(PixelFormat::Rgba8UnormSrgb)
            .expect("sRGB view");
        let sampler = dev
            .new_sampler(ffi::SamplerDesc::NEAREST_CLAMP)
            .expect("sampler");
        let ibuf = dev.new_buffer(2 * 12).expect("instance stream");
        let cell_ubuf = dev
            .new_buffer(size_of::<CellUniforms>())
            .expect("cell uniforms");
        #[expect(clippy::cast_precision_loss, reason = "16-texel extents")]
        let cell_uniforms = CellUniforms {
            screen: [W as f32, H as f32],
            text_blend: 0.0,
            pad: 0.0,
        };
        // SAFETY: `repr(C)` write into an exactly-sized shared buffer.
        unsafe { ffi::buffer_write(&cell_ubuf, as_bytes(&cell_uniforms)) };

        assert_eq!(
            size_of::<BlitUniformBytes>(),
            crate::metal::blit::UNIFORM_BYTES,
            "the repr(C) restatement must be the 112 bytes blit.metal declares"
        );
        #[expect(clippy::cast_precision_loss, reason = "as above")]
        let blit_uniform = BlitUniformBytes {
            flag: 0,
            overlay: 0,
            border_px: 0.0,
            encode_srgb: 0.0,
            accent: [0.0; 3],
            chrome_y1: 0.0,
            dims: [W as f32, H as f32],
            wash_a: 0.0,
            border_a: 0.0,
            band: [1.0, 0.0, 0.0, 1.0],
            content_off: [0.0, 0.0],
            hdr: 0.0,
            translucent: 0.0,
            sdr_white_scale: 1.0,
            visible_y: 0.0,
            visible_h: H as f32,
            premult: 0.0,
            chrome_y0: 0.0,
            chrome_pad: [0.0; 3],
        };
        let blit_ubuf = dev
            .new_buffer(crate::metal::blit::UNIFORM_BYTES)
            .expect("blit uniform");
        // SAFETY: `repr(C)` write into a size-verified shared buffer.
        unsafe { ffi::buffer_write(&blit_ubuf, as_bytes(&blit_uniform)) };

        let present_row = W * PixelFormat::Bgra8Unorm.bytes_per_texel();
        let readback = dev.new_buffer(present_row * H).expect("drawable readback");
        let close = |g: u8, w: u8| (i16::from(g) - i16::from(w)).unsigned_abs() <= 1;

        for frame in 0..FRAMES {
            let shift = u8::try_from(10 * frame).expect("frame < 26");
            let c0: [u8; 4] = [200, 40 + shift, 120, 255];
            let c1: [u8; 4] = [40, 200 - shift, 90, 255];
            let mut stream: Vec<u8> = Vec::new();
            for (rect, colour) in [([0u16, 0, 8, 8], c0), ([8u16, 8, 8, 8], c1)] {
                for v in rect {
                    stream.extend_from_slice(&v.to_le_bytes());
                }
                stream.extend_from_slice(&colour);
            }
            // SAFETY: write into an exactly-sized shared buffer after previous frame completion.
            unsafe { ffi::buffer_write(&ibuf, &stream) };

            let mut cb_a: CommandBuffer<'_> = session.begin().expect("cb A");
            {
                let pass = cb_a
                    .render_pass(&RenderPassDesc {
                        target: &srgb_view,
                        load: ffi::LoadAction::Clear(ffi::ClearColor {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
                        }),
                        store: StoreAction::Store,
                        viewport: None,
                        scissor: None,
                    })
                    .expect("frame pass");
                pass.set_vertex_buffer(&cell_ubuf, 0)
                    .expect("uniforms at 0");
                pass.set_pipeline(&bg_pso);
                pass.set_instance_stream(&ibuf);
                pass.draw_instanced(
                    crate::metal::pipelines::metal_primitive_type(bg_spec.topology),
                    6,
                    2,
                )
                .expect("armed draw: pipeline and stream are set in this fixture");
            }
            let submit_a: Submitted = cb_a.commit();

            let frame_obj = sc
                .acquire()
                .unwrap_or_else(|e| panic!("frame {frame}: acquire failed: {e}"));

            let mut cb_b = session.begin().expect("cb B");
            {
                let pass = cb_b
                    .render_pass(&RenderPassDesc {
                        target: &frame_obj.render_target(),
                        load: ffi::LoadAction::Clear(ffi::ClearColor {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
                        }),
                        store: StoreAction::Store,
                        viewport: None,
                        scissor: None,
                    })
                    .expect("present pass");
                pass.set_pipeline(&blit_pso);
                pass.set_fragment_texture(offscreen.obj(), 0);
                pass.set_fragment_sampler(&sampler, 0);
                pass.set_fragment_buffer(&blit_ubuf, 2);
                pass.draw_fullscreen_triangle()
                    .expect("armed draw: pipeline and stream are set in this fixture");
            }
            cb_b.copy_texture_to_buffer(&frame_obj.render_target(), W, H, &readback, present_row)
                .expect("tap copy");
            #[cfg(debug_assertions)]
            let fed_before = latch.fed_count();
            let outcome_b = cb_b.commit().wait_outcome();
            assert_eq!(
                outcome_b,
                CbOutcome::Completed,
                "frame {frame}: submit B must complete"
            );
            assert_eq!(
                submit_a.try_outcome(),
                Some(CbOutcome::Completed),
                "frame {frame}: submit A must be terminal once B is (one queue, \
                 in order) — if this polls Unfinished, the two submits are not \
                 on one queue and the present ordering argument is void"
            );
            #[cfg(debug_assertions)]
            assert_eq!(
                latch.fed_count(),
                fed_before + 2,
                "frame {frame}: B's wait and A's terminal poll each feed the \
                 latch exactly once"
            );

            // SAFETY: shared storage read after command buffer completion.
            let got = unsafe { ffi::buffer_bytes(&readback, present_row * H) };
            for (i, texel) in got.as_chunks::<4>().0.iter().enumerate() {
                let (px, py) = (i % W, i / W);
                let want_rgba = if px < 8 && py < 8 {
                    c0
                } else if px >= 8 && py >= 8 {
                    c1
                } else {
                    [0, 0, 0, 255]
                };
                let want = [want_rgba[2], want_rgba[1], want_rgba[0], 255];
                assert!(
                    texel.iter().zip(want).all(|(g, w)| close(*g, w)),
                    "frame {frame} ({px},{py}): got {texel:?}, want ~{want:?} (BGRA) — \
                     a stale drawable replays an old frame's colours here, an \
                     unordered submit pair blits a half-rendered offscreen"
                );
            }

            #[cfg(debug_assertions)]
            let fed_before_present = latch.fed_count();
            let outcome = frame_obj
                .present(&session)
                .unwrap_or_else(|e| panic!("frame {frame}: present failed: {e}"))
                .wait_outcome();
            assert_eq!(outcome, CbOutcome::Completed, "frame {frame}: present");
            #[cfg(debug_assertions)]
            assert_eq!(
                latch.fed_count(),
                fed_before_present + 1,
                "frame {frame}: the present wait feeds the latch"
            );
            assert!(
                !latch.is_lost(),
                "frame {frame}: healthy frames never latch"
            );
        }
        crate::stderr_line!(
            "two-submit cycle on {}: {FRAMES} frames of encode-submit + \
             compose-submit + present on ONE session queue, byte-verified, \
             A harvested by polling every frame",
            dev.name()
        );
    }

    #[test]
    fn the_two_latch_cross_wired_present_has_no_remaining_shape() {
        use crate::metal::encoder::{EncodeSession, RenderPassDesc, StoreAction};
        use crate::metal::loss::LossLatch;
        use crate::metal::resources::MetalResourceDevice;

        let dev = device();
        let _test_pool = AutoreleasePool::new();

        let latch1 = Arc::new(LossLatch::new());
        let session1 = EncodeSession::new(&dev, Arc::clone(&latch1)).expect("session 1");
        let mint1 = MetalResourceDevice::new(&dev, Arc::clone(&latch1));
        let mut sc = Swapchain::standalone(
            &dev,
            &SwapchainConfig {
                format: PixelFormat::Bgra8Unorm,
                width: 8,
                height: 8,
                framebuffer_only: false,
                display_sync: false,
                maximum_drawables: 2,
                opaque: true,
            },
            Arc::clone(&latch1),
        )
        .expect("swapchain");

        let latch2 = Arc::new(LossLatch::new());
        let session2 = EncodeSession::new(&dev, Arc::clone(&latch2)).expect("session 2");
        let mint2 = MetalResourceDevice::new(&dev, Arc::clone(&latch2));
        let rogue_offscreen = mint2
            .texture_2d(
                PixelFormat::Bgra8Unorm,
                8,
                8,
                ffi::TEXTURE_USAGE_RENDER_TARGET | ffi::TEXTURE_USAGE_SHADER_READ,
            )
            .expect("rogue offscreen");

        let frame = sc.acquire().expect("acquire");
        let frame_rt = frame.render_target();

        let mut cb2 = session2.begin().expect("cb on session 2");
        let err = cb2
            .render_pass(&RenderPassDesc {
                target: &frame_rt,
                load: ffi::LoadAction::Clear(ffi::ClearColor {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                }),
                store: StoreAction::Store,
                viewport: None,
                scissor: None,
            })
            .expect_err("a foreign-domain render target must refuse");
        assert!(
            err.contains("loss domain"),
            "the refusal names the loss domain: {err}"
        );

        let mut cb1 = session1.begin().expect("cb on session 1");
        let err = cb1
            .copy_texture_to_texture(&rogue_offscreen, &frame_rt)
            .expect_err("a foreign-domain copy source must refuse");
        assert!(err.contains("loss domain"), "got: {err}");
        let rb = dev.new_buffer(8 * 8 * 4).expect("rb");
        let err = cb2
            .copy_texture_to_buffer(&frame_rt, 8, 8, &rb, 8 * 4)
            .expect_err("a foreign-domain readback source must refuse");
        assert!(err.contains("loss domain"), "got: {err}");

        let control = mint1
            .texture_2d(
                PixelFormat::Bgra8Unorm,
                8,
                8,
                ffi::TEXTURE_USAGE_RENDER_TARGET | ffi::TEXTURE_USAGE_SHADER_READ,
            )
            .expect("control offscreen");
        cb1.render_pass(&RenderPassDesc {
            target: &frame_rt,
            load: ffi::LoadAction::Clear(ffi::ClearColor {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            }),
            store: StoreAction::Store,
            viewport: None,
            scissor: None,
        })
        .expect("same-domain render pass");
        cb1.copy_texture_to_texture(&control, &frame_rt)
            .expect("same-domain copy");
        assert_eq!(
            cb1.commit().wait_outcome(),
            crate::metal::loss::CbOutcome::Completed,
            "the control encode completes"
        );

        let err = frame
            .present(&session2)
            .expect_err("the cross-wired present must refuse");
        assert!(
            err.contains("loss domain") || err.contains("latch"),
            "the refusal names the wiring: {err}"
        );
        assert_eq!(
            cb2.commit().wait_outcome(),
            crate::metal::loss::CbOutcome::Completed,
            "every rogue crossing was refused BEFORE encoding — the rogue \
             command buffer is empty and completes clean"
        );
        assert!(
            !latch1.is_lost() && !latch2.is_lost(),
            "cross-wiring is a wiring error, not a device loss"
        );
    }

    fn foreign_metal_layer(dev: &Device, w: usize, h: usize) -> Obj {
        let _pool = AutoreleasePool::new();
        // SAFETY: `alloc`/`init` is +1 and property setters use documented types.
        unsafe {
            let alloc: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id = checked_msg();
            let raw = alloc(class(c"CAMetalLayer"), sel(c"alloc"));
            let init: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let layer = Obj::from_owned(init(raw, sel(c"init"))).expect("CAMetalLayer init");
            let set_obj: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
            set_obj(layer.id(), sel(c"setDevice:"), dev.id());
            let set_size: unsafe extern "C-unwind" fn(Id, Sel, CgSize) = checked_msg();
            #[expect(
                clippy::cast_precision_loss,
                reason = "16-texel extents are exact in f64"
            )]
            set_size(
                layer.id(),
                sel(c"setDrawableSize:"),
                CgSize {
                    width: w as f64,
                    height: h as f64,
                },
            );
            let set_bool: unsafe extern "C-unwind" fn(Id, Sel, ObjcBool) = checked_msg();
            set_bool(layer.id(), sel(c"setFramebufferOnly:"), false.into());
            layer
        }
    }

    #[test]
    fn a_present_beside_a_live_foreign_metal_layer_touches_only_its_own_layer() {
        use crate::pipeline_table::Pipeline;

        let dev = device();
        let _test_pool = AutoreleasePool::new();
        let latch = Arc::new(loss::LossLatch::new());
        let session = encoder::EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");
        let queue = session.one_shot_queue();

        const W: usize = 16;
        const H: usize = 16;
        let parent = plain_calayer();
        let foreign = foreign_metal_layer(&dev, W, H);
        // SAFETY: `addSublayer:` retains `foreign` while both objects are live.
        unsafe {
            let add: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
            add(parent.id(), sel(c"addSublayer:"), foreign.id());
        }

        let blit_spec = Pipeline::Blit.spec();
        let blit_lib = crate::metal::pipelines::compile(&dev, blit_spec).expect("blit.metal");
        let blit_bgra =
            crate::metal::pipelines::build(&dev, &blit_lib, blit_spec, PixelFormat::Bgra8Unorm)
                .expect("blit->bgra");
        let blit_rgba =
            crate::metal::pipelines::build(&dev, &blit_lib, blit_spec, PixelFormat::Rgba8Unorm)
                .expect("blit->rgba");
        let sampler = dev
            .new_sampler(ffi::SamplerDesc::NEAREST_CLAMP)
            .expect("sampler");
        let blit_ubuf = dev
            .new_buffer(crate::metal::blit::UNIFORM_BYTES)
            .expect("blit uniform");
        #[expect(clippy::cast_precision_loss, reason = "16-texel extents")]
        let blit_uniform = BlitUniformBytes {
            flag: 0,
            overlay: 0,
            border_px: 0.0,
            encode_srgb: 0.0,
            accent: [0.0; 3],
            chrome_y1: 0.0,
            dims: [W as f32, H as f32],
            wash_a: 0.0,
            border_a: 0.0,
            band: [1.0, 0.0, 0.0, 1.0],
            content_off: [0.0, 0.0],
            hdr: 0.0,
            translucent: 0.0,
            sdr_white_scale: 1.0,
            visible_y: 0.0,
            visible_h: H as f32,
            premult: 0.0,
            chrome_y0: 0.0,
            chrome_pad: [0.0; 3],
        };
        // SAFETY: `repr(C)` write into a size-verified shared buffer.
        unsafe { ffi::buffer_write(&blit_ubuf, as_bytes(&blit_uniform)) };

        const OURS: [u8; 4] = [10, 200, 60, 255];
        const THEIRS: [u8; 4] = [255, 128, 0, 255];
        let make_src = |c: [u8; 4]| {
            let tex = dev
                .new_texture_2d(
                    PixelFormat::Rgba8Unorm,
                    W,
                    H,
                    ffi::TEXTURE_USAGE_SHADER_READ,
                )
                .expect("solid src");
            let bytes: Vec<u8> = c.iter().copied().cycle().take(W * H * 4).collect();
            // SAFETY: upload into a non-private texture with matching extent and stride.
            unsafe { ffi::texture_upload(&tex, ffi::MtlRegion::full_2d(W, H), &bytes, W * 4) };
            tex
        };
        let ours_src = make_src(OURS);
        let theirs_src = make_src(THEIRS);

        let row = W * PixelFormat::Bgra8Unorm.bytes_per_texel();
        let readback = dev.new_buffer(row * H).expect("readback");

        // SAFETY: `nextDrawable` and `texture` are retained inside the autorelease pool.
        let (foreign_drawable, foreign_tex) = unsafe {
            let _pool = AutoreleasePool::new();
            let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let d = Obj::retain(get(foreign.id(), sel(c"nextDrawable")))
                .expect("the foreign layer vends");
            let t = Obj::retain(get(d.id(), sel(c"texture"))).expect("its texture");
            (d, t)
        };
        let blit_into = |dst: &Obj, src: &Obj, pso: &Obj| {
            ffi::draw_and_read(
                queue,
                &ffi::Pass {
                    pso,
                    dst,
                    dst_w: W,
                    dst_h: H,
                    load: ffi::LoadAction::Clear(ffi::ClearColor {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    }),
                    viewport: None,
                    scissor: None,
                    src_tex: Some((src, blit_spec.binds.fragment_textures[0] as usize)),
                    sampler: Some((&sampler, blit_spec.binds.fragment_samplers[0] as usize)),
                    uniform: Some((&blit_ubuf, blit_spec.binds.fragment_buffers[0] as usize)),
                    vertex_uniform: None,
                    draw: None,
                },
                &readback,
                row,
            )
        };
        blit_into(&foreign_tex, &theirs_src, &blit_bgra).expect("foreign render");

        let config = SwapchainConfig {
            format: PixelFormat::Bgra8Unorm,
            width: W,
            height: H,
            framebuffer_only: false,
            display_sync: false,
            maximum_drawables: 2,
            opaque: true,
        };
        {
            let mut sc = Swapchain::attached(&dev, &parent, &config, Arc::clone(&latch))
                .expect("attached beside a live sibling");
            assert_eq!(
                sublayer_count(&parent),
                2,
                "attach ADDs beside the leftover"
            );

            let frame = sc.acquire().expect("acquire beside a live sibling");
            let came_from = frame.drawable_layer_ptr();
            assert_eq!(
                came_from,
                frame.swapchain().layer_ptr(),
                "the drawable must come from the swapchain's own layer"
            );
            assert_ne!(came_from, foreign.id(), "never the foreign layer's");
            assert_ne!(came_from, parent.id(), "never the parent's");
            assert_ne!(
                frame.texture().id(),
                foreign_tex.id(),
                "our attachment is not the sibling's texture"
            );

            blit_into(frame.texture(), &ours_src, &blit_bgra).expect("our render");
            // SAFETY: shared storage read after `draw_and_read` completed.
            let got = unsafe { ffi::buffer_bytes(&readback, row * H) };
            let want = [OURS[2], OURS[1], OURS[0], 255];
            for (i, texel) in got.as_chunks::<4>().0.iter().enumerate() {
                assert_eq!(
                    texel, &want,
                    "texel {i}: our drawable must hold OUR bytes (BGRA)"
                );
            }

            let outcome = frame.present(&session).expect("present").wait_outcome();
            assert_eq!(outcome, crate::metal::loss::CbOutcome::Completed);
            assert!(!latch.is_lost());

            let scratch = dev
                .new_texture_2d(
                    PixelFormat::Rgba8Unorm,
                    W,
                    H,
                    ffi::TEXTURE_USAGE_RENDER_TARGET | ffi::TEXTURE_USAGE_SHADER_READ,
                )
                .expect("scratch");
            blit_into(&scratch, &foreign_tex, &blit_rgba).expect("foreign readback");
            // SAFETY: shared storage read after `draw_and_read` completed.
            let got = unsafe { ffi::buffer_bytes(&readback, row * H) };
            for (i, texel) in got.as_chunks::<4>().0.iter().enumerate() {
                assert_eq!(
                    texel, &THEIRS,
                    "texel {i}: the foreign in-flight drawable must still hold \
                     the sibling's bytes after our render + present (RGBA)"
                );
            }
        }
        assert_eq!(sublayer_count(&parent), 1, "drop unparents ours alone");
        assert_eq!(
            first_sublayer(&parent),
            foreign.id(),
            "the survivor is the foreign layer"
        );
        drop(foreign_drawable);
    }

    fn first_sublayer(layer: &Obj) -> Id {
        let _pool = AutoreleasePool::new();
        // SAFETY: `sublayers` answers a +0 NSArray or nil and `objectAtIndex:` a +0 element, which callers only compare by pointer.
        unsafe {
            let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let arr = get(layer.id(), sel(c"sublayers"));
            if arr.is_null() {
                return Id::NIL;
            }
            let at: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = checked_msg();
            at(arr, sel(c"objectAtIndex:"), 0)
        }
    }

    #[test]
    fn a_present_ticket_feeds_the_latch_exactly_once_however_asked() {
        use crate::metal::loss::{CbOutcome, LossLatch};

        let dev = device();
        let _test_pool = AutoreleasePool::new();
        let latch = Arc::new(LossLatch::new());
        let session = encoder::EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");
        let mut sc = Swapchain::standalone(
            &dev,
            &SwapchainConfig {
                format: PixelFormat::Bgra8Unorm,
                width: 16,
                height: 16,
                framebuffer_only: true,
                display_sync: false,
                maximum_drawables: 3,
                opaque: true,
            },
            Arc::clone(&latch),
        )
        .expect("swapchain");

        let ticket = sc
            .acquire()
            .expect("frame")
            .present(&session)
            .expect("present");
        #[cfg(debug_assertions)]
        let before = latch.fed_count();
        assert_eq!(ticket.wait_outcome(), CbOutcome::Completed);
        assert_eq!(ticket.try_outcome(), Some(CbOutcome::Completed));
        assert_eq!(ticket.wait_outcome(), CbOutcome::Completed);
        #[cfg(debug_assertions)]
        assert_eq!(
            latch.fed_count(),
            before + 1,
            "one present, one feed — later waits and polls answer from the \
             settled cache (the uncached shape re-fed on every terminal ask)"
        );
        #[cfg(debug_assertions)]
        {
            let seam_before = latch.fed_count();
            let _ = ticket.settle(CbOutcome::Completed);
            assert_eq!(
                latch.fed_count(),
                seam_before + 1,
                "the injection seam stays uncached"
            );
        }
        assert!(!latch.is_lost());
    }

    #[test]
    fn a_present_delivers_a_glass_sample_once_a_sink_listens() {
        use crate::metal::loss::{CbOutcome, LossLatch};
        use crate::present_glass::{GlassReport, delivered, install_sink};

        fn sink(_: GlassReport) {}

        let dev = device();
        let _test_pool = AutoreleasePool::new();
        let _ = install_sink(sink);
        let latch = Arc::new(LossLatch::new());
        let session = encoder::EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");
        let mut sc = Swapchain::standalone(
            &dev,
            &SwapchainConfig {
                format: PixelFormat::Bgra8Unorm,
                width: 16,
                height: 16,
                framebuffer_only: true,
                display_sync: false,
                maximum_drawables: 3,
                opaque: true,
            },
            Arc::clone(&latch),
        )
        .expect("swapchain");

        const PRESENTS: u64 = 3;
        let before = delivered();
        for _ in 0..PRESENTS {
            let ticket = sc
                .acquire()
                .expect("frame")
                .present(&session)
                .expect("present");
            assert_eq!(ticket.wait_outcome(), CbOutcome::Completed);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while delivered() < before + PRESENTS && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            delivered() >= before + PRESENTS,
            "{PRESENTS} presents delivered {} presented-handler samples: the \
             compositor leg is unobserved",
            delivered() - before
        );
        assert!(!latch.is_lost());
    }

    #[test]
    fn attachment_parent_survives_its_callers_release_until_retirement() {
        let dev = device();
        let _test_pool = AutoreleasePool::new();
        let config = SwapchainConfig::aterm_present(PixelFormat::Bgra8Unorm, 16, 16);
        let (sc, parent_id) = {
            let _pool = AutoreleasePool::new();
            let parent = plain_calayer();
            let sc = Swapchain::attached(&dev, &parent, &config, Arc::new(loss::LossLatch::new()))
                .expect("attached swapchain");
            (sc, parent.id())
        };
        let child = sc.layer.clone_retained();
        // SAFETY: a scalar object-property read on the retained layer; the result is compared, never dereferenced or released.
        let parent_of = || unsafe {
            let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            get(child.id(), sel(c"superlayer"))
        };
        assert_eq!(
            parent_of(),
            parent_id,
            "dropping the parent retain is the negative mutant"
        );
        drop(sc);
        assert!(parent_of().is_null(), "retirement must detach the child");
    }

    #[test]
    fn worker_handoff_accepts_its_origin_and_rejects_same_size_foreign_layers() {
        let dev = device();
        let _test_pool = AutoreleasePool::new();
        let config = SwapchainConfig::aterm_present(PixelFormat::Bgra8Unorm, 16, 16);
        let latch = Arc::new(loss::LossLatch::new());
        let mut destination = Swapchain::standalone(&dev, &config, Arc::clone(&latch))
            .expect("destination swapchain");
        let destination_id = destination.layer_ptr();

        let own = destination
            .acquire_request()
            .run()
            .drawable
            .expect("same-layer drawable");
        let frame = destination
            .frame_from_acquired(own)
            .expect("the matching layer must remain admissible");
        assert_eq!(frame.drawable_layer_ptr(), destination_id);
        assert!(Arc::ptr_eq(frame.swapchain().latch(), &latch));
        drop(frame);

        for share_latch in [true, false] {
            let origin_latch = if share_latch {
                Arc::clone(&latch)
            } else {
                Arc::new(loss::LossLatch::new())
            };
            let origin = Swapchain::standalone(&dev, &config, origin_latch)
                .expect("foreign swapchain with identical configuration");
            let origin_id = origin.layer_ptr();
            assert_ne!(origin_id, destination_id);
            let foreign = origin
                .acquire_request()
                .run()
                .drawable
                .expect("foreign drawable");
            assert_eq!(foreign.source_layer.as_ref().map(Obj::id), Some(origin_id));
            drop(origin);
            let error = destination
                .frame_from_acquired(foreign)
                .expect_err("removing the origin guard must fail this negative control");
            assert!(error.contains("different layer/loss domain"), "{error}");
        }
        assert!(
            !latch.is_lost(),
            "rejection must not poison a healthy device"
        );
    }
}
