// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The first-party macOS CPU presenter: a `CALayer` whose `contents` is a
//! `CGImage` built over the frame buffer, with no `softbuffer` and no
//! `objc2-quartz-core` / `objc2-core-graphics` in the graph.
//!
//! # What it replaces, and why it is allowed to be this small
//!
//! `softbuffer 0.4.8`'s macOS backend (`src/backends/cg.rs`, 388 lines) does
//! exactly one thing per present: wrap the pixel words in a `CGDataProvider`,
//! build a 32bpp `CGImage` over them, and assign it to a `CALayer`'s `contents`
//! inside a `CATransaction` with actions disabled. Everything else in that file
//! is the `Context`/`Surface` generic plumbing this seam already removed, plus a
//! KVO observer that keeps the layer's geometry in step with the view.
//!
//! This module keeps the pixel path byte-for-byte and replaces the observer
//! with a direct geometry sync (see [`MacCpuPresenter::sync_layer_geometry`]).
//! That is not a shortcut: `aterm-gui` calls
//! [`CpuPresenter::resize`] on EVERY present, so a pull-based sync runs exactly
//! as often as a KVO push would have, is change-gated so a steady frame issues
//! no setters at all, and cannot leave a dangling observer registration behind
//! (the failure mode `cg.rs` needs its whole `Drop` impl to avoid).
//!
//! # Fidelity: this backend is behaviourally identical to the one it retires
//!
//! Three properties are load-bearing and all three are preserved deliberately:
//!
//! 1. **A fresh, zeroed buffer per acquisition.** [`CpuPresenter::buffer_mut`]
//!    allocates `vec![0; width * height]`, and [`CpuFrameBuffer::present`]
//!    hands that allocation to CoreGraphics, which frees it when the last
//!    reference to the image goes away. This is `cg.rs`'s own model. It is also
//!    the only sound one for a `CGImage`-over-our-memory present: CoreAnimation
//!    may read the provider's bytes after `commit` returns, so a buffer the app
//!    keeps writing into would tear.
//! 2. **`age()` is unconditionally 0**, exactly as `cg.rs:294`. It follows from
//!    (1) — a freshly zeroed allocation has never been on screen — and it means
//!    the frontend's `buf.age() != 1` gate (`app_render.rs`) always selects the
//!    full-copy branch on this cell, as it already did before this change.
//! 3. **`present_with_damage` commits the same pixels as `present`**, as
//!    `cg.rs:364` does. A `CALayer`'s `contents` is a whole-image property;
//!    there is no partial assignment to make. The damage list is still carried
//!    across the seam (linux/windows honour it, and an IOSurface-backed macOS
//!    backend could later honour it without a seam change) — it simply cannot
//!    bound the work here. Given (2), the frontend never reaches this arm on
//!    macOS anyway.
//!
//! Taken together: **this presenter produces the same macOS pixels as the
//! `softbuffer` path it replaces, by construction**, because it performs the
//! same CoreGraphics calls in the same order over the same bytes.
//!
//! # FFI conventions
//!
//! Hand-written, following `crates/aterm-gui/src/net_connections/keychain.rs`
//! and the sibling `platform::layer_colorspace`: only the entry points actually
//! used are declared, CoreGraphics and QuartzCore are already linked in-process
//! (AppKit pulls both), every `Create` result lands in an owning wrapper that
//! releases it exactly once, and every `unsafe` block states the invariant that
//! makes it sound.

use std::ffi::c_void;
use std::fmt;
use std::mem::size_of;
use std::num::NonZeroU32;
use std::ops::{Deref, DerefMut};
use std::ptr;
use std::sync::Arc;

use aterm_objc::{CGPoint, CGRect, Id, Obj, Sel, class, sel};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use super::{CpuFrameBuffer, CpuPresenter, DamageRect};
use crate::appkit::{self, MainThread};
// Two CoreGraphics releasers are ALREADY declared for the `window` verb's
// full-window capture. Reuse those declarations rather than adding a second
// spelling of the same symbols: `platform::layer_colorspace` needs
// `CGColorSpaceRelease` typed as `*mut CGColorSpace` (an objc encoding
// requirement), so a third `*mut c_void` copy here would make the crate carry
// two mutually-clashing declarations of one release function.
use crate::cg_capture::{CGColorSpaceRelease, CGImageRelease};

