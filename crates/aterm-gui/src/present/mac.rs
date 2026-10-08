// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The first-party macOS CPU presenter: a `CALayer` whose `contents` is a
//! `CGImage` built over the frame buffer, without `softbuffer`,
//! `objc2-quartz-core` or `objc2-core-graphics`.
//!
//! Each present wraps the pixel words in a `CGDataProvider`, builds a 32bpp
//! `CGImage` over them, and assigns it to a sublayer's `contents` inside a
//! `CATransaction` with actions disabled, the same CoreGraphics calls in the
//! same order as `softbuffer`'s CG backend, so the pixels are identical. Layer
//! geometry is synced directly ([`MacCpuPresenter::sync_layer_geometry`])
//! instead of through a KVO observer: `aterm-gui` calls [`CpuPresenter::resize`]
//! on every present, the sync is change-gated, and no observer registration
//! can dangle.
//!
//! Three properties matter:
//!
//! 1. **A fresh, zeroed buffer per acquisition.** [`CpuPresenter::buffer_mut`]
//!    allocates `vec![0; width * height]`, and [`CpuFrameBuffer::present`]
//!    hands it to CoreGraphics, which frees it with the last image reference.
//!    CoreAnimation may read the bytes after `commit` returns, so a retained
//!    buffer would tear.
//! 2. **`age()` is always 0**, following from (1), so the frontend's
//!    `buf.age() != 1` gate (`app_render.rs`) always takes the full-copy branch.
//! 3. **`present_with_damage` commits the same pixels as `present`.** A
//!    `CALayer`'s `contents` is a whole-image property. The damage list still
//!    crosses the seam for linux/windows; given (2) this arm is not reached on
//!    macOS.
//!
//! # FFI conventions
//!
//! Hand-written, like `crates/aterm-gui/src/net_connections/keychain.rs` and
//! `platform::layer_colorspace`: only the entry points used are declared,
//! CoreGraphics and QuartzCore are already linked (AppKit pulls both), every
//! `Create` result lands in an owning wrapper that releases it once, and every
//! `unsafe` block states the invariant that makes it sound.

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
// Reuse the releasers declared for the `window` verb's full-window capture:
// `platform::layer_colorspace` needs `CGColorSpaceRelease` typed as
// `*mut CGColorSpace`, so a `*mut c_void` copy here would clash with it.
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
/// The `CGBitmapInfo` softbuffer's CG backend builds: alpha-skip-first, integer
/// components, 32-bit little-endian, packed pixels (the last two contribute 0).
const BITMAP_INFO: u32 = K_CG_IMAGE_ALPHA_NONE_SKIP_FIRST | K_CG_BITMAP_BYTE_ORDER_32_LITTLE;
/// `kCGRenderingIntentDefault`.
const RENDERING_INTENT_DEFAULT: i32 = 0;
/// Bits per colour component in the presented image.
const BITS_PER_COMPONENT: usize = 8;
/// Bits per pixel — one `u32` word.
const BITS_PER_PIXEL: usize = 32;
/// Bytes per pixel — one `u32` word.
const BYTES_PER_PIXEL: usize = 4;

// SAFETY (whole block): stable CoreGraphics C entry points with the signatures
// in Apple's headers. At each call site every `Create` result is checked for
// NULL and released once; the data given to `CGDataProviderCreateWithData` is a
// leaked `Box<[u32]>` that comes back through `release_pixel_box`; and the
// colour space (owned by the presenter, dropped last) outlives every image.
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    /// The generic device RGB space, as `softbuffer`'s CG backend uses.
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

    /// Build an image over a data provider. The image retains the provider, so
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
    /// Pins unrescaled contents to the minimum-Y left corner of a
    /// geometry-flipped layer: the visual top-left, where a terminal grid grows
    /// from.
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
    // SAFETY: `data` and `size` are the thin pointer and byte length of one
    // `Box<[u32]>` leaked in `commit`, the only producer of providers with this
    // callback, delivered back once; reconstituting and dropping it frees it once.
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
/// Every variant is a dropped frame or a declined window at the call site,
/// never a panic: users reach this arm only after their GPU failed.
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
/// Objective-C setters.
type LayerGeometry = (CGFloat, CGFloat, CGFloat, CGFloat, CGFloat);

