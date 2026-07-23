// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

// Platform traits (FontProvider, TextShaper, Clipboard, Notifier, Opener) are
// consumed via `dyn Trait` across terminal, config, GPU, plugins, and FFI.
// The compiler reports trait methods, struct fields, and constructors as "dead"
// because usage is through dynamic dispatch or FFI, not direct Rust calls.
// Keep dead-code suppression at item scope with local justification comments.

//! Platform abstraction traits for portable terminal integration.
//!
//! These traits define platform-specific functionality that varies across
//! operating systems. Each platform (macOS, Linux, Windows) provides its
//! own implementation:
//! | Trait | macOS | Linux | Windows |
//! |-------|-------|-------|---------|
//! | `FontProvider` | Core Text | fontconfig | DirectWrite |
//! | `TextShaper` | Core Text | HarfBuzz | DirectWrite |
//! | `Clipboard` | NSPasteboard | wl-clipboard/xclip | Win32 |
//! | `Notifier` | NSUserNotification | libnotify | Win32 |
//! | `Opener` | NSWorkspace | xdg-open | ShellExecute |
//!
//! # Usage
//!
//! Platform implementations can be provided via:
//! - Native Rust implementations (Linux/Windows)
//! - FFI callbacks from host applications (Swift on macOS)
//!
//! # Example
//!
//! ```text
//! use aterm_core::platform::{FontProvider, FontDescriptor, StubFontProvider};
//!
//! let provider = StubFontProvider::new();
//! let desc = provider.system_monospace();
//! println!("System font: {} {}pt", desc.family, desc.size);
//! ```
#![allow(
    clippy::wildcard_imports,
    reason = "platform facade and child wrappers intentionally share trait surface via glob imports"
)]

use std::path::Path;
use std::sync::Arc;

mod stub;
mod types;

pub use stub::*;
pub use types::*;

// =============================================================================
// Error Types
// =============================================================================

/// Error loading or resolving fonts.
#[derive(Debug, Clone, PartialEq, Eq, aterm_error::Error)]
#[allow(
    dead_code,
    reason = "variants are constructed in FFI wrapper code paths reached via dyn dispatch"
)]
#[non_exhaustive]
pub enum FontError {
    /// Font family not found on the system.
    #[error("font not found: {0}")]
    NotFound(String),
    /// Font file is corrupted or invalid.
    #[error("invalid font data: {0}")]
    InvalidData(String),
}

/// Error accessing the clipboard.
#[derive(Debug, Clone, PartialEq, Eq, aterm_error::Error)]
#[allow(
    dead_code,
    reason = "variants are constructed in FFI wrapper code paths reached via dyn dispatch"
)]
#[non_exhaustive]
pub enum ClipboardError {
    /// Clipboard is not available.
    #[error("clipboard unavailable")]
    Unavailable,
    /// Clipboard operation failed.
    #[error("clipboard error: {0}")]
    SystemError(String),
}

/// Error opening URLs or files.
#[derive(Debug, Clone, PartialEq, Eq, aterm_error::Error)]
#[allow(
    dead_code,
    reason = "variants are constructed in FFI wrapper code paths reached via dyn dispatch"
)]
#[non_exhaustive]
pub enum OpenError {
    /// URL or path is malformed.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    /// File or URL handler not found.
    #[error("no handler for: {0}")]
    NoHandler(String),
    /// Operation failed.
    #[error("open error: {0}")]
    SystemError(String),
}

// Font types (FontDescriptor, FontData, FontMetrics, etc.) in types.rs
// Text shaping types (TextRun, ShapedGlyph) in types.rs

// =============================================================================
// Traits
// =============================================================================

/// Font loading and discovery.
///
/// Implementations provide access to system fonts and font fallback resolution.
#[allow(
    dead_code,
    reason = "trait surface exercised through dyn dispatch from FFI services"
)]
pub trait FontProvider: Send + Sync {
    /// Get the system's default monospace font.
    fn system_monospace(&self) -> FontDescriptor;

    /// Load font data for a descriptor.
    ///
    /// Returns [`FontError::NotFound`] if the font family doesn't exist.
    fn load_font(&self, desc: &FontDescriptor) -> Result<FontData, FontError>;

    /// Find a font that can render the given codepoint.
    ///
    /// Used for fallback when the primary font doesn't support a character.
    /// Returns `None` if no suitable fallback is found.
    fn resolve_fallback(&self, codepoint: char, base: &FontDescriptor) -> Option<FontDescriptor>;
}

/// Text shaping (ligatures, complex scripts).
///
/// Implementations handle text shaping for complex scripts and ligatures.
#[allow(
    dead_code,
    reason = "trait surface exercised through dyn dispatch from FFI services"
)]
pub trait TextShaper: Send + Sync {
    /// Shape a run of text using the given font.
    ///
    /// Returns positioned glyphs ready for rendering.
    fn shape(&self, run: &TextRun, font: &FontData) -> Vec<ShapedGlyph>;
}

/// System clipboard access.
///
/// Implementations provide read/write access to the system clipboard.
pub trait Clipboard: Send + Sync {
    /// Read text from the clipboard.
    ///
    /// Returns `None` if the clipboard is empty or doesn't contain text.
    fn read(&self) -> Option<String>;

    /// Write text to the clipboard.
    fn write(&self, text: &str) -> Result<(), ClipboardError>;