// ---------------------------------------------------------------------------
// CoreGraphics / CoreAnimation FFI. Opaque pointers only: nothing below is ever
// dereferenced in Rust, it is handed straight back to CG/CA or released.
// ---------------------------------------------------------------------------

type CGColorSpaceRef = *mut c_void;
type CGDataProviderRef = *mut c_void;
type CGImageRef = *mut c_void;
/// `CGFloat` is `double` on every 64-bit Apple platform aterm builds for.
type CGFloat = f64;

/// `CGDataProviderReleaseDataCallback`.
type CGDataProviderReleaseDataCallback =
    unsafe extern "C" fn(info: *mut c_void, data: *const c_void, size: usize);

/// `kCGImageAlphaNoneSkipFirst` — the high byte is padding, not alpha. With the
/// little-endian 32-bit byte order below this describes exactly the frontend's
/// `0x00RRGGBB` word.
const K_CG_IMAGE_ALPHA_NONE_SKIP_FIRST: u32 = 6;
/// `kCGBitmapByteOrder32Little` (`2 << 12`, `CGImage.h`).
const K_CG_BITMAP_BYTE_ORDER_32_LITTLE: u32 = 2 << 12;
/// The `CGBitmapInfo` softbuffer's CG backend builds, verbatim: alpha-skip-first
/// plus integer components, 32-bit little-endian and a packed pixel format. The
/// component-info and pixel-format contributions are both zero.
const BITMAP_INFO: u32 = K_CG_IMAGE_ALPHA_NONE_SKIP_FIRST | K_CG_BITMAP_BYTE_ORDER_32_LITTLE;
/// `kCGRenderingIntentDefault`.
const RENDERING_INTENT_DEFAULT: i32 = 0;
/// Bits per colour component in the presented image.
const BITS_PER_COMPONENT: usize = 8;
/// Bits per pixel — one `u32` word.
const BITS_PER_PIXEL: usize = 32;
/// Bytes per pixel — one `u32` word.
const BYTES_PER_PIXEL: usize = 4;

// SAFETY (whole block): these are the stable, documented CoreGraphics C entry
// points with the signatures published in Apple's headers. Each contract is
// upheld at its call site below: every `Create` result is checked for NULL and
// released exactly once, the data handed to `CGDataProviderCreateWithData` is a
// leaked `Box<[u32]>` whose ownership passes to the provider and comes back
// through `release_pixel_box`, and the colour space outlives every image built
// against it (it is owned by the presenter, dropped last).
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    /// The generic device RGB space — what `softbuffer`'s CG backend uses, so
    /// the presented colours are unchanged.
    fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;

    /// Wrap caller-owned bytes in a data provider. `release_data` is invoked
    /// exactly once, when the provider's last reference goes away, and is how
    /// ownership of the buffer comes back to Rust.
    fn CGDataProviderCreateWithData(
        info: *mut c_void,
        data: *const c_void,
        size: usize,
        release_data: Option<CGDataProviderReleaseDataCallback>,
    ) -> CGDataProviderRef;
    fn CGDataProviderRelease(provider: CGDataProviderRef);

    /// Build an image over a data provider. The image RETAINS the provider, so
    /// the caller's create-reference is released immediately afterwards.
    fn CGImageCreate(
        width: usize,
        height: usize,
        bits_per_component: usize,
        bits_per_pixel: usize,
        bytes_per_row: usize,
        space: CGColorSpaceRef,
        bitmap_info: u32,
        provider: CGDataProviderRef,
        decode: *const CGFloat,
        should_interpolate: bool,
        intent: i32,
    ) -> CGImageRef;
}

// SAFETY: a read of an immutable, stable QuartzCore-exported `CFStringRef`
// global. Never freed, never mutated here. `platform::layer_colorspace`
// declares the same symbol with the same type.
#[link(name = "QuartzCore", kind = "framework")]
unsafe extern "C" {
    /// Pins unrescaled contents to the layer's MINIMUM-Y left corner in a
    /// geometry-flipped layer — i.e. the visual top-left, which is the corner a
    /// terminal grid grows from.
    static kCAGravityTopLeft: *const c_void;
}

