// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal OSC 1337 reporting and appearance types.

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::Rgb;

/// Terminal SetColors request (OSC 1337 SetColors).
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Iterm2SetColor {
    /// Color key (e.g., "fg", "bg", "black", "br_white").
    pub key: String,
    /// Raw value string (color spec, possibly with colorspace prefix).
    pub value: String,
    /// Parsed RGB color if the value was recognized.
    pub color: Option<Rgb>,
    /// Optional colorspace prefix (e.g., "p3", "srgb").
    pub color_space: Option<String>,
}

/// Terminal cell size response (OSC 1337 ReportCellSize).
///
/// Values are in **points** (not pixels), with an optional scale factor.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Iterm2CellSize {
    /// Cell height in points.
    pub height_points: f32,
    /// Cell width in points.
    pub width_points: f32,
    /// Optional scale factor (e.g., 2.0 for Retina).
    pub scale: Option<f32>,
}

impl Iterm2CellSize {
    /// Create a cell size response without a scale factor.
    pub fn new(height_points: f32, width_points: f32) -> Self {
        Self {
            height_points,
            width_points,
            scale: None,
        }
    }

    /// Create a cell size response with an explicit scale factor.
    pub fn with_scale(height_points: f32, width_points: f32, scale: f32) -> Self {
        Self {
            height_points,
            width_points,
            scale: Some(scale),
        }
    }
}

/// Terminal shell integration version report (OSC 1337 ShellIntegrationVersion).
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Iterm2ShellIntegrationVersion {
    /// Shell integration version number (Pn).
    pub version: u32,
    /// Optional shell name (Ps).
    pub shell: Option<String>,
}

impl Iterm2ShellIntegrationVersion {
    /// Create a new shell integration version record.
    pub fn new(version: u32, shell: Option<String>) -> Self {
        Self { version, shell }
    }
}
