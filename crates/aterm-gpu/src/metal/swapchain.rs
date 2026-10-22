// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The swapchain: `CAMetalLayer`, reached by class name through the runtime.
//! The QuartzCore link and every drawable selector live here.
//!
//! A [`Swapchain`] is a `CAMetalLayer` configured like the wgpu surface it
//! replaces, either standalone (a layer with a `device` and `drawableSize`
//! vends drawables with no window; every test here uses this) or attached as a
//! sublayer of an existing `CALayer`. Attach never touches the parent's other
//! sublayers, so a sibling wgpu layer (which wgpu teardown leaves parented) is
//! tolerated.
//!
//! # Configured axes (compared with wgpu-hal's `metal/surface.rs`)
//!
//! | axis | this module | why |
//! |---|---|---|
//! | `device` | the [`Device`] handed in | drawables come off this device |
//! | `pixelFormat` | [`SwapchainConfig::format`] ([`super::pipelines::metal_format`]) | `Bgra8Unorm` (SDR) or `Rgba16Float` (EDR) |
//! | `drawableSize` | explicit `width`x`height` | never derived from bounds |
//! | `framebufferOnly` | config; `true` in `aterm_present` | production usage is `RENDER_ATTACHMENT`-only; the video tap flips it on demand; readback tests need `false` |
//! | `displaySyncEnabled` | config; `false` in `aterm_present` | the macOS present mode is `Immediate` |
//! | `maximumDrawableCount` | config; `3` in `aterm_present` | frame latency 2 + 1 (`renderer.rs` `default_frame_latency`); the legal range `[2, 3]` is validated in Rust |
//! | `opaque` | config; `true` in `aterm_present` | `NO` for the translucent present |
//! | `wantsExtendedDynamicRangeContent` | `format == Rgba16Float` | the HDR present |
//! | `colorspace` | SDR `kCGColorSpaceSRGB`, EDR `kCGColorSpaceExtendedLinearSRGB` | see below |
//! | `allowsNextDrawableTimeout` | `YES`, explicitly | wgpu-hal sets `NO`; see below |
//!
//! `CAMetalLayer` does not accept `Rgba8Unorm`, so [`SwapchainConfig`] admits
//! exactly `Bgra8Unorm` and `Rgba16Float` and rejects the rest in Rust rather
//! than letting the layer throw.
//!
//! `setPixelFormat:` installs sRGB on a fresh layer, but flipping a live layer
//! from f16 back to `Bgra8Unorm` keeps the extended space, and the compositor
//! misreads an SDR present tagged scRGB, so `configure` names both arms'
//! spaces. Headless tests pin the property; how WindowServer interprets it
//! needs a composited window.
//!
//! With `allowsNextDrawableTimeout = YES`, `nextDrawable` returns nil after
//! about a second instead of blocking forever, so [`Swapchain::acquire`] maps
//! nil to a bounded `Err` rather than parking the event loop. An unparented
//! layer's pool never exhausts, so headless tests reach the nil arm only with a
//! deviceless layer.
//!
//! # Ownership and the frame boundary
//!
//! `-nextDrawable` returns an autoreleased (+0) drawable; the acquire retains
//! it and its `texture` to +1 [`Obj`]s under its own pool. [`Frame`] holds
//! `&mut Swapchain`, so one drawable is live at a time; [`Frame::present`]
//! consumes it and dropping it discards the drawable back to the pool. A
//! forgotten or never-resolved frame costs a bounded, named acquire error, not
//! a hang.
//!
//! # The loss latch
//!
//! Every swapchain holds the process `Arc<`[`loss::LossLatch`]`>`. Acquire,
//! reconfigure and present refuse before any FFI once it is lost;
//! [`PresentTicket::wait_outcome`] feeds it (via [`PresentTicket::settle`], also
//! the test injection seam). `Retryable` outcomes do not latch.
//!
//! [`Frame::present`] follows wgpu-hal: `presentDrawable:` is registered on a
//! fresh command buffer before `commit`. `presentsWithTransaction` and
//! `presentDrawable:afterMinimumDuration:` are not used.

use std::ffi::c_void;
use std::sync::Arc;

use aterm_objc::Bool as ObjcBool;

use super::ffi::{
    AutoreleasePool, ClassPtr, Device, Id, Obj, PixelFormat, Sel, checked_msg, class, sel,
};
use super::{encoder, loss};

// QuartzCore linkage, beside the only code that resolves QuartzCore classes.
#[link(name = "QuartzCore", kind = "framework")]
unsafe extern "C" {
    /// Mach absolute time in seconds — the clock CoreAnimation itself uses.
    fn CACurrentMediaTime() -> f64;
}

