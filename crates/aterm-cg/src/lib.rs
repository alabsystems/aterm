// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The CoreGraphics display services aterm calls: which displays are active,
//! and each one's bounds, size, model number and current mode; and the reads
//! of a `CGImage`'s byte layout that the GUI's `window` capture copies an
//! AppKit bitmap by.
//!
//! Declarations under Apple's names, plus [`CGDisplay`], whose methods keep
//! the names the `core-graphics` crate gave them so the winit fork's call
//! sites read as before. A display ID is a plain integer that CoreGraphics
//! validates, so those methods are safe.
//!
//! **The SDK is the authority**, as for [`aterm_objc::cf`]: `tests/cg_sdk.rs`
//! holds every declaration here to the headers with clang, both arches.
//!
//! Its own crate, not a feature of `aterm-objc`: CoreGraphics is a GUI
//! framework, and only the windowing backend (crates/aterm-window) and the
//! GUI (crates/aterm-gui, the `window` capture's `CGImage` reads) should make
//! a binary load it. A feature would have been unified into every binary of a
//! build that also compiled winit, and forked aterm-objc between `-p` and
//! `--workspace` selections.
//!
//! It replaces the `core-graphics`, `core-graphics-types` and `foreign-types`
//! crates in the vendored winit fork (`docs/AUDIT-third-party-2026-10-15.md`,
//! winit-core step 3).

#![cfg(target_os = "macos")]
#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]

use std::ptr;

use aterm_objc::CGRect;

aterm_objc::opaque! { CGDisplayMode, CGImage, CGColorSpace, CGDataProvider }

pub type CGError = i32;
pub type CGDirectDisplayID = u32;
pub type CGDisplayModeRef = *mut CGDisplayMode;
pub type CGImageRef = *mut CGImage;
pub type CGColorSpaceRef = *mut CGColorSpace;
pub type CGDataProviderRef = *mut CGDataProvider;
pub type CGBitmapInfo = u32;
pub type CGColorSpaceModel = i32;
/// `aterm_objc::cf`'s, which `CGDataProviderCopyData` returns; named here so
/// `tests/cg_sdk.rs` spells it as the SDK typedef.
pub type CFDataRef = aterm_objc::cf::CFDataRef;

pub const kCGErrorSuccess: CGError = 0;
pub const kCGColorSpaceModelRGB: CGColorSpaceModel = 1;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    pub fn CGMainDisplayID() -> CGDirectDisplayID;
    pub fn CGGetActiveDisplayList(
        max_displays: u32,
        active_displays: *mut CGDirectDisplayID,
        display_count: *mut u32,
    ) -> CGError;
    pub fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;
    pub fn CGDisplayPixelsWide(display: CGDirectDisplayID) -> usize;
    pub fn CGDisplayPixelsHigh(display: CGDirectDisplayID) -> usize;
    pub fn CGDisplayModelNumber(display: CGDirectDisplayID) -> u32;
    pub fn CGDisplayCopyDisplayMode(display: CGDirectDisplayID) -> CGDisplayModeRef;

    pub fn CGImageGetWidth(image: CGImageRef) -> usize;
    pub fn CGImageGetHeight(image: CGImageRef) -> usize;
    pub fn CGImageGetBitsPerComponent(image: CGImageRef) -> usize;
    pub fn CGImageGetBitsPerPixel(image: CGImageRef) -> usize;
    pub fn CGImageGetBytesPerRow(image: CGImageRef) -> usize;
    pub fn CGImageGetBitmapInfo(image: CGImageRef) -> CGBitmapInfo;
    pub fn CGImageGetColorSpace(image: CGImageRef) -> CGColorSpaceRef;
    pub fn CGImageGetDataProvider(image: CGImageRef) -> CGDataProviderRef;
    pub fn CGImageRelease(image: CGImageRef);
    pub fn CGColorSpaceGetModel(space: CGColorSpaceRef) -> CGColorSpaceModel;
    pub fn CGDataProviderCopyData(provider: CGDataProviderRef) -> CFDataRef;
}

#[inline]
const fn check(error: CGError) -> Result<(), CGError> {
    if error == kCGErrorSuccess {
        Ok(())
    } else {
        Err(error)
    }
}

/// One display, by ID. Any ID is a valid argument: CoreGraphics answers an
/// unknown one with zeros, an empty rect or NULL, never by reading through it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CGDisplay {
    pub id: CGDirectDisplayID,
}

