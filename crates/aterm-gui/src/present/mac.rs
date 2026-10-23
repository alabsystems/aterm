// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS `CALayer`/`CGImage` CPU presenter ([`MacCpuPresenter::sync_layer_geometry`]).

use std::ffi::c_void;
use std::fmt;
use std::mem::size_of;
use std::num::NonZeroU32;
use std::ops::{Deref, DerefMut};
use std::ptr;
use std::sync::Arc;

use aterm_objc::{CGPoint, CGRect, Id, Obj, Sel, class, sel};
use aterm_window::window::Window;

use super::{CpuFrameBuffer, CpuPresenter, DamageRect};
use crate::appkit::{self, MainThread};
use crate::cg_capture::{CGColorSpaceRelease, CGImageRelease};

type CGColorSpaceRef = *mut c_void;
type CGDataProviderRef = *mut c_void;
type CGImageRef = *mut c_void;
/// `CGFloat` is `double` on every 64-bit Apple platform aterm builds for.
type CGFloat = f64;

/// `CGDataProviderReleaseDataCallback`.
type CGDataProviderReleaseDataCallback =
    unsafe extern "C" fn(info: *mut c_void, data: *const c_void, size: usize);

/// `kCGImageAlphaNoneSkipFirst`.
const K_CG_IMAGE_ALPHA_NONE_SKIP_FIRST: u32 = 6;
/// `kCGBitmapByteOrder32Little` (`2 << 12`, `CGImage.h`).
const K_CG_BITMAP_BYTE_ORDER_32_LITTLE: u32 = 2 << 12;
/// The `CGBitmapInfo` softbuffer's CG backend builds.
const BITMAP_INFO: u32 = K_CG_IMAGE_ALPHA_NONE_SKIP_FIRST | K_CG_BITMAP_BYTE_ORDER_32_LITTLE;
/// `kCGRenderingIntentDefault`.
const RENDERING_INTENT_DEFAULT: i32 = 0;
/// Bits per colour component in the presented image.
const BITS_PER_COMPONENT: usize = 8;
/// Bits per pixel — one `u32` word.
const BITS_PER_PIXEL: usize = 32;
/// Bytes per pixel.
const BYTES_PER_PIXEL: usize = 4;

// SAFETY: stable CoreGraphics C entry points with the signatures in Apple's headers.
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    /// The generic device RGB space, as `softbuffer`'s CG backend uses.
    fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;

    /// Wrap caller-owned bytes in a data provider.
    fn CGDataProviderCreateWithData(
        info: *mut c_void,
        data: *const c_void,
        size: usize,
        release_data: Option<CGDataProviderReleaseDataCallback>,
    ) -> CGDataProviderRef;
    fn CGDataProviderRelease(provider: CGDataProviderRef);

    /// Build an image over a data provider.
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

// SAFETY: a read of an immutable, stable QuartzCore-exported `CFStringRef` global.
#[link(name = "QuartzCore", kind = "framework")]
unsafe extern "C" {
    /// Pins unrescaled contents to the minimum-Y left corner of a geometry-flipped layer.
    static kCAGravityTopLeft: *const c_void;
}

/// CoreGraphics hands the presented buffer back here when the last image reference drops.
/// # Safety
/// `data` is NULL, or `data`/`size` describe one `Box<[u32]>` leaked in [`MacCpuFrame::commit`],
/// and that pair is passed here only once.
unsafe extern "C" fn release_pixel_box(_info: *mut c_void, data: *const c_void, size: usize) {
    if data.is_null() {
        return;
    }
    let len = size / size_of::<u32>();
    // SAFETY: `data` and `size` are the pointer and byte length of one leaked `Box<[u32]>`,
    // passed here once, so this frees it once.
    unsafe {
        drop(Box::from_raw(ptr::slice_from_raw_parts_mut(
            data.cast::<u32>().cast_mut(),
            len,
        )));
    }
}