// The EDR colorspace, created per configure and released after the layer
// retains it.
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    /// +1: the caller owns the result and must `CGColorSpaceRelease` it.
    fn CGColorSpaceCreateWithName(name: *const c_void) -> *mut c_void;
    /// +0: a borrowed `CFStringRef` owned by the colorspace.
    #[cfg(test)]
    fn CGColorSpaceGetName(space: *mut c_void) -> *const c_void;
    fn CGColorSpaceRelease(space: *mut c_void);
    /// `kCGColorSpaceExtendedLinearSRGB` (scRGB): linear transfer, sRGB
    /// primaries, unclamped components. The macOS f16 present's space.
    static kCGColorSpaceExtendedLinearSRGB: *const c_void;
    /// `kCGColorSpaceSRGB`, the SDR present's space. Set explicitly because an
    /// EDR-to-SDR flip on a live layer does not restore it (see the module
    /// header).
    static kCGColorSpaceSRGB: *const c_void;
}

/// A borrowed `CFString` as an owned `String`, or `None` for NULL. A CFString
/// is toll-free bridged to `NSString`, so [`super::ffi::ns_string_text`]
/// reads it by its counted UTF-16 units, never as a C string.
///
/// # Safety
/// `name` must be NULL or a live `CFString`.
#[cfg(test)]
unsafe fn cf_string_text(name: *const c_void) -> Option<String> {
    // SAFETY: a live CFString is a live NSString to the runtime (toll-free
    // bridged); the caller vouches for it, and it is copied out at once.
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

// SAFETY: `#[repr(C)]` two `double`s, `CGSize`.
unsafe impl aterm_objc::Encode for CgSize {
    const ENCODING: &'static str = "{CGSize=dd}";
}

/// `CGPoint`/`CGRect`, the `setFrame:` argument.
///
/// Sound as an argument through plain `objc_msgSend` on both arm64 and x86_64.
/// A `CGRect` return would need `objc_msgSend_stret` on x86_64, which `ffi`
/// does not declare, so [`Swapchain::sync_layer_geometry`] reads only the
/// scalar `contentsScale` and derives the frame.
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

// SAFETY: `#[repr(C)]` a `CGPoint` then a `CGSize`, `CGRect`.
unsafe impl aterm_objc::Encode for CgRect {
    const ENCODING: &'static str = "{CGRect={CGPoint=dd}{CGSize=dd}}";
}

/// Everything one swapchain configure sets. `Self::aterm_present` builds the
/// production values; tests build others to prove every setter is live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SwapchainConfig {
    /// The layer `pixelFormat`: [`PixelFormat::Bgra8Unorm`] (SDR) or
    /// [`PixelFormat::Rgba16Float`] (EDR); anything else is rejected.
    pub(crate) format: PixelFormat,
    /// `drawableSize`, in pixels. Explicit, never derived from layer bounds.
    pub(crate) width: usize,
    pub(crate) height: usize,
    /// `framebufferOnly`: `true` gives lossless drawable compression but no
    /// sampling or blit access. Production runs `true` until a video tap arms
    /// (`renderer.rs::surface_usage`); readback tests run `false`.
    pub(crate) framebuffer_only: bool,
    /// `displaySyncEnabled`. `false` is wgpu's `Immediate`, the shipped macOS
    /// choice; `true` is `Fifo` (`wgpu-hal surface.rs:74-78`).
    pub(crate) display_sync: bool,
    /// `maximumDrawableCount`. Legal range `[2, 3]` (`CAMetalLayer.h:104-108`;
    /// out-of-range values throw); the shipped value is 3.
    pub(crate) maximum_drawables: usize,
    /// `CALayer.opaque`. `false` for the `PostMultiplied` translucent
    /// present.
    pub(crate) opaque: bool,
}

impl SwapchainConfig {
    /// The production macOS values: `framebufferOnly` on, display sync off
    /// (Immediate), 3 drawables, opaque.
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

    /// Reject in Rust what the layer would throw (or silently misdraw) on.
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

/// One configured `CAMetalLayer` and what every acquire needs. Thread-pinned
/// raw-pointer holder; not `Send`/`Sync`.
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