/// CoreGraphics hands the presented buffer back here when the last reference to
/// the image (and through it, the data provider) goes away.
///
/// # Safety
///
/// Called only by CoreGraphics, only with the `data`/`size` pair passed to
/// `CGDataProviderCreateWithData` in [`MacCpuFrame::commit`], and only once.
unsafe extern "C" fn release_pixel_box(_info: *mut c_void, data: *const c_void, size: usize) {
    if data.is_null() {
        return;
    }
    let len = size / size_of::<u32>();
    // SAFETY: `data` and `size` are exactly the thin pointer and byte length of
    // one `Box<[u32]>` leaked with `Box::into_raw` in `commit`, which is the
    // only producer of providers with this callback. CoreGraphics delivers that
    // pair back verbatim, exactly once, so reconstituting the same box and
    // dropping it frees it exactly once.
    unsafe {
        drop(Box::from_raw(ptr::slice_from_raw_parts_mut(
            data.cast::<u32>().cast_mut(),
            len,
        )));
    }
}

// ---------------------------------------------------------------------------
// Owning wrappers. Every +1 reference this module takes lives in one of these.
// ---------------------------------------------------------------------------

// The +1 Objective-C reference that used to live in a hand-rolled
// `OwnedObject` here is now `aterm_objc::Obj`, which is the same wrapper with
// the same single-release Drop — and DELETING it is a small instance of the
// thing the whole campaign is about. The crate docs open by naming `CfOwned`,
// duplicated byte-for-byte between `net_connections/keychain.rs` and
// `aterm-http/src/verifier/apple.rs`, as the tree's demonstrated answer to not
// having a shared layer. `OwnedObject` was a third copy of that shape.

/// A +1 `CGColorSpaceRef`, released exactly once on drop.
struct OwnedColorSpace(CGColorSpaceRef);

impl Drop for OwnedColorSpace {
    fn drop(&mut self) {
        if self.0.is_null() {
            return;
        }
        // SAFETY: `self.0` came from `CGColorSpaceCreateDeviceRGB` (+1) and is
        // released nowhere else; every `CGImage` built against it retained it
        // for as long as it needs it.
        unsafe { CGColorSpaceRelease(self.0) };
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a macOS CPU present could not be set up or committed.
///
/// Every variant is a DROPPED frame or a declined window at the call site, never
/// a panic: this is the arm a user reaches only because their GPU already
/// failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MacPresentError {
    /// AppKit views and layers are main-thread only.
    NotMainThread,
    /// The window has no AppKit (`NSView`) handle — it is not a Cocoa window.
    NoAppKitHandle,
    /// `-[NSView layer]` returned nil even after `setWantsLayer:`.
    NoRootLayer,
    /// A required Objective-C class is absent from this process.
    MissingClass(&'static str),
    /// `+[CALayer alloc] init]` returned nil.
    LayerAlloc,
    /// `CGColorSpaceCreateDeviceRGB` returned NULL.
    ColorSpace,
    /// The surface has no pixels yet — `resize` has not been called.
    EmptySurface,
    /// `CGDataProviderCreateWithData` returned NULL.
    DataProvider,
    /// `CGImageCreate` returned NULL.
    Image,
}

impl fmt::Display for MacPresentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotMainThread => {
                f.write_str("CoreGraphics presentation requires the main thread")
            }
            Self::NoAppKitHandle => f.write_str("window has no AppKit NSView handle"),
            Self::NoRootLayer => f.write_str("NSView refused to become layer-backed"),
            Self::MissingClass(name) => write!(f, "Objective-C class {name} is unavailable"),
            Self::LayerAlloc => f.write_str("CALayer allocation failed"),
            Self::ColorSpace => f.write_str("CGColorSpaceCreateDeviceRGB failed"),
            Self::EmptySurface => f.write_str("CPU surface has no pixels (resize never ran)"),
            Self::DataProvider => f.write_str("CGDataProviderCreateWithData failed"),
            Self::Image => f.write_str("CGImageCreate failed"),
        }
    }
}

// ---------------------------------------------------------------------------
// The presenter
// ---------------------------------------------------------------------------

/// The geometry last pushed to our layer: the root layer's bounds plus its
/// contents scale. Compared as raw components so a steady frame issues no
/// Objective-C setters at all.
type LayerGeometry = (CGFloat, CGFloat, CGFloat, CGFloat, CGFloat);

