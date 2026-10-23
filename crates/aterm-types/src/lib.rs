// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shared types and cross-crate vocabulary for the aterm terminal engine.

#![warn(missing_docs)]
// Registers the `trust` tool so `#[trust::…]` attributes resolve under the Trust verifier.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

mod mutex_ext;
pub use mutex_ext::MutexExt;

mod trust_fmt;

pub mod dev_seam;

pub mod argv0;
pub mod channel;
pub mod cli_path;

pub mod identity;
pub mod needs;
pub mod version;

mod cursor;

pub mod index;

#[macro_use]
mod bitflags;

pub mod dirs;

mod time;
pub use time::duration_to_nanos;

pub mod rfc3339;
pub mod rust_lane;

pub mod app_inspection;
mod color_palette;
pub mod control_request;
pub mod control_socket;
pub mod control_verbs;
pub mod fabric_grammar;

pub mod fabric_command;
pub mod fs_restricted;
pub mod pe_image;

mod inline_image;
pub mod scheme;
pub mod scheme_import;
pub mod scheme_install;
pub mod scheme_typography;

pub mod scheme_signature;

pub mod scheme_palette256;
mod x11_colors;
pub use color_palette::ColorPalette;
pub use cursor::CursorStyle;
pub use inline_image::{
    ImageData, ImageFormat, ImageScaling, KittyPlacementTag, SourceRect, layer_order,
};
pub use scheme::{Appearance, ColorScheme, ThemeParts};

mod clipboard;
pub mod domain;
mod env_sanitize;
pub mod input;
pub mod keyboard;
mod kitty_keyboard;
pub mod mouse;
pub mod native_window;
pub mod pipeline_timestamps;
pub use pipeline_timestamps::PipelineTimestamps;
mod callback_types;
pub mod charset;
pub mod macos_setup;
pub mod paragraph_direction;
pub mod selection;
mod shell_blocks;
mod shell_types;
mod terminal_core;
pub mod terminal_host;
mod terminal_modes;
pub mod text_shaping;
pub mod vt_level;
mod window;
mod xterm_keyboard;
pub use charset::{
    CharacterSet, CharacterSet96, CharacterSetState, GlMapping, GrMapping, SingleShift,
};
pub use clipboard::{ClipboardOperation, ClipboardSelection};
pub use kitty_keyboard::{
    KittyKeyboardFlags, KittyKeyboardState, KittyKeyboardStateSnapshot, ScreenBuffer,
};
pub use paragraph_direction::ParagraphDirection;
pub use shell_blocks::{BlockState, OutputBlock};
pub use shell_types::{Annotation, CommandMark, TerminalMark, current_time_ms};
pub use terminal_core::TerminalCapabilities;
pub use terminal_modes::TerminalModes;
pub use vt_level::VtLevel;
pub use window::{WindowOperation, WindowResponse};
pub use xterm_keyboard::XtermKeyboardState;
pub mod osc;
pub use callback_types::{
    AdvancedNotificationCallback, ClipboardCallback, NotificationCallback, WindowCallback,
};
pub use osc::{
    Iterm2ShellIntegrationVersion, Notification, NotificationUrgency, RemoteHost, TaskbarProgress,
};

/// Terminal dimensions (rows and columns).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TerminalSize {
    rows: u16,
    cols: u16,
}

impl TerminalSize {
    /// Create a new terminal size.
    #[inline]
    #[must_use]
    pub const fn new(rows: u16, cols: u16) -> Self {
        Self { rows, cols }
    }

    /// Number of rows.
    #[inline]
    #[must_use]
    pub const fn rows(&self) -> u16 {
        self.rows
    }

    /// Number of columns.
    #[inline]
    #[must_use]
    pub const fn cols(&self) -> u16 {
        self.cols
    }
}

/// Default `faint_opacity`: the fraction of foreground contrast SGR 2 (dim/faint) keeps in log
/// space.
pub const DIM_FACTOR: f32 = 0.5;