    /// A swapchain parented under `parent` (any `CALayer`). Adds one sublayer
    /// and never touches existing ones. Dropping the `Swapchain` unparents its
    /// own layer only.
    pub(crate) fn attached(
        device: &Device,
        parent: &Obj,
        config: &SwapchainConfig,
        latch: Arc<loss::LossLatch>,
    ) -> Result<Self, String> {
        let layer = Self::new_layer()?;
        Self::configure(&layer, device, config)?;
        // SAFETY: `addSublayer:` is a void message on the live parent; it
        // retains the sublayer, and our +1 stays ours for drop to balance.
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

    /// Adopt the parent layer's `contentsScale`, so a drawable pixel is a
    /// display pixel; a fresh `CAMetalLayer` defaults to 1.0, which on a 2x
    /// display draws the grid at twice its size and soft. Called at attach and
    /// from every reconcile, since moving a window between displays changes the
    /// parent's scale with no resize. Change-gated. The standalone shape has no
    /// parent and is left alone.
    pub(crate) fn sync_layer_geometry(&mut self) {
        if !self.is_attached {
            return;
        }
        let _pool = AutoreleasePool::new();
        // SAFETY: `superlayer` returns a borrowed (autoreleased) `CALayer` or
        // nil, never released here. `contentsScale` returns a `CGFloat` and
        // `setContentsScale:` takes one by value; both are scalar on every
        // target, so no `objc_msgSend_stret` is needed.
        unsafe {
            let superlayer: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let parent = superlayer(self.layer.id(), sel(c"superlayer"));
            if parent.is_null() {
                return;
            }
            let get_scale: unsafe extern "C-unwind" fn(Id, Sel) -> f64 = checked_msg();
            let scale = get_scale(parent, sel(c"contentsScale"));
            // Positive only (also rejects NaN): a zero or negative scale would
            // blank the window, so keep the layer's current value instead.
            if scale > 0.0 && self.applied_geometry != Some((scale, self.width, self.height)) {
                // An action-free transaction keeps the scale change from
                // animating (as `MacCpuPresenter::sync_layer_geometry` requires).
                let txn = class(c"CATransaction");
                let cls_void: unsafe extern "C-unwind" fn(ClassPtr, Sel) = checked_msg();
                let cls_bool: unsafe extern "C-unwind" fn(ClassPtr, Sel, ObjcBool) = checked_msg();
                cls_void(txn, sel(c"begin"));
                cls_bool(txn, sel(c"setDisableActions:"), true.into());
                let set_scale: unsafe extern "C-unwind" fn(Id, Sel, f64) = checked_msg();
                set_scale(self.layer.id(), sel(c"setContentsScale:"), scale);
                // Give the layer the bounds its contents occupy; an attached
                // layer is otherwise left at `CGRectZero`, a trap for anything
                // that consults bounds. Keeps
                // `bounds x contentsScale == drawableSize` by construction.
                let set_frame: unsafe extern "C-unwind" fn(Id, Sel, CgRect) = checked_msg();
                set_frame(
                    self.layer.id(),
                    sel(c"setFrame:"),
                    CgRect {
                        // The origin corner is where gravity already anchors the
                        // contents, so nothing moves on glass.
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

    /// The `contentsScale` read back off the live layer, not echoed from
    /// [`Self::applied_geometry`].
    #[cfg(test)]
    pub(crate) fn contents_scale(&self) -> f64 {
        let _pool = AutoreleasePool::new();
        // SAFETY: scalar `CGFloat` getter on our live +1 layer.
        unsafe {
            let get_scale: unsafe extern "C-unwind" fn(Id, Sel) -> f64 = checked_msg();
            get_scale(self.layer.id(), sel(c"contentsScale"))
        }
    }

    /// `[[CAMetalLayer alloc] init]` — +1, owned.
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
        // SAFETY: `alloc` returns a +1 uninitialized instance; `init` consumes
        // it and returns the initialized +1 object (or nil, mapped to Err).
        unsafe {
            let alloc: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id = checked_msg();
            let raw = alloc(cls, sel(c"alloc"));
            let init: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            Obj::from_owned(init(raw, sel(c"init")))
                .ok_or_else(|| "CAMetalLayer init returned nil".to_owned())
        }
    }

    /// Write every axis in the module header's table onto the layer.
    fn configure(layer: &Obj, device: &Device, config: &SwapchainConfig) -> Result<(), String> {
        config.validate()?;
        let _pool = AutoreleasePool::new();
        // SAFETY: every send is a documented property setter on the live layer
        // with its prototype written out (object, NSUInteger, `ObjcBool`,
        // `CGSize` by value, `CGColorSpaceRef`). The colorspace is created at
        // +1 and released after `setColorspace:`, which retains it.
        unsafe {
            let set_obj: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
            let set_usize: unsafe extern "C-unwind" fn(Id, Sel, usize) = checked_msg();
            let set_bool: unsafe extern "C-unwind" fn(Id, Sel, ObjcBool) = checked_msg();
            let set_size: unsafe extern "C-unwind" fn(Id, Sel, CgSize) = checked_msg();

            // EDR iff the format is f16, as in wgpu-hal. The colorspace is
            // created before the first layer write, so a refused configure
            // leaves the layer untouched (`reconcile` and
            // `upgrade_surface_for_screen` rely on this); it is set after
            // `setPixelFormat:`, which would otherwise overwrite it.
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
            // Explicit although it is the OS default (wgpu-hal sets `NO`).
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
            // Both arms name their colorspace: on a live layer holding scRGB,
            // flipping back to `Bgra8Unorm` keeps the extended space, and an SDR
            // present tagged scRGB is misread by the compositor.
            let set_space: unsafe extern "C-unwind" fn(Id, Sel, *mut c_void) = checked_msg();
            set_space(layer.id(), sel(c"setColorspace:"), space);
            CGColorSpaceRelease(space);
        }
        Ok(())
    }

    /// Apply a new config to the live layer instead of rebuilding the
    /// `Swapchain` (resize, HDR flip, `COPY_SRC` arming). Metal permits every
    /// axis live; drawables vended before keep the old config, those after
    /// carry the new one. [`Frame`] holds `&mut Swapchain`, so no live frame
    /// can present onto a stale config. Refuses when the loss latch is set,
    /// before touching the layer.
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
    ///
    /// `&mut self` is the frame boundary. Nil from `nextDrawable` (pool
    /// exhausted past the ~1 s timeout, or invalid properties) returns `Err`
    /// naming the wait. Refuses before any FFI once the loss latch is set.
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
                // SAFETY: as above.
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

    /// The raw acquire: `nextDrawable` under a pool, both objects retained to
    /// +1.
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

    // Readback getters: one per configured axis, through the property's own
    // getter, so tests compare the layer's actual state with their constants.

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
        // SAFETY: `drawableSize` returns CGSize by value (HFA in v0/v1).
        let s = unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> CgSize = checked_msg();
            f(self.layer.id(), sel(c"drawableSize"))
        };
        (s.width, s.height)
    }

    #[cfg(test)]
    pub(crate) fn framebuffer_only(&self) -> bool {
        // SAFETY: BOOL getter on the live layer.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> ObjcBool = checked_msg();
            f(self.layer.id(), sel(c"framebufferOnly")).as_bool()
        }
    }

    #[cfg(test)]
    pub(crate) fn display_sync_enabled(&self) -> bool {
        // SAFETY: BOOL getter on the live layer.
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
        // SAFETY: BOOL getter on the live layer.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> ObjcBool = checked_msg();
            f(self.layer.id(), sel(c"wantsExtendedDynamicRangeContent")).as_bool()
        }
    }

    #[cfg(test)]
    pub(crate) fn is_opaque(&self) -> bool {
        // SAFETY: BOOL getter on the live layer.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> ObjcBool = checked_msg();
            f(self.layer.id(), sel(c"isOpaque")).as_bool()
        }
    }