/// One window's first-party macOS CPU presentation target.
///
/// FIELD ORDER IS LOAD-BEARING HERE. Rust drops a struct's fields in
/// DECLARATION order, so `_window` is declared LAST: the `Arc<Window>` that
/// keeps the `NSView` (and therefore `root_layer`) alive must outlive every
/// field that borrows from it. It previously sat first, which is the exact
/// inverse of what its own doc comment asked for.
///
/// No use-after-free was reachable from the old order — [`Drop`] runs before
/// any field drops and already unparents the layer, and `root_layer` is a raw
/// pointer nothing dereferences during teardown — so this is a latent hazard
/// closed, not a live bug fixed. It is worth closing because the next thing
/// that lands here (a `CAMetalLayer`, whose release path DOES touch its
/// superlayer) would make the order matter for real.
pub(crate) struct MacCpuPresenter {
    /// Our own +1 `CALayer`, a SUBLAYER of the view's backing layer.
    ///
    /// A sublayer, not the view's own layer, for the reason `softbuffer` gives:
    /// setting `contents` on a layer the view controls is brittle — AppKit may
    /// overwrite it on any redisplay.
    layer: Obj,
    /// The view's backing layer. BORROWED: owned by the `NSView`, which is kept
    /// alive by `_window`. Never released here.
    root_layer: Id,
    /// Created once, reused by every image, released with the presenter.
    color_space: OwnedColorSpace,
    /// Backing-store width in physical pixels, from [`CpuPresenter::resize`].
    width: usize,
    /// Backing-store height in physical pixels, from [`CpuPresenter::resize`].
    height: usize,
    /// Change gate for [`MacCpuPresenter::sync_layer_geometry`].
    applied_geometry: Option<LayerGeometry>,
    /// Keeps winit's `NSWindow`/`NSView` alive for as long as this presenter
    /// holds a layer parented into that view. Dropping the last window
    /// reference before the layer would leave `root_layer` dangling — which is
    /// why this is the LAST field and therefore the last to drop.
    _window: Arc<Window>,
}

impl MacCpuPresenter {
    /// Keep our sublayer covering the view and rendering at the view's scale.
    ///
    /// `softbuffer` does this from a KVO observer on the root layer's `bounds`
    /// and `contentsScale`; we pull the same two values instead, which is
    /// equivalent because the frontend resizes and presents in one step and
    /// therefore visits this function exactly as often as a notification would
    /// have fired. Change-gated, so the steady state costs two property reads.
    ///
    /// Caller must already be inside a `CATransaction` with actions disabled —
    /// `frame` and `contentsScale` both carry implicit animations otherwise.
    ///
    /// # Safety
    ///
    /// `self.root_layer` and `self.layer` must be live `CALayer`s and the caller
    /// must be on the main thread.
    unsafe fn sync_layer_geometry(&mut self) {
        // SAFETY: both layers are live for the presenter's lifetime (`layer` is
        // our +1, `root_layer` is owned by the `NSView` that `_window` keeps
        // alive). `bounds` and `contentsScale` are side-effect-free reads;
        // `setFrame:` and `setContentsScale:` are plain property writes.
        unsafe {
            let bounds = appkit::send_rect(self.root_layer, sel!(bounds));
            let scale: CGFloat = appkit::send_f64(self.root_layer, sel!(contentsScale));
            let geometry = (
                bounds.origin.x,
                bounds.origin.y,
                bounds.size.width,
                bounds.size.height,
                scale,
            );
            if self.applied_geometry == Some(geometry) {
                return;
            }
            let set_frame: unsafe extern "C" fn(Id, Sel, CGRect) = aterm_objc::msg();
            set_frame(self.layer.id(), sel!(setFrame:), bounds);
            // A non-positive scale would divide the contents to nothing; keep
            // whatever the layer already had rather than blank the window.
            if scale > 0.0 {
                let set_scale: unsafe extern "C" fn(Id, Sel, f64) = aterm_objc::msg();
                set_scale(self.layer.id(), sel!(setContentsScale:), scale);
            }
            self.applied_geometry = Some(geometry);
        }
    }
}

impl Drop for MacCpuPresenter {
    fn drop(&mut self) {
        // SAFETY: `self.layer` is our live +1 `CALayer`; unparenting it before
        // `Obj`'s Drop releases our reference is what keeps a downgraded-then-
        // rebuilt window from stacking dead layers over the live one.
        // `-removeFromSuperlayer` is `-(void)`. Runs on the main thread, where
        // the presenter is owned.
        unsafe {
            appkit::send_v(self.layer.id(), sel!(removeFromSuperlayer));
        }
    }
}

impl CpuPresenter for MacCpuPresenter {
    type Error = MacPresentError;
    type Buffer<'a>
        = MacCpuFrame<'a>
    where
        Self: 'a;

