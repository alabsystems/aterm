// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/cursor.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// LOCAL PATCH (aterm), 2026-10-16 (docs/AUDIT-third-party-2026-10-15.md, winit-core step 5):
// custom cursors are API no aterm crate uses. The macOS backend no longer implements them, nor
// (2026-10-21, WC.8) the Linux module, so everything here but `Cursor` and its `Icon` variant is
// `#[cfg(windows_platform)]`; the Windows carve deletes the rest with its half. The
// `OnlyCursorImage` handle the Wayland backend alone kept a cursor image in left with WC.8.

#[cfg(windows_platform)]
use core::fmt;
#[cfg(windows_platform)]
use std::error::Error;

use crate::cursor_icon::CursorIcon;

#[cfg(windows_platform)]
use crate::platform_impl::{PlatformCustomCursor, PlatformCustomCursorSource};

/// The maximum width and height for a cursor when using [`CustomCursor::from_rgba`].
#[cfg(windows_platform)]
pub const MAX_CURSOR_SIZE: u16 = 2048;

#[cfg(windows_platform)]
const PIXEL_SIZE: usize = 4;

/// See [`Window::set_cursor()`][crate::window::Window::set_cursor] for more details.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Cursor {
    Icon(CursorIcon),
    #[cfg(windows_platform)]
    Custom(CustomCursor),
}

impl Default for Cursor {
    #[inline]
    fn default() -> Self {
        Self::Icon(CursorIcon::default())
    }
}

impl From<CursorIcon> for Cursor {
    #[inline]
    fn from(icon: CursorIcon) -> Self {
        Self::Icon(icon)
    }
}

#[cfg(windows_platform)]
impl From<CustomCursor> for Cursor {
    #[inline]
    fn from(custom: CustomCursor) -> Self {
        Self::Custom(custom)
    }
}

/// Use a custom image as a cursor (mouse pointer).
///
/// Is guaranteed to be cheap to clone.
///
/// ## Platform-specific
///
/// **Web**: Some browsers have limits on cursor sizes usually at 128x128.
///
/// # Example
///
/// ```no_run
/// # use aterm_window::event_loop::ActiveEventLoop;
/// # use aterm_window::window::Window;
/// # fn scope(event_loop: &ActiveEventLoop, window: &Window) {
/// use aterm_window::window::CustomCursor;
///
/// let w = 10;
/// let h = 10;
/// let rgba = vec![255; (w * h * 4) as usize];
///
/// #[cfg(not(target_family = "wasm"))]
/// let source = CustomCursor::from_rgba(rgba, w, h, w / 2, h / 2).unwrap();
///
/// #[cfg(target_family = "wasm")]
/// let source = {
///     use aterm_window::platform::web::CustomCursorExtWebSys;
///     CustomCursor::from_url(String::from("http://localhost:3000/cursor.png"), 0, 0)
/// };
///
/// let custom_cursor = event_loop.create_custom_cursor(source);
///
/// window.set_cursor(custom_cursor.clone());
/// # }
/// ```
#[cfg(windows_platform)]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CustomCursor {
    /// Platforms should make sure this is cheap to clone.
    pub(crate) inner: PlatformCustomCursor,
}

#[cfg(windows_platform)]
impl CustomCursor {
    /// Creates a new cursor from an rgba buffer.
    ///
    /// The alpha channel is assumed to be **not** premultiplied.
    pub fn from_rgba(
        rgba: impl Into<Vec<u8>>,
        width: u16,
        height: u16,
        hotspot_x: u16,
        hotspot_y: u16,
    ) -> Result<CustomCursorSource, BadImage> {
        let _span = tracing::debug_span!(
            "aterm_window::Cursor::from_rgba",
            width,
            height,
            hotspot_x,
            hotspot_y
        )
        .entered();

        Ok(CustomCursorSource {
            inner: PlatformCustomCursorSource::from_rgba(
                rgba.into(),
                width,
                height,
                hotspot_x,
                hotspot_y,
            )?,
        })
    }
}

/// Source for [`CustomCursor`].
///
/// See [`CustomCursor`] for more details.
#[cfg(windows_platform)]
#[derive(Debug)]
pub struct CustomCursorSource {
    pub(crate) inner: PlatformCustomCursorSource,
}