/// One window's first-party macOS CPU presentation target.
///
/// Field order matters: fields drop in declaration order, so `_window` (the
/// `Arc<Window>` keeping the `NSView`, and so `root_layer`, alive) is last.
/// [`Drop`] also unparents the layer before any field drops.
pub(crate) struct MacCpuPresenter {
    /// Our own +1 `CALayer`, a sublayer of the view's backing layer: AppKit may
    /// overwrite `contents` on a layer the view controls.
    layer: Obj,
    /// The view's backing layer. Borrowed: owned by the `NSView`, which
    /// `_window` keeps alive. Never released here.
    root_layer: Id,
    /// Created once, reused by every image, released with the presenter.
    color_space: OwnedColorSpace,
    /// Backing-store width in physical pixels, from [`CpuPresenter::resize`].
    width: usize,
    /// Backing-store height in physical pixels, from [`CpuPresenter::resize`].
    height: usize,
    /// Change gate for [`MacCpuPresenter::sync_layer_geometry`].
    applied_geometry: Option<LayerGeometry>,
    /// Keeps winit's `NSWindow`/`NSView` alive while this presenter holds a
    /// layer parented into that view. The last field, so the last to drop.
    _window: Arc<Window>,
}

impl MacCpuPresenter {
    /// Keep our sublayer covering the view at the view's scale.
    ///
    /// Pulls the root layer's `bounds` and `contentsScale` (the values a KVO
    /// observer would push) on every present. Change-gated, so the steady state
    /// costs two property reads.
    ///
    /// Caller must be inside a `CATransaction` with actions disabled; `frame`
    /// and `contentsScale` carry implicit animations otherwise.
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
            let set_frame: unsafe extern "C-unwind" fn(Id, Sel, CGRect) = aterm_objc::msg();
            set_frame(self.layer.id(), sel!(setFrame:), bounds);
            // A non-positive scale would divide the contents to nothing; keep
            // whatever the layer already had rather than blank the window.
            if scale > 0.0 {
                let set_scale: unsafe extern "C-unwind" fn(Id, Sel, f64) = aterm_objc::msg();
                set_scale(self.layer.id(), sel!(setContentsScale:), scale);
            }
            self.applied_geometry = Some(geometry);
        }
    }
}