    /// The layer colorspace's CoreGraphics name, or `None` when nil or unnamed.
    #[cfg(test)]
    pub(crate) fn colorspace_name(&self) -> Option<String> {
        // SAFETY: `colorspace` returns a +0 `CGColorSpaceRef` or NULL;
        // `CGColorSpaceGetName` borrows a CFString (or NULL) the colorspace
        // owns, copied out at once. Nothing is retained or released.
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
            // SAFETY: `removeFromSuperlayer` is a void message and no-ops on a
            // layer already released by its parent. The parent's retain drops
            // here; our +1 drops with `Obj`.
            unsafe {
                let f: unsafe extern "C-unwind" fn(Id, Sel) = checked_msg();
                f(self.layer.id(), sel(c"removeFromSuperlayer"));
            }
        }
        // The child has been explicitly detached; release the retained parent
        // on this same retirement thread, inside a pool for driver dealloc work.
        let _pool = AutoreleasePool::new();
        drop(self.parent.take());
    }
}

/// A retained `CAMetalLayer` used only for `nextDrawable`. The owner defers
/// every layer setter, geometry sync and unparent until the request completes.
pub(crate) struct LayerAcquire {
    layer: Option<Obj>,
}

// SAFETY: a retained `CAMetalLayer`, whose `nextDrawable` may run on another
// thread. Single-request admission excludes concurrent acquires and layer
// mutation. `Obj` and `Swapchain` stay !Send/!Sync.
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
    // Retaining the origin prevents address reuse from defeating the domain check.
    source_layer: Option<Obj>,
    objects: Option<(Obj, Obj)>,
}