/// An error produced when using [`CustomCursor::from_rgba`] with invalid arguments.
#[cfg(windows_platform)]
#[derive(Debug, Clone)]
pub enum BadImage {
    /// Produced when the image dimensions are larger than [`MAX_CURSOR_SIZE`]. This doesn't
    /// guarantee that the cursor will work, but should avoid many platform and device specific
    /// limits.
    TooLarge { width: u16, height: u16 },
    /// Produced when the length of the `rgba` argument isn't divisible by 4, thus `rgba` can't be
    /// safely interpreted as 32bpp RGBA pixels.
    ByteCountNotDivisibleBy4 { byte_count: usize },
    /// Produced when the number of pixels (`rgba.len() / 4`) isn't equal to `width * height`.
    /// At least one of your arguments is incorrect.
    DimensionsVsPixelCount { width: u16, height: u16, width_x_height: u64, pixel_count: u64 },
    /// Produced when the hotspot is outside the image bounds
    HotspotOutOfBounds { width: u16, height: u16, hotspot_x: u16, hotspot_y: u16 },
}

#[cfg(windows_platform)]
impl fmt::Display for BadImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BadImage::TooLarge { width, height } => write!(
                f,
                "The specified dimensions ({width:?}x{height:?}) are too large. The maximum is \
                 {MAX_CURSOR_SIZE:?}x{MAX_CURSOR_SIZE:?}.",
            ),
            BadImage::ByteCountNotDivisibleBy4 { byte_count } => write!(
                f,
                "The length of the `rgba` argument ({byte_count:?}) isn't divisible by 4, making \
                 it impossible to interpret as 32bpp RGBA pixels.",
            ),
            BadImage::DimensionsVsPixelCount { width, height, width_x_height, pixel_count } => {
                write!(
                    f,
                    "The specified dimensions ({width:?}x{height:?}) don't match the number of \
                     pixels supplied by the `rgba` argument ({pixel_count:?}). For those \
                     dimensions, the expected pixel count is {width_x_height:?}.",
                )
            },
            BadImage::HotspotOutOfBounds { width, height, hotspot_x, hotspot_y } => write!(
                f,
                "The specified hotspot ({hotspot_x:?}, {hotspot_y:?}) is outside the image bounds \
                 ({width:?}x{height:?}).",
            ),
        }
    }
}

#[cfg(windows_platform)]
impl Error for BadImage {}

/// Platforms export this directly as `PlatformCustomCursorSource` if they need to only work with
/// images.
#[cfg(windows_platform)]
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct OnlyCursorImageSource(pub(crate) CursorImage);

#[cfg(windows_platform)]
#[allow(dead_code)]
impl OnlyCursorImageSource {
    pub(crate) fn from_rgba(
        rgba: Vec<u8>,
        width: u16,
        height: u16,
        hotspot_x: u16,
        hotspot_y: u16,
    ) -> Result<Self, BadImage> {
        CursorImage::from_rgba(rgba, width, height, hotspot_x, hotspot_y).map(Self)
    }
}

#[cfg(windows_platform)]
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct CursorImage {
    pub(crate) rgba: Vec<u8>,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) hotspot_x: u16,
    pub(crate) hotspot_y: u16,
}

#[cfg(windows_platform)]
impl CursorImage {
    pub(crate) fn from_rgba(
        rgba: Vec<u8>,
        width: u16,
        height: u16,
        hotspot_x: u16,
        hotspot_y: u16,
    ) -> Result<Self, BadImage> {
        if width > MAX_CURSOR_SIZE || height > MAX_CURSOR_SIZE {
            return Err(BadImage::TooLarge { width, height });
        }

        if !rgba.len().is_multiple_of(PIXEL_SIZE) {
            return Err(BadImage::ByteCountNotDivisibleBy4 { byte_count: rgba.len() });
        }

        let pixel_count = (rgba.len() / PIXEL_SIZE) as u64;
        let width_x_height = width as u64 * height as u64;
        if pixel_count != width_x_height {
            return Err(BadImage::DimensionsVsPixelCount {
                width,
                height,
                width_x_height,
                pixel_count,
            });
        }

        if hotspot_x >= width || hotspot_y >= height {
            return Err(BadImage::HotspotOutOfBounds { width, height, hotspot_x, hotspot_y });
        }

        Ok(CursorImage { rgba, width, height, hotspot_x, hotspot_y })
    }
}
