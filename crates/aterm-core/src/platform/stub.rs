// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Stub implementations of platform traits for testing.
//!
//! [`StubTextShaper`] is available in production builds. All other stubs
//! (`StubFontProvider`, `StubClipboard`, `StubNotifier`, `StubOpener`) are
//! gated behind `#[cfg(test)]`.

#[allow(
    clippy::wildcard_imports,
    reason = "internal stub module re-uses parent traits"
)]
use super::*;

// =============================================================================
// Stub Text Shaper (production)
// =============================================================================

/// Stub text shaper for testing.
///
/// Returns simple 1:1 glyph mapping without actual shaping.
pub struct StubTextShaper;

impl TextShaper for StubTextShaper {
    fn shape(&self, run: &TextRun, font: &FontData) -> Vec<ShapedGlyph> {
        // Simple 1:1 mapping without real shaping
        run.text
            .char_indices()
            .map(|(byte_idx, c)| {
                let cluster = u32::try_from(byte_idx).unwrap_or(u32::MAX);
                ShapedGlyph {
                    font_id: ShapedGlyph::FONT_PRIMARY,
                    glyph_id: c as u32,
                    cluster,
                    x_offset: 0.0,
                    y_offset: 0.0,
                    x_advance: font.metrics.cell_width,
                    y_advance: 0.0,
                }
            })
            .collect()
    }
}

// =============================================================================
// Test-only stubs
// =============================================================================

#[cfg(test)]
mod test_stubs {
    use super::*;
    use std::sync::Mutex;

    // =========================================================================
    // Stub Font Provider
    // =========================================================================

    /// Stub font provider for testing.
    ///
    /// Returns mock font data suitable for unit tests.
    pub struct StubFontProvider {
        /// Default monospace font descriptor.
        default_font: FontDescriptor,
    }

    impl StubFontProvider {
        /// Create a new stub font provider.
        pub(crate) fn new() -> Self {
            Self {
                default_font: FontDescriptor::default(),
            }
        }
    }

    impl Default for StubFontProvider {
        fn default() -> Self {
            Self::new()
        }
    }

    impl FontProvider for StubFontProvider {
        fn system_monospace(&self) -> FontDescriptor {
            self.default_font.clone()
        }

        fn load_font(&self, desc: &FontDescriptor) -> Result<FontData, FontError> {
            // Return mock font metrics based on size
            let cell_width = desc.size * 0.6;
            let line_height = desc.size * 1.2;

            Ok(FontData {
                descriptor: desc.clone(),
                data: FontDataKind::Handle(0),
                metrics: FontMetrics {
                    line_height,
                    cell_width,
                    ascent: desc.size * 0.8,
                    descent: desc.size * 0.2,
                    leading: 0.0,
                    underline_position: desc.size * 0.1,
                    underline_thickness: 1.0,
                },
            })
        }

        fn resolve_fallback(
            &self,
            _codepoint: char,
            base: &FontDescriptor,
        ) -> Option<FontDescriptor> {
            // Always return a fallback for testing
            Some(FontDescriptor {
                family: "Apple Color Emoji".to_string(),
                size: base.size,
                weight: 400,
                italic: false,
            })
        }
    }

    // =========================================================================
    // Stub Clipboard
    // =========================================================================

    /// Stub clipboard for testing.
    ///
    /// Stores clipboard content in memory.
    pub struct StubClipboard {
        content: Mutex<Option<String>>,
        selection: Mutex<Option<String>>,
    }

    impl StubClipboard {
        /// Create a new empty stub clipboard.
        pub(crate) fn new() -> Self {
            Self {
                content: Mutex::new(None),
                selection: Mutex::new(None),
            }
        }
    }

    impl Default for StubClipboard {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Clipboard for StubClipboard {
        fn read(&self) -> Option<String> {
            self.content
                .lock()
                .expect("clipboard mutex poisoned")
                .clone()
        }

        fn write(&self, text: &str) -> Result<(), ClipboardError> {
            *self.content.lock().expect("clipboard mutex poisoned") = Some(text.to_string());
            Ok(())
        }

        fn read_selection(&self) -> Option<String> {
            self.selection
                .lock()
                .expect("selection mutex poisoned")
                .clone()
        }

        fn write_selection(&self, text: &str) -> Result<(), ClipboardError> {
            *self.selection.lock().expect("selection mutex poisoned") = Some(text.to_string());
            Ok(())
        }
    }

    // =========================================================================
    // Stub Notifier
    // =========================================================================

    /// Stub notifier for testing.
    ///
    /// Discards all notifications silently.
    pub struct StubNotifier;

    impl Notifier for StubNotifier {
        fn notify(&self, _title: &str, _body: &str) {
            // No-op in stub
        }

        fn set_badge(&self, _count: Option<u32>) {
            // No-op in stub
        }

        fn set_badge_format(&self, _format: &str) {
            // No-op in stub
        }

        fn bell(&self) {
            // No-op in stub
        }
    }

    // =========================================================================
    // Stub Opener
    // =========================================================================

    /// Stub opener for testing.
    ///
    /// Records open attempts without actually opening anything.
    pub struct StubOpener;

    impl Opener for StubOpener {
        fn open_url(&self, _url: &str) -> Result<(), OpenError> {
            // No-op in stub
            Ok(())
        }

        fn open_file(&self, _path: &Path) -> Result<(), OpenError> {
            // No-op in stub
            Ok(())
        }

        fn reveal_file(&self, _path: &Path) -> Result<(), OpenError> {
            // No-op in stub
            Ok(())
        }
    }
}

#[cfg(test)]
pub use test_stubs::{StubClipboard, StubFontProvider, StubNotifier, StubOpener};

#[cfg(test)]
#[path = "../../test_support/platform/stub_tests.rs"]
mod tests;