impl Drop for MacCpuPresenter {
    fn drop(&mut self) {
        // SAFETY: `self.layer` is our live +1 `CALayer`; unparenting it before
        // `Obj`'s Drop keeps a rebuilt window from stacking dead layers over the
        // live one. `-removeFromSuperlayer` is `-(void)`; main thread, where the
        // presenter is owned.
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
        // Dynamic lookup: a process without QuartzCore gets a typed error.
        // `aterm_objc::class` is `objc_getClass`, which answers null.
        let layer_class = class(c"CALayer");
        if layer_class.is_null() {
            return Err(MacPresentError::MissingClass("CALayer"));
        }

        // SAFETY: `ns_view` is this window's live `NSView` (winit owns it for the
        // window's lifetime, which `window`, moved into `Self` below, holds open),
        // and we are on the main thread (checked above). `setWantsLayer:` and
        // `layer` are the AppKit layer-backing pair; `alloc`/`init` on `CALayer`
        // yields a +1 that `Obj` owns before anything else can fail; the anchor,
        // flip and gravity setters are plain property writes (the gravity string
        // is a framework constant, retained by the setter); `addSublayer:`
        // retains our layer, so our own +1 is still released.
        unsafe {
            let view = Id::from_ptr(ns_view.as_ptr().cast());
            appkit::send_v_bool(view, sel!(setWantsLayer:), true);
            let root_layer = appkit::send_id(view, sel!(layer));
            if root_layer.is_null() {
                return Err(MacPresentError::NoRootLayer);
            }

            let raw = appkit::send_id(appkit::alloc(layer_class), sel!(init));
            // A failing `init` has already released the allocation (Cocoa
            // convention); `Obj::from_owned`'s `None` arm touches nothing.
            let Some(layer) = Obj::from_owned(raw) else {
                return Err(MacPresentError::LayerAlloc);
            };

            // Top-left origin, matching the frontend's framebuffer coordinates.
            let set_anchor: unsafe extern "C-unwind" fn(Id, Sel, CGPoint) = aterm_objc::msg();
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
                // Last, per the type's field-order note.
                _window: window,
            })
        }
    }

    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) -> Result<(), Self::Error> {
        // Only the backing-store size lives here; layer geometry is synced at
        // commit time.
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
        // A fresh zeroed allocation every acquisition (see the module docs).
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
    /// assign it to our layer inside an action-free `CATransaction`. A bare
    /// `contents` assignment would cross-fade every frame.
    fn commit(self) -> Result<(), MacPresentError> {
        let Self { presenter, buffer } = self;
        let (width, height) = (presenter.width, presenter.height);
        // Sized from these fields at acquisition, and `resize` is unreachable
        // while borrowed; checked anyway before handing CoreGraphics a length.
        if width == 0 || height == 0 || buffer.len() != width * height {
            return Err(MacPresentError::EmptySurface);
        }
        let byte_len = buffer.len() * size_of::<u32>();
        let raw: *mut [u32] = Box::into_raw(buffer.into_boxed_slice());

        // SAFETY: `raw` is a live, uniquely-owned `Box<[u32]>` of `byte_len`
        // bytes, leaked above; `release_pixel_box` reconstitutes it. Ownership
        // passes to the provider on success and is reclaimed by hand on the NULL
        // path, so it is freed once either way.
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
        // arguments describe that buffer (8 bits per component, 32 per pixel,
        // `width * 4` bytes per row) and `BITMAP_INFO` names the `0x00RRGGBB`
        // word. A NULL `decode` (the default decode array) is documented valid.
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
        // SAFETY: `CATransaction`'s begin/setDisableActions:/commit are balanced
        // on every path (nothing between them returns early). `setContents:`
        // takes an `id`; a `CGImageRef` is a valid contents value and the
        // property retains it, so our create-reference is released after the
        // commit. Main thread, as checked in `new`.
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
        // Always 0: each acquisition is a fresh zeroed allocation never on
        // screen, which keeps the frontend on its full-copy branch.
        0
    }

    fn present(self) -> Result<(), Self::Error> {
        self.commit()
    }

    fn present_with_damage(self, _damage: &[DamageRect]) -> Result<(), Self::Error> {
        // `contents` is a whole-image property, so the damage list cannot bound
        // the work; commit the same pixels `present` would (over-claiming damage
        // is safe). Unreachable in practice because `age()` is 0.
        self.commit()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BITMAP_INFO, BITS_PER_COMPONENT, BITS_PER_PIXEL, BYTES_PER_PIXEL, MacPresentError,
        release_pixel_box,
    };

    /// Pins the bitmap descriptor (where a divergence would show as wrong
    /// colours, not a build error) to `CGImageAlphaInfo::NoneSkipFirst |
    /// CGImageComponentInfo::Integer | CGImageByteOrderInfo::Order32Little |
    /// CGImagePixelFormatInfo::Packed`.
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

    /// CoreGraphics calls the release callback on a background thread at an
    /// unpredictable time, so its inputs are exercised here: a NULL pointer is
    /// inert, and a real leaked box is reclaimed exactly once.
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

    /// Every failure is a displayable drop with a non-empty message, never a
    /// panic.
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
