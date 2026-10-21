// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shared types for the aterm terminal engine: primitives (`Rgb`, `CursorStyle`, `BiDiMode`,
//! `ColorPalette`, `TerminalSize`) and the cross-crate vocabulary for domain, input, keyboard,
//! mouse, selection, text shaping, terminal hosting, themes and the control socket.

#![warn(missing_docs)]
// Registers the `trust` tool so `#[trust::…]` attributes resolve under the Trust verifier.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

mod mutex_ext;
pub use mutex_ext::MutexExt;

// Formatting helpers that avoid runtime-argument `format_args!`, which the
// Trust native lowering cannot model (see module docs).
mod trust_fmt;

// `dev_seam!` — the one reader of a development-only environment variable.
pub mod dev_seam;
// The argv0 aliases of the one `aterm` binary: the list the bundle lays and every
// Rust reader shares.
pub mod argv0;
pub mod cli_path;

pub mod identity;
// The need words (`docs/DESIGN-owner-surfaces-2026-10-16.md` §2.2): what a
// session needs of its owner, one vocabulary for the server and its readers.
pub mod needs;
pub mod version;

mod cursor;

// Grid indexing types (Line, Column, Point, etc.).
pub mod index;

#[macro_use]
mod bitflags;

pub mod dirs;

mod time;
pub use time::duration_to_nanos;

// Civil-calendar math (Howard Hinnant's) and the RFC 3339 UTC stamp built on it: the one
// workspace home for that shape.
pub mod rfc3339;

// Rust on this machine means the Trust toolchain: the stock names, their Trust spellings, and
// the reader that finds a stock tool at command position, for atpkg's reroute,
// `aterm pkg lane` and `aterm help rust`.
pub mod rust_lane;

pub mod app_inspection;
mod color_palette;
// The request line's grammar (selector, verb, rest): the server's lanes, relay
// and dispatch and every client's reply framing read a line through it.
pub mod control_request;
pub mod control_socket;
pub mod control_verbs;
// The fabric's word grammar (principal, topic, kind, verdict, `via=`): the
// bridge, the endpoint and the supervisor each hold a line to it and none
// depends on another.
pub mod fabric_grammar;
// `[fabric] command`'s two spellings (a whitespace-split line, or an argv
// array), read alike by the bridge launcher and `aterm fabric`.
pub mod fabric_command;
pub mod fs_restricted;
pub mod pe_image;
// The inline-image payload: shared vocabulary because the live grid and the history line model
// must hold the same `Arc`, and neither of those crates can see the other's types.
mod inline_image;
pub mod scheme;
// `aterm theme import`: offline converters from other terminals' theme formats
// into the native theme file the window reads.
pub mod scheme_import;
// The one write path an import takes into the themes folder (CLI and window).
pub mod scheme_install;
// A theme's typography (font families and type knobs) and its value readers.
pub mod scheme_typography;
// The signature bar (OkLCh hue, chroma and distance rows): the built-ins'
// law test and `aterm theme check --signature` share it.
pub mod scheme_signature;
// The 256-colour palette (slots 16-255) a theme derives from its ANSI
// colours when it opts in with `palette_generate`.
pub mod scheme_palette256;
mod x11_colors;
pub use color_palette::ColorPalette;
pub use cursor::CursorStyle;
pub use inline_image::{
    ImageData, ImageFormat, ImageScaling, KittyPlacementTag, SourceRect, layer_order,
};
// Named color schemes ("theme palettes"): the single source for terminal colors.
pub use scheme::{Appearance, ColorScheme, ThemeParts};

mod clipboard;
pub mod domain;
mod env_sanitize;
pub mod input;
pub mod keyboard;
mod kitty_keyboard;
pub mod mouse;
// A window's X11 or Wayland handles, from the windowing crate to the GL arm
// and the CPU presenters.
pub mod native_window;
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

/// Default `faint_opacity`: the fraction of the foreground's contrast SGR 2
/// (dim/faint) keeps, in log space. Faint is a blend toward the cell
/// background (for light ink as far as an sRGB alpha blend), raised to a
/// readable floor where needed; see
/// `aterm_core::terminal::color_resolve::dim_toward_bg`.
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

/// The engine's default foreground: ANSI "white", `#E5E5E5` (the index-7 grey, not pure
/// white). The terminal state (`aterm_core::terminal::transient_state`) and
/// `TerminalConfig::default()` both take it, so a bare `Terminal::new()` and a default config
/// agree. The windowed GUI applies its themed [`ColorScheme`] instead (fg `#D0D0D0`).
pub const DEFAULT_FOREGROUND: Rgb = Rgb::new(229, 229, 229);

/// The engine's default background, black, shared like [`DEFAULT_FOREGROUND`]. The GUI's
/// theme uses `#111318`.
pub const DEFAULT_BACKGROUND: Rgb = Rgb::new(0, 0, 0);

impl Rgb {
    /// Create a new RGB color.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// The WCAG contrast ratio between two colors.
    #[must_use]
    // Skip: f64 arithmetic, the float-interval lane.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn contrast(self, other: Self) -> f64 {
        // `luminance` is in [0, 1]; the clamp is identity but gives the Trust gate the bound.
        let lum1 = self.luminance().clamp(0.0, 1.0);
        let lum2 = other.luminance().clamp(0.0, 1.0);
        let (lighter, darker) = if lum1 > lum2 {
            (lum1, lum2)
        } else {
            (lum2, lum1)
        };
        // Re-clamped at the arithmetic site (identity) so the Trust gate sees the operand
        // bounds locally: no overflow to infinity, a divisor >= 0.05.
        let lighter = lighter.clamp(0.0, 1.0);
        let darker = darker.clamp(0.0, 1.0);
        (lighter + 0.05) / (darker + 0.05)
    }

    /// Calculate relative luminance per WCAG 2.0.
    fn luminance(self) -> f64 {
        // Skip: `powf` is an inexact float intrinsic, which Trust refuses by design. No
        // `[f64; 256]` table replaces it: `aterm_messages::ink::contrast` transcribes this body
        // and is pinned to it bit for bit, so a table here would need a second copy there.
        #[cfg_attr(trust_verify, trust::skip)]
        fn linearize(c: u8) -> f64 {
            let c = f64::from(c) / 255.0;
            let l = if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            };
            // Identity on [0, 1], but it makes the range explicit to the Trust gate, for which
            // the `powf` result is opaque.
            l.clamp(0.0, 1.0)
        }
        // Re-clamped here (identity) so the bound sits in the frame where the Trust gate
        // evaluates the weighted sum: each product <= 1, no overflow to infinity.
        let r = linearize(self.r).clamp(0.0, 1.0);
        let g = linearize(self.g).clamp(0.0, 1.0);
        let b = linearize(self.b).clamp(0.0, 1.0);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }
}

impl std::ops::Mul<f32> for Rgb {
    type Output = Self;

    // Skip: f32 multiply, clamp and saturating cast, the float-interval lane
    // (docs/TRUST_100PCT_PLAN.md). Delete it when a promoted seal proves this body.
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

    #[test]
    fn cursor_style_from_decscusr_param() {
        // Param 0 is BlinkingBlock, as 1 is (DECSCUSR).
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
