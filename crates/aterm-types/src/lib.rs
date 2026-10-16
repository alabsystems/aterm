// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shared types for the aterm terminal engine.
//!
//! Provides primitive types (`Rgb`, `CursorStyle`, `BiDiMode`, `ColorPalette`,
//! `TerminalSize`) and cross-crate type definitions for domain, input,
//! keyboard, mouse, selection, GPU views, text shaping, terminal hosting,
//! and perception.

#![warn(missing_docs)]
// Trust tool-attribute plumbing (scrollback/lz4 pattern) for #[trust::contract_panic].
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

mod mutex_ext;
pub use mutex_ext::MutexExt;

// Formatting helpers that avoid runtime-argument `format_args!`, which the
// Trust native lowering cannot model (see module docs).
mod trust_fmt;

// `dev_seam!` — the one reader of a development-only environment variable.
pub mod dev_seam;

pub mod identity;
pub mod version;

// ============================================================================
// Domain types — owned by this crate
// ============================================================================
mod cursor;

// Grid indexing types (Line, Column, Point, etc.).
pub mod index;

#[macro_use]
mod bitflags;

pub mod dirs;

mod time;
pub use time::duration_to_nanos;

// Howard Hinnant civil-calendar math + the RFC3339 UTC stamp built on it —
// the one workspace home (update, release, gui, and atpkg all stamp this shape).
pub mod rfc3339;

// Rust on this machine means the Trust toolchain: the stock names, their Trust
// spellings, and the reader that finds a stock tool at command position — one
// vocabulary for atpkg's reroute, `aterm pkg lane` and `aterm help rust`.
pub mod rust_lane;

pub mod app_inspection;
mod color_palette;
pub mod control_socket;
pub mod control_verbs;
// The fabric's word grammar (principal, topic, kind, verdict, `via=`): the
// bridge, the endpoint and the supervisor each hold a line to it and none
// depends on another.
pub mod fabric_grammar;
pub mod fs_restricted;
// The inline-image payload: shared vocabulary because the live grid and the
// history line model must hold the SAME `Arc`, and neither of those crates can
// see the other's types (see the module docs).
mod inline_image;
pub mod scheme;
// `aterm theme import`: offline converters from other terminals' theme formats
// into the native theme file the window reads.
pub mod scheme_import;
// The one write path an import takes into the themes folder (CLI and window).
pub mod scheme_install;
// The signature bar (OkLCh hue, chroma and distance rows): the built-ins'
// law test and `aterm theme check --signature` share it.
pub mod scheme_signature;
mod x11_colors;
pub use color_palette::ColorPalette;
pub use cursor::CursorStyle;
pub use inline_image::{
    ImageData, ImageFormat, ImageScaling, KittyPlacementTag, SourceRect, layer_order,
};
// Named color schemes ("theme palettes"): the single source for terminal colors.
pub use scheme::{Appearance, ColorScheme, ThemeParts};

mod buffer_access;
pub mod buffer_command;
mod clipboard;
pub mod domain;
mod env_sanitize;
pub mod input;
pub mod keyboard;
mod kitty_keyboard;
pub mod mouse;
pub mod pipeline_timestamps;
pub use pipeline_timestamps::PipelineTimestamps;
mod callback_types;
pub mod charset;
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
mod search_content;
pub use buffer_access::BufferAccess;
pub use buffer_command::BufferCommand;
pub use callback_types::{
    AdvancedNotificationCallback, ClipboardCallback, ColorChangeCallback, ColorChangeOp,
    NotificationCallback, WindowCallback,
};
pub use osc::{
    Iterm2ShellIntegrationVersion, Notification, NotificationUrgency, RemoteHost, TaskbarProgress,
};
pub use search_content::SearchContent;

// ============================================================================
// Terminal Size
// ============================================================================

/// Terminal dimensions (rows and columns).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TerminalSize {
    rows: u16,
    cols: u16,
}

impl TerminalSize {
    /// Create a new terminal size.
    #[must_use]
    pub const fn new(rows: u16, cols: u16) -> Self {
        Self { rows, cols }
    }

    /// Number of rows.
    #[must_use]
    pub const fn rows(&self) -> u16 {
        self.rows
    }

    /// Number of columns.
    #[must_use]
    pub const fn cols(&self) -> u16 {
        self.cols
    }
}

// ============================================================================
// SGR Dim (Faint) Factor
// ============================================================================

/// Dim (SGR faint) brightness multiplier applied to each RGB channel.
///
/// Centralized here so all render paths (CPU color resolve, FFI cell export,
/// GPU shader logic) share one authoritative value. The WGSL shader cannot
/// import Rust constants — keep its inline `0.5` in sync manually.
///
/// aterm uses 0.5; Alacritty upstream uses 0.66.
pub const DIM_FACTOR: f32 = 0.5;

// ============================================================================
// RGB Color Type
// ============================================================================
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