/// RGB color value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Rgb {
    /// Red component (0-255).
    pub r: u8,
    /// Green component (0-255).
    pub g: u8,
    /// Blue component (0-255).
    pub b: u8,
}

/// Default engine foreground (`#E5E5E5`, ANSI index-7 grey).
pub const DEFAULT_FOREGROUND: Rgb = Rgb::new(229, 229, 229);

/// Default engine background (`#000000`).
pub const DEFAULT_BACKGROUND: Rgb = Rgb::new(0, 0, 0);

impl Rgb {
    /// Create a new RGB color.
    #[inline]
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// The WCAG contrast ratio between two colors.
    #[must_use]
    // Skip: f64 arithmetic, the float-interval lane.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn contrast(self, other: Self) -> f64 {
        let lum1 = self.luminance().clamp(0.0, 1.0);
        let lum2 = other.luminance().clamp(0.0, 1.0);
        let (lighter, darker) = if lum1 > lum2 {
            (lum1, lum2)
        } else {
            (lum2, lum1)
        };
        let lighter = lighter.clamp(0.0, 1.0);
        let darker = darker.clamp(0.0, 1.0);
        (lighter + 0.05) / (darker + 0.05)
    }

    /// Calculate relative luminance per WCAG 2.0.
    fn luminance(self) -> f64 {
        // Skip: `powf` is an inexact float intrinsic, the float-interval lane.
        #[cfg_attr(trust_verify, trust::skip)]
        fn linearize(c: u8) -> f64 {
            let c = f64::from(c) / 255.0;
            let l = if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            };
            l.clamp(0.0, 1.0)
        }
        let r = linearize(self.r).clamp(0.0, 1.0);
        let g = linearize(self.g).clamp(0.0, 1.0);
        let b = linearize(self.b).clamp(0.0, 1.0);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }
}

impl std::ops::Mul<f32> for Rgb {
    type Output = Self;

    // Skip: f32 multiply, clamp and saturating cast, the float-interval lane.
    #[inline]
    #[cfg_attr(trust_verify, trust::skip)]
    fn mul(self, rhs: f32) -> Self::Output {
        Self {
            r: (f32::from(self.r) * rhs).clamp(0.0, 255.0) as u8,
            g: (f32::from(self.g) * rhs).clamp(0.0, 255.0) as u8,
            b: (f32::from(self.b) * rhs).clamp(0.0, 255.0) as u8,
        }
    }
}

impl std::ops::Add for Rgb {
    type Output = Self;

    #[inline]
    fn add(self, rhs: Self) -> Self::Output {
        Self {
            r: self.r.saturating_add(rhs.r),
            g: self.g.saturating_add(rhs.g),
            b: self.b.saturating_add(rhs.b),
        }
    }
}

/// BiDi (Bidirectional Text) display mode.
#[non_exhaustive]
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum BiDiMode {
    /// BiDi processing disabled. Text displays in logical order.
    Disabled = 0,
    /// Implicit BiDi mode (default). Each line is automatically analyzed.
    #[default]
    Implicit = 1,
    /// Explicit BiDi mode. Application controls direction via escape sequences.
    Explicit = 2,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_style_from_decscusr_param() {
        assert_eq!(CursorStyle::from_param(0), Some(CursorStyle::BlinkingBlock));
        assert_eq!(CursorStyle::from_param(1), Some(CursorStyle::BlinkingBlock));
        assert_eq!(CursorStyle::from_param(2), Some(CursorStyle::SteadyBlock));
        assert_eq!(
            CursorStyle::from_param(3),
            Some(CursorStyle::BlinkingUnderline)
        );
        assert_eq!(
            CursorStyle::from_param(4),
            Some(CursorStyle::SteadyUnderline)
        );
        assert_eq!(CursorStyle::from_param(5), Some(CursorStyle::BlinkingBar));
        assert_eq!(CursorStyle::from_param(6), Some(CursorStyle::SteadyBar));
        for invalid in [7, 255, u16::MAX] {
            assert_eq!(CursorStyle::from_param(invalid), None, "{invalid}");
        }
    }
}