// SAFETY: Metal drawable/texture references support handoff between rendering
// threads. Ownership moves once; neither reference is accessed concurrently.
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
    // SAFETY: `layer` is a retained `CAMetalLayer`. Both +0 returns are retained
    // before the pool drains; nil refuses. The caller serializes this with
    // layer mutation and other acquisitions.
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
///
/// Holds `&mut Swapchain`, so no second acquire while unresolved. Ends with
/// [`Self::present`] (consuming) or drop (returns the drawable to the pool).
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
    /// The drawable's texture: the render-pass colour attachment (and, when
    /// the layer is not `framebufferOnly`, a legal blit source for readback).
    #[inline]
    pub(crate) const fn texture(&self) -> &Obj {
        &self.texture
    }

    /// The drawable's texture as a sealed render target stamped with this
    /// swapchain's loss domain, the only form the encoder's write verbs
    /// accept; a session on a different latch is refused there. Read-only uses
    /// use [`Self::texture`].
    pub(crate) fn render_target(&self) -> super::resources::SealedTexture {
        // SAFETY: retaining a live +1 texture; the sealed handle releases its
        // own +1 on drop, independently of this frame's.
        let obj =
            unsafe { Obj::retain(self.texture.id()) }.expect("retain of a live drawable texture");
        super::resources::SealedTexture::from_parts(obj, Arc::clone(&self.swapchain.latch))
    }

    /// The swapchain this frame came from (for format/extent queries mid-frame).
    #[cfg(test)]
    pub(crate) fn swapchain(&self) -> &Swapchain {
        self.swapchain
    }

    /// `-[CAMetalDrawable layer]`, the layer this drawable came from; the
    /// stacking test compares it with [`Swapchain::layer_ptr`].
    #[cfg(test)]
    pub(crate) fn drawable_layer_ptr(&self) -> Id {
        // SAFETY: object getter (+0 borrow, compared by pointer only).
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            f(self.drawable.id(), sel(c"layer"))
        }
    }

    /// Present the frame: a fresh command buffer from the session's queue with
    /// `presentDrawable:` registered before `commit` (wgpu-hal's shape), so
    /// presentation cannot fire before the drawable's rendering is scheduled.
    ///
    /// That ordering needs one queue. Taking the [`encoder::EncodeSession`]
    /// that owns it (`EncodeSession::new` refuses a latch already bound),
    /// sealed render targets, and the latch check below leave no way to present
    /// behind a second queue. Submission is lock-stepped with the window server
    /// servicing the present, which frame pacing must expect. Consumes the
    /// frame; the returned [`PresentTicket`] is its command-buffer handle.
    pub(crate) fn present(self, session: &encoder::EncodeSession) -> Result<PresentTicket, String> {
        // The loss may land between acquire and present. Refuse before touching
        // the queue; dropping `self` discards the drawable.
        if let Some(reason) = self.swapchain.latch.reason() {
            return Err(format!(
                "present refused: the device-loss latch is set ({reason}); \
                 the frame is discarded, not committed to the dead queue"
            ));
        }
        // A session on a different latch would record outcomes acquire never
        // sees; refuse it.
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
        // SAFETY: `commandBuffer` returns a +0 buffer, retained into the ticket
        // so callers can wait after the pool pops. `presentDrawable:` then
        // `commit`, in that order.
        unsafe {
            let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let raw = get(session.one_shot_queue().id(), sel(c"commandBuffer"));
            let cb = Obj::retain(raw)
                .ok_or_else(|| "commandBuffer returned nil for the present".to_owned())?;
            // Present-to-glass reporting (`crate::present_glass`): the handler
            // must be added before presenting. Only when a sink listens.
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

/// Hang `addPresentedHandler:` on `drawable`, reporting `presentDrawable:`
/// registration to `-[MTLDrawable presentedTime]` via
/// [`crate::present_glass::deliver_report`] with the registration's serial and
/// pacing tag. The registration instant is read on `CACurrentMediaTime`, the
/// clock `presentedTime` uses.
///
/// # Safety
///
/// `drawable` must be a live `id<MTLDrawable>` not yet presented.
unsafe fn register_presented_handler(drawable: Id) {
    // SAFETY: a pure clock read.
    let registered_s = unsafe { CACurrentMediaTime() };
    // Serial and pacing tag for this registration, decided now so the handler
    // reports its own present.
    let (serial, tag) = crate::present_glass::registration_begin(registered_s);
    // Defined outside any `unsafe` block so its one unsafe operation, the
    // `presentedTime` message, carries its own scope.
    let handler = move |presented: Id| {
        // SAFETY: Metal passes the live drawable being reported;
        // `presentedTime` is a `CFTimeInterval` (double) getter, 0 when the
        // drawable was never shown.
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
    // SAFETY: `MTLDrawablePresentedHandler` is `void (^)(id<MTLDrawable>)` —
    // exactly `(Id) -> ()`, the prototype this block is invoked with. The
    // closure captures one `f64`, one `u64` and one `PresentTag` (all `Copy`),
    // performs no allocation or panicking operation, and the sink contract
    // forbids blocking or unwinding.
    let block = unsafe { aterm_objc::RcBlock::new1(handler) };
    let Some(block) = block else { return };
    // SAFETY: `addPresentedHandler:` copies (retains) the block, so ours is
    // released independently when `block` drops.
    unsafe {
        let add: unsafe extern "C-unwind" fn(Id, Sel, aterm_objc::BlockPtr) = checked_msg();
        add(drawable, sel(c"addPresentedHandler:"), block.as_block_ptr());
    }
}

/// The committed present's command buffer, retained so its status can be
/// awaited and classified, plus the latch handle so the classification always
/// reaches the latch.
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
    /// Block until the present's command buffer completes, classify it via
    /// [`loss::outcome_of`] and feed the latch. On a healthy GPU the outcome is
    /// always `Completed`, whose record is a no-op; the injected-loss test
    /// exercises `settle` with a `PageFault`.
    pub(crate) fn wait_outcome(&self) -> loss::CbOutcome {
        if let Some(done) = self.settled.get() {
            return done;
        }
        // SAFETY: `waitUntilCompleted` is a void message on the owned +1
        // command buffer; it returns once the status is terminal.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) = checked_msg();
            f(self.cb.id(), sel(c"waitUntilCompleted"));
        }
        self.settle_terminal(loss::outcome_of(self.cb.id()))
    }

    /// Poll: `None` while in flight, `Some(outcome)` once terminal. The first
    /// terminal answer feeds the latch like [`Self::wait_outcome`] (same
    /// contract as [`super::encoder::Submitted::try_outcome`]); later calls
    /// answer from the cache and feed nothing.
    pub(crate) fn try_outcome(&self) -> Option<loss::CbOutcome> {
        if let Some(done) = self.settled.get() {
            return Some(done);
        }
        match loss::outcome_of(self.cb.id()) {
            loss::CbOutcome::Unfinished { .. } => None,
            terminal => Some(self.settle_terminal(terminal)),
        }
    }

    /// Record the terminal outcome once: feed the latch, fill the cache.
    fn settle_terminal(&self, outcome: loss::CbOutcome) -> loss::CbOutcome {
        self.settled.set(Some(outcome));
        self.settle(outcome)
    }

    /// Record one classified outcome on the process latch and return it. Also
    /// the injection seam, deliberately uncached: a real `Lost` cannot be
    /// produced on a healthy GPU, so the end-to-end test drives a `PageFault`
    /// through this on a real present's ticket.
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

    /// [`Swapchain::colorspace_name`]'s CFString is read by its counted UTF-16
    /// units. CoreGraphics names only its own system colorspaces, so a bridged
    /// `NSString` built from raw units stands in for the name.
    #[test]
    fn a_colorspace_name_is_read_whole() {
        let _pool = ffi::AutoreleasePool::new();
        // SAFETY: each `NSString` is live for its read, and a live NSString is
        // a live CFString (toll-free bridged).
        let read = C_STRING_HOLES
            .map(|(units, _)| unsafe { cf_string_text(ns_string_of_units(units).id().as_ptr()) });
        assert_eq!(read, C_STRING_HOLES.map(|(_, want)| Some(want.to_owned())));
        // SAFETY: NULL is an accepted input.
        assert_eq!(unsafe { cf_string_text(std::ptr::null()) }, None);
    }

    /// The config refuses exactly what `CAMetalLayer` would throw on (or never
    /// vend a drawable for), with the reason in the message.
    #[test]
    fn the_config_rejects_what_the_layer_would_throw_on() {
        let ok = SwapchainConfig::aterm_present(PixelFormat::Bgra8Unorm, 16, 16);
        assert!(ok.validate().is_ok());

        // Not a layer format: pick_surface_format's Rgba8Unorm fallback is for
        // other backends, never reachable on Metal.
        let mut bad = ok;
        bad.format = PixelFormat::Rgba8Unorm;
        let err = bad
            .validate()
            .expect_err("Rgba8Unorm is not a layer format");
        assert!(err.contains("Rgba8Unorm"), "got: {err}");

        // CAMetalLayer.h:104-108 — [2, 3] or an exception.
        for n in [0, 1, 4] {
            let mut bad = ok;
            bad.maximum_drawables = n;
            let err = bad.validate().expect_err("out-of-range drawable count");
            assert!(err.contains("[2, 3]"), "got: {err}");
        }

        // A zero extent never vends a drawable (CAMetalLayer.h:95-97).
        let mut bad = ok;
        bad.width = 0;
        assert!(bad.validate().is_err());
    }

    /// The standalone shape has no parent, so the sync is a no-op and must not
    /// crash on a nil `superlayer`.
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

    /// Injected loss end to end through production plumbing: a real present's
    /// ticket, an injected `PageFault` via [`PresentTicket::settle`], then:
    ///
    /// * a retryable outcome does not latch, and the next acquire vends;
    /// * the `Lost` latches, and a frame acquired before it refuses to present;
    /// * every later acquire refuses, naming the first loss's reason.
    ///
    /// "Touching nothing" is counted by [`encoder::queue_ffi_census`]
    /// (`nextDrawable` and the present's `commandBuffer` mint), not timed.
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

        // A healthy frame: `wait_outcome` feeds the latch itself. The census
        // sees its `nextDrawable` and `commandBuffer`, proving it counts.
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

        // The polled twin answers from the settled cache and feeds nothing.
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

        // Retryable does not latch, and the swapchain keeps serving.
        let outcome = ticket.settle(classify(STATUS_ERROR, Some(ERROR_TIMEOUT)));
        assert!(matches!(outcome, CbOutcome::Retryable { .. }));
        assert!(!latch.is_lost(), "Retryable must never latch");
        drop(
            sc.acquire()
                .expect("a retryable failure must not stop acquire"),
        );

        // Acquire while healthy, then inject a `PageFault` mid-frame through
        // the ticket's seam.
        let frame = sc.acquire().expect("acquired before the loss lands");
        let outcome = ticket.settle(classify(STATUS_ERROR, Some(ERROR_PAGE_FAULT)));
        assert!(matches!(outcome, CbOutcome::Lost { .. }));
        assert!(latch.is_lost(), "the injected PageFault must latch");

        // The mid-flight frame refuses to present, fast, and is discarded.
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

        // Reconfigure refuses too, before touching any layer property.
        let err = sc
            .reconfigure(&dev, &config)
            .expect_err("reconfigure after the loss must refuse");
        assert!(
            err.contains("reconfigure refused") && err.contains("PageFault"),
            "the refusal names the first loss: {err}"
        );

        // Every later acquire refuses in microseconds, naming the reason.
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

    /// Reconfigure storm: every axis the renderer reconfigures (sizes, both
    /// formats, `opaque`, `framebufferOnly`, `displaySyncEnabled`,
    /// `maximumDrawableCount`), 12 reconfigures of one live layer with an
    /// acquire and present after each. Each step reads every axis back, the
    /// colorspace (an EDR-to-SDR flip must return to `kCGColorSpaceSRGB`), and
    /// the vended drawable's extent and format.
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

        // The storm: every axis moved at least twice, EDR flipped in and out.
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

                // Layer axes, each off its own getter.
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
                // The OS does not restore sRGB on an EDR-to-SDR flip; the
                // explicit SDR arm does.
                assert_eq!(
                    sc.colorspace_name().as_deref(),
                    Some(if wants_edr {
                        "kCGColorSpaceExtendedLinearSRGB"
                    } else {
                        "kCGColorSpaceSRGB"
                    }),
                    "cycle {cycle} step {i}: colorspace after the flip"
                );

                // The next drawable carries the new config.
                // SAFETY: reading a monotonic clock.
                let t0 = unsafe { CACurrentMediaTime() };
                let frame = sc
                    .acquire()
                    .unwrap_or_else(|e| panic!("cycle {cycle} step {i}: acquire: {e}"));
                // SAFETY: as above.
                let waited = unsafe { CACurrentMediaTime() } - t0;
                // A hang detector: a starved pool answers nil at Apple's ~1 s
                // timeout (the `unwrap_or_else` above), never a slow vend.
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

    /// The two-submit frame, six times through a 3-drawable pool on one queue.
    /// Submit A renders the bg row into the offscreen's sRGB view and is not
    /// waited on; Submit B blits onto the drawable through `Pipeline::Blit`,
    /// appends the tap copy and is waited on, which proves A completed (checked
    /// with [`encoder::Submitted::try_outcome`]); `Frame::present(&session)`
    /// registers the present behind both. Bytes are checked per frame, and A's
    /// poll, B's wait and the ticket's wait each feed the latch exactly once.
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
                framebuffer_only: false, // readback
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

        // Minted sealed: the offscreen carries the session's loss domain,
        // which lets the encoder's write verbs accept it.
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
        // SAFETY: `repr(C)` into an exactly-sized fresh shared buffer.
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
        // SAFETY: `repr(C)`, size asserted equal to the buffer's above.
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
            // SAFETY: exactly-sized shared buffer; the previous frame's
            // command buffers were all waited on before this point.
            unsafe { ffi::buffer_write(&ibuf, &stream) };

            // Submit A: encode the frame; committed, not waited on.
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

            // The thread keeps working: acquire while A is in flight.
            let frame_obj = sc
                .acquire()
                .unwrap_or_else(|e| panic!("frame {frame}: acquire failed: {e}"));

            // Submit B: compose onto the drawable + the tap-shaped readback.
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
            // One queue, commit order: B terminal proves A terminal — the
            // polled harvest observes it without ever having waited on A.
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

            // Byte-verify the drawable (BGRA store order).
            // SAFETY: shared storage; submit B completed above.
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

            // Present, behind A and B on the session's one queue.
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

    /// A second latch and session cross-wired with the first are refused at
    /// every crossing:
    ///
    /// * rendering the frame's texture on session 2 dies at `render_pass`;
    /// * copying session-2 bytes onto the drawable dies at the copy verbs;
    /// * presenting on the foreign session dies at `present`.
    ///
    /// Same-latch controls prove the refusals come from the seal.
    #[test]
    fn the_two_latch_cross_wired_present_has_no_remaining_shape() {
        use crate::metal::encoder::{EncodeSession, RenderPassDesc, StoreAction};
        use crate::metal::loss::LossLatch;
        use crate::metal::resources::MetalResourceDevice;

        let dev = device();
        let _test_pool = AutoreleasePool::new();

        // Domain 1: the swapchain's.
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

        // Domain 2: a second latch, its session and a texture minted in it.
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

        // Cross-wire 1: render the frame's texture on session 2.
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

        // Cross-wire 2: copy onto the drawable via session 1 and read it back
        // via session 2; both refuse.
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

        // Same-latch controls: identical calls with matching stamps succeed.
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

        // Cross-wire 3: present on the foreign session.
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

    /// A raw foreign `CAMetalLayer` (the wgpu leftover) with device and
    /// `drawableSize` set and `framebufferOnly` off.
    fn foreign_metal_layer(dev: &Device, w: usize, h: usize) -> Obj {
        let _pool = AutoreleasePool::new();
        // SAFETY: alloc/init is +1; the setters are documented property
        // setters on the live layer, prototypes written per selector.
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

    /// Stacking: our swapchain and a live foreign `CAMetalLayer` under one
    /// parent, both with drawables in flight:
    ///
    /// * our drawable's `-layer` is our layer;
    /// * our bytes land in our drawable;
    /// * the sibling's drawable keeps its bytes after our present;
    /// * dropping the swapchain unparents only its own layer.
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
        // The leftover is parented first, as a dead wgpu surface's layer is.
        // SAFETY: `addSublayer:` retains `foreign`; both objects are live.
        unsafe {
            let add: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
            add(parent.id(), sel(c"addSublayer:"), foreign.id());
        }

        // The blit row, once per attachment format it will write.
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
        // SAFETY: `repr(C)`, size-asserted layout into a fresh shared buffer.
        unsafe { ffi::buffer_write(&blit_ubuf, as_bytes(&blit_uniform)) };

        // Two solid source textures with different colours (RGBA memory).
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
            // SAFETY: managed-storage (non-Private) texture, exact extent and
            // stride.
            unsafe { ffi::texture_upload(&tex, ffi::MtlRegion::full_2d(W, H), &bytes, W * 4) };
            tex
        };
        let ours_src = make_src(OURS);
        let theirs_src = make_src(THEIRS);

        let row = W * PixelFormat::Bgra8Unorm.bytes_per_texel();
        let readback = dev.new_buffer(row * H).expect("readback");

        // The foreign layer's in-flight frame, rendered before our swapchain
        // exists.
        // SAFETY: `nextDrawable`/`texture` are +0 under the pool, retained to
        // +1 as `raw_next_drawable` does.
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
            // The drawable's own back-pointer: our layer, not the sibling's or
            // the parent's.
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

            // Render our colour into our drawable and check every byte.
            blit_into(frame.texture(), &ours_src, &blit_bgra).expect("our render");
            // SAFETY: shared storage, written before draw_and_read returned.
            let got = unsafe { ffi::buffer_bytes(&readback, row * H) };
            let want = [OURS[2], OURS[1], OURS[0], 255]; // BGRA store order
            for (i, texel) in got.as_chunks::<4>().0.iter().enumerate() {
                assert_eq!(
                    texel, &want,
                    "texel {i}: our drawable must hold OUR bytes (BGRA)"
                );
            }

            // Present ours; the sibling's frame is still in flight.
            let outcome = frame.present(&session).expect("present").wait_outcome();
            assert_eq!(outcome, crate::metal::loss::CbOutcome::Completed);
            assert!(!latch.is_lost());

            // The sibling's drawable after our present keeps its bytes (read
            // through a scratch target so nothing re-renders into it).
            let scratch = dev
                .new_texture_2d(
                    PixelFormat::Rgba8Unorm,
                    W,
                    H,
                    ffi::TEXTURE_USAGE_RENDER_TARGET | ffi::TEXTURE_USAGE_SHADER_READ,
                )
                .expect("scratch");
            blit_into(&scratch, &foreign_tex, &blit_rgba).expect("foreign readback");
            // SAFETY: as above.
            let got = unsafe { ffi::buffer_bytes(&readback, row * H) };
            for (i, texel) in got.as_chunks::<4>().0.iter().enumerate() {
                assert_eq!(
                    texel, &THEIRS,
                    "texel {i}: the foreign in-flight drawable must still hold \
                     the sibling's bytes after our render + present (RGBA)"
                );
            }
        }
        // Teardown beside a live sibling: only the sibling remains.
        assert_eq!(sublayer_count(&parent), 1, "drop unparents ours alone");
        assert_eq!(
            first_sublayer(&parent),
            foreign.id(),
            "the survivor is the foreign layer"
        );
        drop(foreign_drawable);
    }

    /// `-[CALayer sublayers][0]`, or null.
    fn first_sublayer(layer: &Obj) -> Id {
        let _pool = AutoreleasePool::new();
        // SAFETY: `sublayers` returns a +0 NSArray (or nil) owned by the
        // pool; `objectAtIndex:` returns a +0 element, compared by pointer
        // before the pool pops.
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

    /// One present ticket feeds the latch exactly once across wait and poll,
    /// however mixed; the injection seam stays uncached. Twin of the encoder's
    /// test.
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

    /// With a sink installed, each present delivers one presented-handler
    /// sample (on glass or skipped; an unparented layer may read `Skipped`).
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
        // A hang detector: the presented handler fires on the compositor's
        // thread, and the loop returns the moment the last sample lands.
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
        // SAFETY: a scalar object-property read on the independently retained
        // child. The result is compared, never dereferenced or released here.
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

        // Positive control: drive the very same retained-layer request/output
        // handoff as the worker, then recover the exclusive Frame boundary.
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

        // Equal size, format and device must not hide a foreign origin, with
        // shared or distinct loss domains.
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
            // The packet must retain its origin even after the source owner
            // disappears. It cannot be rebound to the surviving destination.
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