/// The terminal's default foreground — the VT/engine spec default (ANSI "white",
/// `#E5E5E5`, the index-7 grey, NOT pure white). This is the SINGLE source of truth
/// for the unstyled foreground: both the runtime terminal state
/// (`aterm_core::terminal::transient_state`) and `TerminalConfig::default()` derive
/// their default fg from it, so a bare `Terminal::new()` and an applied default
/// config can never disagree.
///
/// NOTE: this is the *engine* default. The windowed GUI deliberately overrides it
/// with its themed [`ColorScheme`] (default fg `#D0D0D0`) via the GUI's
/// `applied_terminal_config()`, so end users see the theme, not this value.
pub const DEFAULT_FOREGROUND: Rgb = Rgb::new(229, 229, 229);

/// The terminal's default background — the VT/engine spec default (black). Single
/// source of truth, mirrored by `transient_state` and `TerminalConfig::default()`
/// (see [`DEFAULT_FOREGROUND`]). The GUI overrides it with the theme bg (`#111318`).
pub const DEFAULT_BACKGROUND: Rgb = Rgb::new(0, 0, 0);

impl Rgb {
    /// Create a new RGB color.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Calculate contrast ratio between two colors.
    ///
    /// Returns the WCAG relative luminance contrast ratio.
    #[must_use]
    // Skip: f64 arithmetic — the float-interval lane.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn contrast(self, other: Self) -> f64 {
        // `luminance` is mathematically in [0, 1]; the explicit clamp is identity
        // on every real value but gives the Trust gate a finite bound so the
        // `+ 0.05` additions provably cannot overflow to infinity.
        let lum1 = self.luminance().clamp(0.0, 1.0);
        let lum2 = other.luminance().clamp(0.0, 1.0);
        let (lighter, darker) = if lum1 > lum2 {
            (lum1, lum2)
        } else {
            (lum2, lum1)
        };
        // Re-clamp at the arithmetic site: `clamp` is idempotent, so this is
        // exactly the values selected above (already clamped to [0, 1]) — but
        // it puts the bound on the operands the Trust gate reasons about
        // locally, so `+ 0.05` provably cannot overflow to infinity and the
        // divisor is provably >= 0.05 (no division by zero).
        let lighter = lighter.clamp(0.0, 1.0);
        let darker = darker.clamp(0.0, 1.0);
        (lighter + 0.05) / (darker + 0.05)
    }

    /// Calculate relative luminance per WCAG 2.0.
    fn luminance(self) -> f64 {
        // Skip, standing: `powf` is an inexact float intrinsic, which Trust
        // refuses to admit by design (std documents its precision as
        // non-deterministic; trust d1a7b27696 and c065186961). No seal will
        // discharge this body as written. No `[f64; 256]` table replaces it:
        // `aterm_messages::ink::contrast` transcribes this body token for token
        // and is pinned to it bit for bit (ruling 324) with no aterm-types
        // dependency, so a table here needs a second copy there or leaves the
        // pin resting on the host's `pow` matching the table's.
        #[cfg_attr(trust_verify, trust::skip)]
        fn linearize(c: u8) -> f64 {
            let c = f64::from(c) / 255.0;
            let l = if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            };
            // sRGB linearization maps [0, 1] onto [0, 1]; the clamp is identity
            // on every real input but makes the range explicit so the weighted
            // sum below provably cannot overflow to infinity (the `powf` result
            // is otherwise opaque to the Trust gate).
            l.clamp(0.0, 1.0)
        }
        // Re-clamp at the call site: `linearize` already returns a value
        // clamped to [0, 1] and `clamp` is idempotent, so these are the exact
        // same values — but the bound now sits in this frame, where the Trust
        // gate evaluates the weighted sum, so each product is provably <= 1
        // and the adds provably cannot overflow to infinity.
        let r = linearize(self.r).clamp(0.0, 1.0);
        let g = linearize(self.g).clamp(0.0, 1.0);
        let b = linearize(self.b).clamp(0.0, 1.0);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }
}

impl std::ops::Mul<f32> for Rgb {
    type Output = Self;

    // Skip: f32 multiply, clamp and saturating cast, no transcendental — the
    // float-interval lane (docs/TRUST_100PCT_PLAN.md, wave 3b: "float-clamp
    // intervals", blocked on native float theory in $HOME/trust). Delete it when a
    // promoted seal proves this body.
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

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            r: self.r.saturating_add(rhs.r),
            g: self.g.saturating_add(rhs.g),
            b: self.b.saturating_add(rhs.b),
        }
    }
}

// ============================================================================
// BiDi Mode
// ============================================================================

/// BiDi (Bidirectional Text) display mode.
///
/// These modes align with the Terminal Working Group BiDi specification:
/// <https://terminal-wg.pages.freedesktop.org/bidi/>
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

    // =========================================================================
    // Rgb
    // =========================================================================

    // =========================================================================
    // CursorStyle
    // =========================================================================

    #[test]
    fn cursor_style_from_param_valid() {
        // Param 0 maps to BlinkingBlock (same as 1 per DECSCUSR)
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
    }

    #[test]
    fn cursor_style_from_param_invalid() {
        assert_eq!(CursorStyle::from_param(7), None);
        assert_eq!(CursorStyle::from_param(255), None);
        assert_eq!(CursorStyle::from_param(u16::MAX), None);
    }
}