    /// Read from X11 primary selection (Linux only).
    ///
    /// On non-X11 platforms, this falls back to the main clipboard.
    fn read_selection(&self) -> Option<String>;

    /// Write to X11 primary selection (Linux only).
    ///
    /// On non-X11 platforms, this is a no-op.
    fn write_selection(&self, text: &str) -> Result<(), ClipboardError>;
}

/// System notifications.
///
/// Implementations deliver notifications through the platform's notification system.
pub trait Notifier: Send + Sync {
    /// Send a notification.
    fn notify(&self, title: &str, body: &str);

    /// Set the application badge count.
    ///
    /// Pass `None` to clear the badge.
    fn set_badge(&self, count: Option<u32>);

    /// Set the badge format string (Terminal SetBadgeFormat).
    ///
    /// The format string can contain Terminal variables like `\(session.name)`.
    /// Pass empty string to clear the badge format.
    fn set_badge_format(&self, format: &str);

    /// Play the system bell/alert sound.
    fn bell(&self);
}

/// URL and file opening.
///
/// Implementations delegate to the platform's URL/file handler system.
pub trait Opener: Send + Sync {
    /// Open a URL in the default browser.
    fn open_url(&self, url: &str) -> Result<(), OpenError>;

    /// Open a file in the default application.
    fn open_file(&self, path: &Path) -> Result<(), OpenError>;

    /// Reveal a file in the file manager.
    fn reveal_file(&self, path: &Path) -> Result<(), OpenError>;
}

// =============================================================================
// Platform Context
// =============================================================================

/// Collection of platform services.
///
/// Provides a convenient way to pass all platform services together.
#[derive(Clone)]
pub struct PlatformServices {
    /// Font loading service.
    pub fonts: Arc<dyn FontProvider>,
    /// Text shaping service.
    pub shaper: Arc<dyn TextShaper>,
    /// Clipboard service.
    pub clipboard: Arc<dyn Clipboard>,
    /// Notification service.
    pub notifier: Arc<dyn Notifier>,
    /// URL/file opener service.
    pub opener: Arc<dyn Opener>,
}

impl PlatformServices {
    /// Create platform services with stub implementations for unit tests.
    #[cfg(test)]
    pub(crate) fn stub() -> Self {
        Self {
            fonts: Arc::new(StubFontProvider::new()),
            shaper: Arc::new(StubTextShaper),
            clipboard: Arc::new(StubClipboard::new()),
            notifier: Arc::new(StubNotifier),
            opener: Arc::new(StubOpener),
        }
    }
}

impl std::fmt::Debug for PlatformServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlatformServices").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_font_descriptor_default() {
        let desc = FontDescriptor::default();
        assert_eq!(desc.family, "SF Mono");
        assert!(
            (desc.size - 14.0).abs() < f32::EPSILON,
            "default size should be 14.0"
        );
        assert_eq!(desc.weight, 400);
        assert!(!desc.italic);
    }

    #[test]
    fn test_font_descriptor_builder() {
        let desc = FontDescriptor {
            family: "SF Mono".to_string(),
            size: 12.0,
            weight: 400,
            italic: false,
        }
        .with_weight(700)
        .with_italic(true);
        assert_eq!(desc.family, "SF Mono");
        assert!(
            (desc.size - 12.0).abs() < f32::EPSILON,
            "size should be 12.0"
        );
        assert_eq!(desc.weight, 700);
        assert!(desc.italic);
    }

    #[test]
    fn test_font_descriptor_variants() {
        let base = FontDescriptor::default();
        let bold = base.bold();
        let italic = base.italic();

        assert_eq!(bold.weight, 700);
        assert!(!bold.italic);

        assert_eq!(italic.weight, 400);
        assert!(italic.italic);
    }

    #[test]
    fn test_stub_font_provider() {
        let provider = StubFontProvider::new();
        let desc = provider.system_monospace();
        assert_eq!(desc.family, "SF Mono");

        let font = provider.load_font(&desc).unwrap();
        // 14.0 * 0.6 = 8.4
        assert!((font.metrics.cell_width - 8.4).abs() < 0.001);
    }

    #[test]
    fn test_stub_clipboard() {
        let clipboard = StubClipboard::new();
        clipboard.write("test").unwrap();
        assert_eq!(clipboard.read(), Some("test".to_string()));
    }

    #[test]
    fn test_platform_services_stub() {
        let services = PlatformServices::stub();
        let desc = services.fonts.system_monospace();
        assert_eq!(desc.family, "SF Mono");
    }

    #[test]
    fn test_error_display() {
        assert_eq!(
            format!("{}", FontError::NotFound("Arial".to_string())),
            "font not found: Arial"
        );
        assert_eq!(
            format!("{}", FontError::InvalidData("corrupt".to_string())),
            "invalid font data: corrupt"
        );

        assert_eq!(
            format!("{}", ClipboardError::Unavailable),
            "clipboard unavailable"
        );
        assert_eq!(
            format!("{}", ClipboardError::SystemError("fail".to_string())),
            "clipboard error: fail"
        );

        assert_eq!(
            format!("{}", OpenError::InvalidUrl("bad://url".to_string())),
            "invalid URL: bad://url"
        );
        assert_eq!(
            format!("{}", OpenError::NoHandler("/app".to_string())),
            "no handler for: /app"
        );
        assert_eq!(
            format!("{}", OpenError::SystemError("oops".to_string())),
            "open error: oops"
        );
    }
}
