// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/icon.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use crate::platform_impl::PlatformIcon;
use std::error::Error;
use std::{fmt, io, mem};

#[repr(C)]
#[derive(Debug)]
pub(crate) struct Pixel {
    pub(crate) r: u8,
    pub(crate) g: u8,
    pub(crate) b: u8,
    pub(crate) a: u8,
}

pub(crate) const PIXEL_SIZE: usize = mem::size_of::<Pixel>();

#[derive(Debug)]
/// An error produced when using [`Icon::from_rgba`] with invalid arguments.
pub enum BadIcon {
    /// Byte count is not divisible by 4.
    ByteCountNotDivisibleBy4 { byte_count: usize },
    /// Pixel count (`rgba.len() / 4`) does not match `width * height`.
    DimensionsVsPixelCount { width: u32, height: u32, width_x_height: usize, pixel_count: usize },
    /// Underlying OS icon creation failed.
    OsError(io::Error),
}

impl fmt::Display for BadIcon {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BadIcon::ByteCountNotDivisibleBy4 { byte_count } => write!(
                f,
                "The length of the `rgba` argument ({byte_count:?}) isn't divisible by 4, making \
                 it impossible to interpret as 32bpp RGBA pixels.",
            ),
            BadIcon::DimensionsVsPixelCount { width, height, width_x_height, pixel_count } => {
                write!(
                    f,
                    "The specified dimensions ({width:?}x{height:?}) don't match the number of \
                     pixels supplied by the `rgba` argument ({pixel_count:?}). For those \
                     dimensions, the expected pixel count is {width_x_height:?}.",
                )
            },
            BadIcon::OsError(e) => write!(f, "OS error when instantiating the icon: {e:?}"),
        }
    }
}

impl Error for BadIcon {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RgbaIcon {
    pub(crate) rgba: Vec<u8>,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// For platforms which don't have window icons (e.g. web)
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NoIcon;

#[allow(dead_code)] // These are not used on every platform
mod constructors {
    use super::*;

    impl RgbaIcon {
        pub fn from_rgba(rgba: Vec<u8>, width: u32, height: u32) -> Result<Self, BadIcon> {
            if !rgba.len().is_multiple_of(PIXEL_SIZE) {
                return Err(BadIcon::ByteCountNotDivisibleBy4 { byte_count: rgba.len() });
            }
            let pixel_count = rgba.len() / PIXEL_SIZE;
            let width_x_height = (width as usize).checked_mul(height as usize).ok_or(
                BadIcon::DimensionsVsPixelCount {
                    width,
                    height,
                    width_x_height: usize::MAX,
                    pixel_count,
                },
            )?;
            if pixel_count != width_x_height {
                Err(BadIcon::DimensionsVsPixelCount { width, height, width_x_height, pixel_count })
            } else {
                Ok(RgbaIcon { rgba, width, height })
            }
        }
    }

    impl NoIcon {
        pub fn from_rgba(rgba: Vec<u8>, width: u32, height: u32) -> Result<Self, BadIcon> {
            let _ = RgbaIcon::from_rgba(rgba, width, height)?;
            Ok(NoIcon)
        }
    }
}

/// An icon used for the window titlebar, taskbar, etc.
#[derive(Clone)]
pub struct Icon {
    pub(crate) inner: PlatformIcon,
}

impl fmt::Debug for Icon {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        fmt::Debug::fmt(&self.inner, formatter)
    }
}

impl Icon {
    /// Creates an icon from 32bpp RGBA data.
    pub fn from_rgba(rgba: Vec<u8>, width: u32, height: u32) -> Result<Self, BadIcon> {
        let _span = tracing::debug_span!("aterm_window::Icon::from_rgba", width, height).entered();

        Ok(Icon { inner: PlatformIcon::from_rgba(rgba, width, height)? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_icon_rejects_overflowing_or_mismatched_dimensions() {
        assert!(matches!(
            RgbaIcon::from_rgba(Vec::new(), 65_536, 65_536),
            Err(BadIcon::DimensionsVsPixelCount {
                width: 65_536,
                height: 65_536,
                pixel_count: 0,
                ..
            })
        ));
        assert!(matches!(
            RgbaIcon::from_rgba(vec![0; 4], u32::MAX, u32::MAX),
            Err(BadIcon::DimensionsVsPixelCount {
                width: u32::MAX,
                height: u32::MAX,
                pixel_count: 1,
                ..
            })
        ));
        assert!(RgbaIcon::from_rgba(vec![0; 16], 2, 2).is_ok());
    }
}
