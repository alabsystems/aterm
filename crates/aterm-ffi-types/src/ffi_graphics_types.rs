// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kitty graphics protocol FFI type definitions.
//!
//! Shared `#[repr(C)]` types for the Kitty graphics protocol FFI surface.
//! Extracted from `aterm-core::ffi::graphics::types` to `aterm-types` so that
//! both `aterm-core` and `aterm-core-ffi` can access them without routing
//! through the aterm-core monolith (Part of #2584).

/// Kitty graphics placement location type.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtermKittyPlacementLocation {
    /// Placed at absolute cursor position.
    Absolute = 0,
    /// Virtual placement (for Unicode placeholder mode).
    Virtual = 1,
    /// Relative to another placement.
    Relative = 2,
}

/// Kitty graphics placement structure for FFI.
#[repr(C)]
pub struct AtermKittyPlacement {
    /// Placement ID within the image.
    pub id: u32,
    /// Location type.
    pub location_type: AtermKittyPlacementLocation,
    /// Row position (for Absolute) or parent image ID (for Relative).
    pub row_or_parent_image: u32,
    /// Column position (for Absolute) or parent placement ID (for Relative).
    pub col_or_parent_placement: u32,
    /// Horizontal offset (for Relative placement, in cells).
    pub offset_x: i32,
    /// Vertical offset (for Relative placement, in cells).
    pub offset_y: i32,
    /// Source rectangle x offset (in pixels).
    pub source_x: u32,
    /// Source rectangle y offset (in pixels).
    pub source_y: u32,
    /// Source rectangle width (0 = full image).
    pub source_width: u32,
    /// Source rectangle height (0 = full image).
    pub source_height: u32,
    /// Pixel offset within starting cell, x.
    pub cell_x_offset: u32,
    /// Pixel offset within starting cell, y.
    pub cell_y_offset: u32,
    /// Number of columns to display (0 = auto).
    pub num_columns: u32,
    /// Number of rows to display (0 = auto).
    pub num_rows: u32,
    /// Z-index for stacking (negative = below text).
    pub z_index: i32,
    /// Whether this is a virtual placement.
    pub is_virtual: bool,
}

impl Default for AtermKittyPlacement {
    fn default() -> Self {
        Self {
            id: 0,
            location_type: AtermKittyPlacementLocation::Absolute,
            row_or_parent_image: 0,
            col_or_parent_placement: 0,
            offset_x: 0,
            offset_y: 0,
            source_x: 0,
            source_y: 0,
            source_width: 0,
            source_height: 0,
            cell_x_offset: 0,
            cell_y_offset: 0,
            num_columns: 0,
            num_rows: 0,
            z_index: 0,
            is_virtual: false,
        }
    }
}

/// Kitty graphics image info structure for FFI.
#[repr(C)]
#[derive(Default)]
pub struct AtermKittyImageInfo {
    /// Image ID.
    pub id: u32,
    /// Image number (0 if not assigned).
    pub number: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Number of placements.
    pub placement_count: u32,
}

/// Error codes for graphics FFI operations.
///
/// Following FFI_GUIDELINES.md error code ranges:
/// - 0: Success
/// - 1-9: Null pointer errors
/// - 10-19: Configuration/parameter errors
/// - 20-29: Resource errors
/// - 30+: Domain-specific errors
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtermGraphicsError {
    /// Operation succeeded.
    Ok = 0,

    // Null pointer errors (1-9)
    /// Null terminal pointer passed.
    ErrNullTerminal = 1,
    /// Null output buffer pointer passed.
    ErrNullBuffer = 2,
    /// Null output info pointer passed.
    ErrNullInfo = 3,
    /// Null output pixels pointer passed.
    ErrNullPixels = 4,
    /// Null output count pointer passed.
    ErrNullCount = 5,
    /// Null output placement pointer passed.
    ErrNullPlacement = 6,
    /// Null output pointer passed.
    ErrNullOutput = 7,

    // Configuration/parameter errors (10-19)
    /// Invalid image ID.
    ErrInvalidImageId = 10,
    /// Invalid placement ID.
    ErrInvalidPlacementId = 11,

    // Resource errors (20-29)
    /// Memory allocation failed.
    ErrAllocationFailed = 20,
    /// Image data is empty.
    ErrEmptyImageData = 21,

    // Domain-specific errors (30+)
    /// Image not found.
    ErrImageNotFound = 30,
    /// Placement not found.
    ErrPlacementNotFound = 31,

    // Internal errors (40+)
    /// Internal error (unexpected state or panic).
    ErrInternal = 40,
}
