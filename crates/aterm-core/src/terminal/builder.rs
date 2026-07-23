// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal builder.
//!
//! Extracted from `terminal/mod.rs` as part of #485 (code health - large files refactor).

use std::sync::Arc;

use crate::grid::Grid;
use crate::platform::FontDescriptor;
use crate::scrollback::Scrollback;

use super::Terminal;
use aterm_types::Rgb;

/// Builder for creating [`Terminal`] instances with custom configuration.
///
/// Provides a fluent API for configuring terminal options before construction.
///
/// # Example
///
/// ```
/// use aterm_core::terminal::TerminalBuilder;
///
/// let terminal = TerminalBuilder::new()
///     .rows(24)
///     .cols(80)
///     .ring_buffer_size(10_000)
///     .foreground(aterm_core::terminal::Rgb { r: 255, g: 255, b: 255 })
///     .background(aterm_core::terminal::Rgb { r: 0, g: 0, b: 0 })
///     .build();
/// ```
#[derive(Debug)]
pub struct TerminalBuilder {
    rows: u16,
    cols: u16,
    ring_buffer_size: Option<usize>,
    scrollback: Option<Scrollback>,
    foreground: Option<Rgb>,
    background: Option<Rgb>,
    font: Option<FontDescriptor>,
    title: Option<Arc<str>>,
}

impl Default for TerminalBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalBuilder {
    /// Create a new terminal builder with default settings.
    ///
    /// Defaults: 24 rows, 80 cols, no scrollback, default colors.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rows: 24,
            cols: 80,
            ring_buffer_size: None,
            scrollback: None,
            foreground: None,
            background: None,
            font: None,
            title: None,
        }
    }

    /// Set the number of rows.
    #[must_use]
    pub fn rows(mut self, rows: u16) -> Self {
        self.rows = rows;
        self
    }

    /// Set the number of columns.
    #[must_use]
    pub fn cols(mut self, cols: u16) -> Self {
        self.cols = cols;
        self
    }

    /// Set the terminal size (rows and cols).
    #[must_use]
    pub fn size(mut self, rows: u16, cols: u16) -> Self {
        self.rows = rows;
        self.cols = cols;
        self
    }

    /// Set the ring buffer size for in-memory scrollback.
    ///
    /// If not set, the terminal will not have a ring buffer scrollback.
    #[must_use]
    pub fn ring_buffer_size(mut self, size: usize) -> Self {
        self.ring_buffer_size = Some(size);
        self
    }

    /// Set the tiered scrollback storage.
    ///
    /// If not set, the terminal will not have tiered scrollback.
    #[must_use]
    pub fn scrollback(mut self, scrollback: Scrollback) -> Self {
        self.scrollback = Some(scrollback);
        self
    }

    /// Set the default foreground color.
    #[must_use]
    pub fn foreground(mut self, color: Rgb) -> Self {
        self.foreground = Some(color);
        self
    }

    /// Set the default background color.
    #[must_use]
    pub fn background(mut self, color: Rgb) -> Self {
        self.background = Some(color);
        self
    }

    /// Set the initial font descriptor.
    #[must_use]
    pub fn font(mut self, font: FontDescriptor) -> Self {
        self.font = Some(font);
        self
    }

    /// Set the initial window title.
    #[must_use]
    pub fn title(mut self, title: impl Into<Arc<str>>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Build the terminal with the configured options.
    #[must_use]
    pub fn build(self) -> Terminal {
        // Build the grid based on scrollback configuration.
        // Default ring buffer size matches Grid::new (10,000 lines).
        let grid = match (self.ring_buffer_size, self.scrollback) {
            (Some(ring_size), Some(scrollback)) => {
                Grid::with_tiered_scrollback(self.rows, self.cols, ring_size, scrollback)
            }
            (None, Some(scrollback)) => {
                // Caller provided tiered scrollback but no explicit ring buffer size.
                // Use the default (10,000) rather than silently dropping the scrollback.
                Grid::with_tiered_scrollback(self.rows, self.cols, 10_000, scrollback)
            }
            (Some(ring_size), None) => {
                // Caller set ring buffer size but no tiered scrollback.
                Grid::with_scrollback(self.rows, self.cols, ring_size)
            }
            (None, None) => Grid::new(self.rows, self.cols),
        };

        // Use Terminal::with_grid for consistent field initialization (#1648)
        let mut terminal = Terminal::with_grid(grid);

        // Apply builder-specific customizations
        if let Some(title) = self.title {
            // Defense-in-depth: enforce MAX_TITLE_BYTES even for programmatic API
            let boundary = title.floor_char_boundary(super::MAX_TITLE_BYTES);
            terminal.title.window = if boundary < title.len() {
                Arc::from(&title[..boundary])
            } else {
                title
            };
        }
        if let Some(fg) = self.foreground {
            terminal.color.default_foreground = fg;
        }
        if let Some(bg) = self.background {
            terminal.color.default_background = bg;
        }
        if let Some(font) = self.font {
            terminal.font = font;
        }

        terminal
    }
}