/// A +1 `CGColorSpaceRef`, released exactly once on drop.
struct OwnedColorSpace(CGColorSpaceRef);

impl Drop for OwnedColorSpace {
    fn drop(&mut self) {
        if self.0.is_null() {
            return;
        }
        // SAFETY: `self.0` came from `CGColorSpaceCreateDeviceRGB` (+1) and is released once here;
        // every `CGImage` built against it retained it.
        unsafe { CGColorSpaceRelease(self.0) };
    }
}

/// Why a macOS CPU present could not be set up or committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MacPresentError {
    /// AppKit views and layers are main-thread only.
    NotMainThread,
    /// The window answered no AppKit content `NSView`.
    NoAppKitHandle,
    /// `-[NSView layer]` returned nil even after `setWantsLayer:`.
    NoRootLayer,
    /// A required Objective-C class is absent from this process.
    MissingClass(&'static str),
    /// `+[CALayer alloc] init]` returned nil.
    LayerAlloc,
    /// `CGColorSpaceCreateDeviceRGB` returned NULL.
    ColorSpace,
    /// The surface has no pixels.
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

/// The geometry last pushed to our layer.
type LayerGeometry = (CGFloat, CGFloat, CGFloat, CGFloat, CGFloat);

/// One window's first-party macOS CPU presentation target.
pub(crate) struct MacCpuPresenter {
    /// Our own +1 `CALayer`, a sublayer of the view's backing layer.
    layer: Obj,
    /// The view's backing layer.
    root_layer: Id,
    /// Created once, reused by every image, released with the presenter.
    color_space: OwnedColorSpace,
    /// Backing-store width in physical pixels, from [`CpuPresenter::resize`].
    width: usize,
    /// Backing-store height in physical pixels, from [`CpuPresenter::resize`].
    height: usize,
    /// Change gate for [`MacCpuPresenter::sync_layer_geometry`].
    applied_geometry: Option<LayerGeometry>,
    /// Keeps winit's `NSWindow`/`NSView` alive while this presenter holds a sublayer.
    _window: Arc<Window>,
}

impl MacCpuPresenter {
    /// Keep our sublayer covering the view at the view's scale.
    /// # Safety
    /// `self.root_layer` and `self.layer` must be live `CALayer`s on the main thread.
    unsafe fn sync_layer_geometry(&mut self) {
        // SAFETY: both layers are live for the presenter's lifetime. `bounds` and `contentsScale`
        // are property reads, and `setFrame:` and `setContentsScale:` are property writes.
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
            let set_frame: unsafe extern "C-unwind" fn(Id, Sel, CGRect) = aterm_objc::checked_msg();
            set_frame(self.layer.id(), sel!(setFrame:), bounds);
            if scale > 0.0 {
                let set_scale: unsafe extern "C-unwind" fn(Id, Sel, f64) =
                    aterm_objc::checked_msg();
                set_scale(self.layer.id(), sel!(setContentsScale:), scale);
            }
            self.applied_geometry = Some(geometry);
        }
    }
}

impl Drop for MacCpuPresenter {
    fn drop(&mut self) {
        // SAFETY: `self.layer` is our live +1 `CALayer`, and `-removeFromSuperlayer` is `-(void)`;
        // the presenter is not `Send` and was built on the main thread, so it drops there.
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
        let _main = MainThread::new().ok_or(MacPresentError::NotMainThread)?;
        let view = crate::platform::ns_view_of(&window);
        if view.is_null() {
            return Err(MacPresentError::NoAppKitHandle);
        }
        let layer_class = class(c"CALayer");
        if layer_class.is_null() {
            return Err(MacPresentError::MissingClass("CALayer"));
        }

        // SAFETY: `view` is this window's live `NSView`, on the main thread (checked above).
        // `alloc`/`init` on `CALayer` yields a +1 that `Obj` owns; `layer` and the setters are
        // plain property accesses; `addSublayer:` retains our layer, so `Obj` keeps its +1.
        unsafe {
            appkit::send_v_bool(view, sel!(setWantsLayer:), true);
            let root_layer = appkit::send_id(view, sel!(layer));
            if root_layer.is_null() {
                return Err(MacPresentError::NoRootLayer);
            }

            let raw = appkit::send_id(appkit::alloc(layer_class), sel!(init));
            let Some(layer) = Obj::from_owned(raw) else {
                return Err(MacPresentError::LayerAlloc);
            };

            let set_anchor: unsafe extern "C-unwind" fn(Id, Sel, CGPoint) =
                aterm_objc::checked_msg();
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
                _window: window,
            })
        }
    }

    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) -> Result<(), Self::Error> {
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
        Ok(MacCpuFrame {
            buffer: vec![0_u32; len],
            presenter: self,
        })
    }
}