    fn new(window: Arc<Window>) -> Result<Self, Self::Error> {
        // AppKit views and CoreAnimation layers may only be touched from the
        // main thread; refuse rather than corrupt AppKit state off it.
        let _main = MainThread::new().ok_or(MacPresentError::NotMainThread)?;
        // Scoped so the borrow of `window` ends before `window` is moved into
        // `Self`. `ns_view` is a plain `NonNull` and borrows nothing.
        let ns_view = {
            let handle = window
                .window_handle()
                .map_err(|_| MacPresentError::NoAppKitHandle)?;
            match handle.as_raw() {
                RawWindowHandle::AppKit(appkit) => appkit.ns_view,
                _ => return Err(MacPresentError::NoAppKitHandle),
            }
        };
        // Dynamic lookup: a process somehow without QuartzCore reports a typed
        // error instead of aborting. `aterm_objc::class` is `objc_getClass`,
        // which answers null rather than panicking, so the check is the same
        // one `AnyClass::get`'s `Option` made.
        let layer_class = class(c"CALayer");
        if layer_class.is_null() {
            return Err(MacPresentError::MissingClass("CALayer"));
        }

        // SAFETY: `ns_view` points at this window's live `NSView` — winit owns
        // it for the window's lifetime and `window` (moved into `Self` below)
        // holds that lifetime open — and we are on the main thread, checked
        // above. `setWantsLayer:` and `layer` are the documented AppKit
        // layer-backing pair; `alloc`/`init` on `CALayer` yields a +1 reference
        // that `OwnedObject` takes ownership of before anything else can fail;
        // `setAnchorPoint:`, `setGeometryFlipped:` and `setContentsGravity:` are
        // plain property writes (the gravity string is a framework constant that
        // outlives the process, and the setter retains it); `addSublayer:`
        // retains our layer, which is why our own +1 must still be released.
        unsafe {
            let view = Id::from_ptr(ns_view.as_ptr().cast());
            appkit::send_v_bool(view, sel!(setWantsLayer:), true);
            let root_layer = appkit::send_id(view, sel!(layer));
            if root_layer.is_null() {
                return Err(MacPresentError::NoRootLayer);
            }

            let raw = appkit::send_id(appkit::alloc(layer_class), sel!(init));
            // A failing `init` has already released the allocation per Cocoa
            // convention — touching it here would be an over-release, which is
            // exactly what `Obj::from_owned`'s `None` arm expresses.
            let Some(layer) = Obj::from_owned(raw) else {
                return Err(MacPresentError::LayerAlloc);
            };

            // Top-left origin, matching the frontend's framebuffer coordinates.
            let set_anchor: unsafe extern "C" fn(Id, Sel, CGPoint) = aterm_objc::msg();
            set_anchor(
                layer.id(),
                sel!(setAnchorPoint:),
                CGPoint { x: 0.0, y: 0.0 },
            );
            appkit::send_v_bool(layer.id(), sel!(setGeometryFlipped:), true);
            if !kCAGravityTopLeft.is_null() {
                appkit::send_v_id(
                    layer.id(),
                    sel!(setContentsGravity:),
                    Id::from_ptr(kCAGravityTopLeft.cast_mut()),
                );
            }
            appkit::send_v_id(root_layer, sel!(addSublayer:), layer.id());

            let color_space = CGColorSpaceCreateDeviceRGB();
            if color_space.is_null() {
                // Unparent before `layer`'s drop releases our reference, so a
                // failed construction leaves the view exactly as it was found.
                appkit::send_v(layer.id(), sel!(removeFromSuperlayer));
                return Err(MacPresentError::ColorSpace);
            }

            Ok(Self {
                layer,
                root_layer,
                color_space: OwnedColorSpace(color_space),
                width: 0,
                height: 0,
                applied_geometry: None,
                // Last, matching the declaration order the type's docs pin.
                _window: window,
            })
        }
    }

    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) -> Result<(), Self::Error> {
        // Only the BACKING-STORE size lives here. Layer geometry is the view's
        // business and is synced at commit time; this mirrors `softbuffer`'s CG
        // backend, whose `resize` is likewise pure bookkeeping.
        self.width = width.get() as usize;
        self.height = height.get() as usize;
        Ok(())
    }

    fn buffer_mut(&mut self) -> Result<Self::Buffer<'_>, Self::Error> {
        let len = self
            .width
            .checked_mul(self.height)
            .filter(|len| *len > 0)
            .ok_or(MacPresentError::EmptySurface)?;
        // A FRESH zeroed allocation every acquisition — see the module note on
        // why the buffer cannot be retained across presents, and why `age()` is
        // therefore 0.
        Ok(MacCpuFrame {
            buffer: vec![0_u32; len],
            presenter: self,
        })
    }
}