impl CGDisplay {
    #[inline]
    #[must_use]
    pub const fn new(id: CGDirectDisplayID) -> Self {
        Self { id }
    }

    /// The main display: the one whose origin is the global origin.
    #[inline]
    #[must_use]
    pub fn main() -> Self {
        // SAFETY: no arguments and no preconditions.
        Self::new(unsafe { CGMainDisplayID() })
    }

    /// The active displays' IDs. A display that leaves between the sizing
    /// call and the fill is dropped from the list, not left as a zero ID.
    pub fn active_displays() -> Result<Vec<CGDirectDisplayID>, CGError> {
        let mut count = 0_u32;
        // SAFETY: the sizing call — a NULL list of capacity 0 is written to
        // nowhere, and `count` receives the number of active displays.
        check(unsafe { CGGetActiveDisplayList(0, ptr::null_mut(), &raw mut count) })?;
        let mut ids = vec![0; count as usize];
        // SAFETY: `ids` has room for `count` IDs; CoreGraphics writes at most
        // that many and stores how many it wrote in `count`.
        check(unsafe { CGGetActiveDisplayList(count, ids.as_mut_ptr(), &raw mut count) })?;
        ids.truncate(count as usize);
        Ok(ids)
    }

    /// The bounds in the global display space (points, top-left origin).
    #[inline]
    #[must_use]
    pub fn bounds(&self) -> CGRect {
        // SAFETY: a by-value ID; see the type's doc.
        unsafe { CGDisplayBounds(self.id) }
    }

    #[inline]
    #[must_use]
    pub fn pixels_wide(&self) -> usize {
        // SAFETY: a by-value ID; see the type's doc.
        unsafe { CGDisplayPixelsWide(self.id) }
    }

    #[inline]
    #[must_use]
    pub fn pixels_high(&self) -> usize {
        // SAFETY: a by-value ID; see the type's doc.
        unsafe { CGDisplayPixelsHigh(self.id) }
    }

    #[inline]
    #[must_use]
    pub fn model_number(&self) -> u32 {
        // SAFETY: a by-value ID; see the type's doc.
        unsafe { CGDisplayModelNumber(self.id) }
    }

    /// Return the raw `CGDirectDisplayID`.
    #[inline]
    #[must_use]
    pub const fn raw_id(self) -> CGDirectDisplayID {
        self.id
    }

    /// Encode display geometry (`id`, `width_px`, `height_px`, `model_number`) into 16 LE bytes.
    #[inline]
    #[must_use]
    pub const fn encode_geometry_le(
        id: CGDirectDisplayID,
        width_px: u32,
        height_px: u32,
        model_number: u32,
    ) -> [u8; 16] {
        [
            id as u8,
            (id >> 8) as u8,
            (id >> 16) as u8,
            (id >> 24) as u8,
            width_px as u8,
            (width_px >> 8) as u8,
            (width_px >> 16) as u8,
            (width_px >> 24) as u8,
            height_px as u8,
            (height_px >> 8) as u8,
            (height_px >> 16) as u8,
            (height_px >> 24) as u8,
            model_number as u8,
            (model_number >> 8) as u8,
            (model_number >> 16) as u8,
            (model_number >> 24) as u8,
        ]
    }

    /// Decode 16 LE bytes into display geometry (`id`, `width_px`, `height_px`, `model_number`).
    #[inline]
    #[must_use]
    pub const fn decode_geometry_le(bytes: [u8; 16]) -> (CGDirectDisplayID, u32, u32, u32) {
        let id = (bytes[0] as u32)
            | ((bytes[1] as u32) << 8)
            | ((bytes[2] as u32) << 16)
            | ((bytes[3] as u32) << 24);
        let w = (bytes[4] as u32)
            | ((bytes[5] as u32) << 8)
            | ((bytes[6] as u32) << 16)
            | ((bytes[7] as u32) << 24);
        let h = (bytes[8] as u32)
            | ((bytes[9] as u32) << 8)
            | ((bytes[10] as u32) << 16)
            | ((bytes[11] as u32) << 24);
        let m = (bytes[12] as u32)
            | ((bytes[13] as u32) << 8)
            | ((bytes[14] as u32) << 16)
            | ((bytes[15] as u32) << 24);
        (id, w, h, m)
    }
}

/// Return `true` when `error == kCGErrorSuccess`.
#[inline]
#[must_use]
pub const fn is_cg_success(error: CGError) -> bool {
    error == kCGErrorSuccess
}