/// One acquired macOS CPU frame.
pub(crate) struct MacCpuFrame<'a> {
    presenter: &'a mut MacCpuPresenter,
    buffer: Vec<u32>,
}

impl MacCpuFrame<'_> {
    /// The one commit path.
    fn commit(self) -> Result<(), MacPresentError> {
        let Self { presenter, buffer } = self;
        let (width, height) = (presenter.width, presenter.height);
        if width == 0 || height == 0 || buffer.len() != width * height {
            return Err(MacPresentError::EmptySurface);
        }
        let byte_len = buffer.len() * size_of::<u32>();
        let raw: *mut [u32] = Box::into_raw(buffer.into_boxed_slice());

        // SAFETY: `raw` is a live, uniquely-owned `Box<[u32]>` of `byte_len` bytes, leaked above;
        // on success the provider owns it and hands it back to `release_pixel_box` once.
        let provider = unsafe {
            CGDataProviderCreateWithData(
                ptr::null_mut(),
                raw.cast::<c_void>(),
                byte_len,
                Some(release_pixel_box),
            )
        };
        if provider.is_null() {
            // SAFETY: `raw` is still the sole owning pointer to that box.
            unsafe { drop(Box::from_raw(raw)) };
            return Err(MacPresentError::DataProvider);
        }

        // SAFETY: `provider` is a live +1 provider over `width * height` words, which the layout
        // arguments and `BITMAP_INFO` describe; the colour space is the presenter's live one, and a
        // NULL `decode` is documented valid.
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
        // SAFETY: Balances the +1 from `CGDataProviderCreateWithData`.
        unsafe { CGDataProviderRelease(provider) };
        if image.is_null() {
            return Err(MacPresentError::Image);
        }

        let transaction = class(c"CATransaction");
        // SAFETY: `CATransaction`'s begin/setDisableActions:/commit are balanced on every path;
        // main thread, as checked in `new`. `setContents:` takes the `CGImageRef` as an `id` and
        // retains it, so our +1 is released after the commit.
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
            CGImageRelease(image.cast());
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
        0
    }

    fn present(self) -> Result<(), Self::Error> {
        self.commit()
    }

    fn present_with_damage(self, _damage: &[DamageRect]) -> Result<(), Self::Error> {
        self.commit()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BITMAP_INFO, BITS_PER_COMPONENT, BITS_PER_PIXEL, BYTES_PER_PIXEL, release_pixel_box,
    };

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

    #[test]
    fn release_callback_reclaims_the_box_and_ignores_null() {
        // SAFETY: the documented degenerate input.
        unsafe { release_pixel_box(std::ptr::null_mut(), std::ptr::null(), 64) };

        let pixels = vec![0x0011_2233_u32; 32];
        let byte_len = pixels.len() * std::mem::size_of::<u32>();
        let raw: *mut [u32] = Box::into_raw(pixels.into_boxed_slice());
        // SAFETY: exactly the pairing `commit` sets up, passed once.
        unsafe { release_pixel_box(std::ptr::null_mut(), raw.cast(), byte_len) };
    }
}