// ---------------------------------------------------------------------------
// The acquired frame
// ---------------------------------------------------------------------------

/// One acquired macOS CPU frame: `width * height` words of `0x00RRGGBB`.
pub(crate) struct MacCpuFrame<'a> {
    presenter: &'a mut MacCpuPresenter,
    buffer: Vec<u32>,
}

impl MacCpuFrame<'_> {
    /// The one commit path: hand the buffer to CoreGraphics as a `CGImage` and
    /// assign it to our layer inside an action-free `CATransaction`.
    ///
    /// The transaction is not cosmetic: a bare `contents` assignment carries
    /// `CALayer`'s default action, which cross-fades over roughly a quarter of a
    /// second — every frame — and would smear the terminal.
    fn commit(self) -> Result<(), MacPresentError> {
        let Self { presenter, buffer } = self;
        let (width, height) = (presenter.width, presenter.height);
        // The buffer was sized from these fields at acquisition and `resize` is
        // not reachable while it is borrowed, so this cannot fail; check it
        // anyway rather than hand CoreGraphics a length it disagrees with.
        if width == 0 || height == 0 || buffer.len() != width * height {
            return Err(MacPresentError::EmptySurface);
        }
        let byte_len = buffer.len() * size_of::<u32>();
        let raw: *mut [u32] = Box::into_raw(buffer.into_boxed_slice());

        // SAFETY: `raw` is a live, uniquely-owned `Box<[u32]>` of exactly
        // `byte_len` bytes, leaked immediately above; `release_pixel_box` is the
        // matching reconstitution. Ownership passes to the provider on success
        // and is reclaimed by hand on the NULL path, so the allocation is freed
        // exactly once either way.
        let provider = unsafe {
            CGDataProviderCreateWithData(
                ptr::null_mut(),
                raw.cast::<c_void>(),
                byte_len,
                Some(release_pixel_box),
            )
        };
        if provider.is_null() {
            // CoreGraphics never took the buffer; take it back.
            // SAFETY: `raw` is still the sole owning pointer to that box.
            unsafe { drop(Box::from_raw(raw)) };
            return Err(MacPresentError::DataProvider);
        }

        // SAFETY: `provider` is a live +1 provider over `width * height` words;
        // `color_space` is the presenter's live device-RGB space; the layout
        // arguments describe exactly that buffer (8 bits per component, 32 bits
        // per pixel, `width * 4` bytes per row) and `BITMAP_INFO` names the
        // frontend's `0x00RRGGBB` word. `decode` is NULL (the default decode
        // array), which the API documents as valid.
        let image = unsafe {
            CGImageCreate(
                width,
                height,
                BITS_PER_COMPONENT,
                BITS_PER_PIXEL,
                width * BYTES_PER_PIXEL,
                presenter.color_space.0,
                BITMAP_INFO,
                provider,
                ptr::null(),
                false,
                RENDERING_INTENT_DEFAULT,
            )
        };
        // The image retained the provider (or, on failure, nothing did and this
        // release runs `release_pixel_box` and frees the buffer).
        // SAFETY: balances the +1 from `CGDataProviderCreateWithData`.
        unsafe { CGDataProviderRelease(provider) };
        if image.is_null() {
            return Err(MacPresentError::Image);
        }

        let transaction = class(c"CATransaction");
        // SAFETY: `CATransaction`'s begin/setDisableActions:/commit are the
        // documented class-method triple and are balanced here on every path
        // (nothing between them can return early). `setContents:` takes an `id`;
        // a `CGImageRef` is a valid layer-contents value and the property
        // retains it, which is why our create-reference is released immediately
        // after the commit. Main thread, as checked in `new`.
        unsafe {
            if !transaction.is_null() {
                appkit::send_v(transaction.as_id(), sel!(begin));
                appkit::send_v_bool(transaction.as_id(), sel!(setDisableActions:), true);
            }
            presenter.sync_layer_geometry();
            appkit::send_v_id(
                presenter.layer.id(),
                sel!(setContents:),
                Id::from_ptr(image),
            );
            if !transaction.is_null() {
                appkit::send_v(transaction.as_id(), sel!(commit));
            }
            CGImageRelease(image);
        }
        Ok(())
    }
}

