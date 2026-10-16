// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The CoreGraphics display services aterm calls: which displays are active,
//! and each one's bounds, size, model number and current mode.
//!
//! Declarations under Apple's names, plus [`CGDisplay`], whose methods keep
//! the `core-graphics` crate's names so the winit fork's call sites read as
//! before. A display ID is a plain integer that CoreGraphics validates, so
//! those methods are safe. `tests/cf_cg_sdk.rs` holds every declaration here
//! to the SDK headers with clang, both arches.
//!
//! Behind the `cg` feature: CoreGraphics is a GUI framework, and only the
//! windowing backend should make a binary load it.
//!
//! It replaces the `core-graphics`, `core-graphics-types` and `foreign-types`
//! crates in the vendored winit fork (`docs/AUDIT-third-party-2026-10-15.md`,
//! winit-core step 3).

#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals)]

use std::ptr;

use crate::CGRect;

crate::cf::opaque! { CGDisplayMode }

pub type CGError = i32;
pub type CGDirectDisplayID = u32;
pub type CGDisplayModeRef = *mut CGDisplayMode;

pub const kCGErrorSuccess: CGError = 0;

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
}

fn check(error: CGError) -> Result<(), CGError> {
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
    #[must_use]
    pub const fn new(id: CGDirectDisplayID) -> Self {
        Self { id }
    }

    /// The main display: the one whose origin is the global origin.
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
    #[must_use]
    pub fn bounds(&self) -> CGRect {
        // SAFETY: a by-value ID; see the type's doc.
        unsafe { CGDisplayBounds(self.id) }
    }

    #[must_use]
    pub fn pixels_wide(&self) -> usize {
        // SAFETY: a by-value ID; see the type's doc.
        unsafe { CGDisplayPixelsWide(self.id) }
    }

    #[must_use]
    pub fn pixels_high(&self) -> usize {
        // SAFETY: a by-value ID; see the type's doc.
        unsafe { CGDisplayPixelsHigh(self.id) }
    }

    #[must_use]
    pub fn model_number(&self) -> u32 {
        // SAFETY: a by-value ID; see the type's doc.
        unsafe { CGDisplayModelNumber(self.id) }
    }
}
