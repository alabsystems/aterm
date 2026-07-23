// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Builder pattern for `TerminalConfig`.

use aterm_types::{CursorStyle, ParagraphDirection, Rgb};

use super::types::{BiDiConfig, BiDiMode, ScrollbackBackend, TerminalConfig};

/// Builder for `TerminalConfig` with fluent API.
///
/// Provides a convenient way to construct a [`TerminalConfig`] without
/// specifying all 19 fields directly. Unset fields use their defaults.
///
/// # Example
///
/// ```
/// use aterm_core::config::TerminalConfig;
/// use aterm_core::terminal::CursorStyle;
///
/// let config = TerminalConfig::builder()
///     .cursor_style(CursorStyle::SteadyBar)
///     .cursor_blink(false)
///     .scrollback_limit(50_000)
///     .build();
/// ```
#[derive(Debug, Clone)]
pub struct ConfigBuilder {
    config: TerminalConfig,
}

impl Default for ConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigBuilder {
    /// Create a new builder with default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: TerminalConfig::default(),
        }
    }

    /// Set the cursor style.
    #[must_use]
    pub fn cursor_style(mut self, style: CursorStyle) -> Self {
        self.config.cursor_style = style;
        self
    }

    /// Set cursor blink mode.
    #[must_use]
    pub fn cursor_blink(mut self, blink: bool) -> Self {
        self.config.cursor_blink = blink;
        self
    }

    /// Set cursor visibility.
    #[must_use]
    pub fn cursor_visible(mut self, visible: bool) -> Self {
        self.config.cursor_visible = visible;
        self
    }

    /// Set the font family name.
    #[must_use]
    pub fn font_family(mut self, family: impl Into<String>) -> Self {
        self.config.font.family = family.into();
        self
    }

    /// Set default foreground color.
    #[must_use]
    pub fn default_foreground(mut self, color: Rgb) -> Self {
        self.config.default_foreground = color;
        self
    }

    /// Set default background color.
    #[must_use]
    pub fn default_background(mut self, color: Rgb) -> Self {
        self.config.default_background = color;
        self
    }

    /// Set scrollback limit in lines.
    ///
    /// Use [`unlimited_scrollback`](Self::unlimited_scrollback) to remove the
    /// line limit (scrollback is then bounded only by `memory_budget`).
    #[must_use]
    pub fn scrollback_limit(mut self, limit: usize) -> Self {
        self.config.scrollback_limit = Some(limit);
        self
    }

    /// Remove the scrollback line limit.
    ///
    /// With no line limit, scrollback grows until the `memory_budget` is
    /// reached.
    #[must_use]
    pub fn unlimited_scrollback(mut self) -> Self {
        self.config.scrollback_limit = None;
        self
    }

    /// Set auto-wrap mode.
    #[must_use]
    pub fn auto_wrap(mut self, enabled: bool) -> Self {
        self.config.auto_wrap = enabled;
        self
    }

    /// Set focus reporting mode.
    #[must_use]
    pub fn focus_reporting(mut self, enabled: bool) -> Self {
        self.config.focus_reporting = enabled;
        self
    }

    /// Set bracketed paste mode.
    #[must_use]
    pub fn bracketed_paste(mut self, enabled: bool) -> Self {
        self.config.bracketed_paste = enabled;
        self
    }

    /// Set memory budget for scrollback (in bytes).
    #[must_use]
    pub fn memory_budget(mut self, budget: usize) -> Self {
        self.config.memory_budget = budget;
        self
    }

    /// Set synchronized output timeout in milliseconds.
    #[must_use]
    pub fn sync_timeout_ms(mut self, timeout: u64) -> Self {
        self.config.sync_timeout_ms = timeout;
        self
    }

    /// Set the scrollback storage backend.
    #[must_use]
    pub fn scrollback_backend(mut self, backend: ScrollbackBackend) -> Self {
        self.config.scrollback_backend = backend;
        self
    }

    /// Set the full BiDi configuration.
    #[must_use]
    pub fn bidi(mut self, config: BiDiConfig) -> Self {
        self.config.bidi = config;
        self
    }

    /// Set the BiDi mode.
    #[must_use]
    pub fn bidi_mode(mut self, mode: BiDiMode) -> Self {
        self.config.bidi.mode = mode;
        self
    }

    /// Set the default BiDi paragraph direction.
    #[must_use]
    pub fn bidi_direction(mut self, direction: ParagraphDirection) -> Self {
        self.config.bidi.direction = direction;
        self
    }

    /// Set whether to reorder non-spacing marks in BiDi text.
    #[must_use]
    pub fn bidi_reorder_nsm(mut self, reorder: bool) -> Self {
        self.config.bidi.reorder_nsm = reorder;
        self
    }

    /// Build the configuration.
    #[must_use]
    pub fn build(self) -> TerminalConfig {
        self.config
    }
}