impl Deref for MacCpuFrame<'_> {
    type Target = [u32];

    fn deref(&self) -> &[u32] {
        &self.buffer
    }
}

impl DerefMut for MacCpuFrame<'_> {
    fn deref_mut(&mut self) -> &mut [u32] {
        &mut self.buffer
    }
}

impl CpuFrameBuffer for MacCpuFrame<'_> {
    type Error = MacPresentError;

    fn age(&self) -> u8 {
        // Always 0: each acquisition is a fresh zeroed allocation whose contents
        // have never been on screen. Identical to `softbuffer`'s CG backend
        // (`cg.rs:294`), and it is what keeps the frontend on its full-copy
        // branch on this cell.
        0
    }

    fn present(self) -> Result<(), Self::Error> {
        self.commit()
    }

    fn present_with_damage(self, _damage: &[DamageRect]) -> Result<(), Self::Error> {
        // A `CALayer`'s `contents` is a whole-image property: there is no
        // partial assignment, so the damage list cannot bound the work. Commit
        // the same pixels `present` would — over-claiming damage is always safe,
        // and this is precisely what `softbuffer`'s CG backend does
        // (`cg.rs:364`). Unreachable in practice because `age()` is 0.
        self.commit()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BITMAP_INFO, BITS_PER_COMPONENT, BITS_PER_PIXEL, BYTES_PER_PIXEL, MacPresentError,
        release_pixel_box,
    };

    /// The bitmap descriptor is the ONE place a silent divergence from the
    /// retired `softbuffer` backend would show up as wrong colours rather than
    /// as a build error, so pin it against `cg.rs`'s literal composition:
    /// `CGImageAlphaInfo::NoneSkipFirst | CGImageComponentInfo::Integer |
    /// CGImageByteOrderInfo::Order32Little | CGImagePixelFormatInfo::Packed`.
    #[test]
    fn bitmap_descriptor_matches_the_retired_backend() {
        let none_skip_first = 6_u32;
        let integer_components = 0_u32;
        let order_32_little = 2_u32 << 12;
        let packed = 0_u32;
        assert_eq!(
            BITMAP_INFO,
            none_skip_first | integer_components | order_32_little | packed,
        );
        assert_eq!(BITS_PER_COMPONENT * 4, BITS_PER_PIXEL);
        assert_eq!(BITS_PER_PIXEL / 8, BYTES_PER_PIXEL);
    }

    /// The release callback is called by CoreGraphics on a background thread at
    /// an unpredictable time, so its two degenerate inputs are exercised here
    /// rather than left to a live present: a NULL pointer must be inert, and a
    /// real leaked box must be reclaimed exactly once (run under a leak checker
    /// this is the whole ownership contract).
    #[test]
    fn release_callback_reclaims_the_box_and_ignores_null() {
        // SAFETY: the documented degenerate input — the callback must not touch
        // a NULL data pointer.
        unsafe { release_pixel_box(std::ptr::null_mut(), std::ptr::null(), 64) };

        let pixels = vec![0x0011_2233_u32; 32];
        let byte_len = pixels.len() * std::mem::size_of::<u32>();
        let raw: *mut [u32] = Box::into_raw(pixels.into_boxed_slice());
        // SAFETY: exactly the pairing `commit` sets up — the thin pointer and
        // byte length of one leaked `Box<[u32]>`, delivered once.
        unsafe { release_pixel_box(std::ptr::null_mut(), raw.cast(), byte_len) };
    }

    /// Every failure is a reported, displayable drop — never a panic and never
    /// an empty message, because this arm is only ever reached by a user whose
    /// GPU has already failed and the string is what they get.
    #[test]
    fn every_error_prints_something_actionable() {
        for error in [
            MacPresentError::NotMainThread,
            MacPresentError::NoAppKitHandle,
            MacPresentError::NoRootLayer,
            MacPresentError::MissingClass("CALayer"),
            MacPresentError::LayerAlloc,
            MacPresentError::ColorSpace,
            MacPresentError::EmptySurface,
            MacPresentError::DataProvider,
            MacPresentError::Image,
        ] {
            assert!(!error.to_string().is_empty());
        }
        assert!(
            MacPresentError::MissingClass("CALayer")
                .to_string()
                .contains("CALayer")
        );
    }
}
