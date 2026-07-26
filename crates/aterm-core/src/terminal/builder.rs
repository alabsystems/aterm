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

/// Engine-default hot-ring cap for a TIERED terminal (audit E1). The ring is
/// the fixed fast tier in front of the compressed store — deep enough that
/// interactive scroll-back over recent output never touches a decode path,
/// small enough that the compressed tiers (not raw cells) hold deep history.
pub const TIERED_RING_CAP_DEFAULT: usize = 1_000;

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

    /// Attach the ENGINE-DEFAULT tiered scrollback (audit E1): hot+warm(+cold)
    /// store with the crate-default tier sizes and memory budget, behind a
    /// [`TIERED_RING_CAP_DEFAULT`]-line hot ring, capped so the ONE total
    /// retention limit (ring + staged + store — see
    /// [`Terminal::set_scrollback_line_limit`](super::Terminal::set_scrollback_line_limit))
    /// starts at `DEFAULT_LINE_LIMIT` exactly.
    ///
    /// This is the constructor embedding daemons should use instead of a bare
    /// `ring_buffer_size(N)` (which retains raw uncompressed cells, ~640 B/line
    /// at 80 cols, content-independent): the tiered store retains attributed
    /// history at ~1/3 to 1/10 that, and unlocks the off-thread history-reflow
    /// path. The cold-tier codec follows the BUILD, not the call site — LZ4 in
    /// the default no-platform build, zstd (+ optional disk spill) under the
    /// `disk-tier`/`zstd` features; introspect via
    /// [`TierCapabilities::current`](crate::scrollback::TierCapabilities::current).
    ///
    /// NOTE for hosts without a compression-drain thread: scrolled-off lines are
    /// promoted (LZ4) inline in ~1000-line batches on the feeding thread. A
    /// throughput-critical host should mirror the GUI session:
    /// `set_compress_offload_active(true)` + a worker draining
    /// `drain_lazy_bounded(...)` off the PTY-read path.
    #[must_use]
    pub fn tiered_scrollback_defaults(mut self) -> Self {
        let mut scrollback = Scrollback::with_defaults();
        // Store share = total − ring share, so the unified getter round-trips
        // DEFAULT_LINE_LIMIT as the out-of-the-box total.
        scrollback.set_line_limit(Some(
            aterm_scrollback::DEFAULT_LINE_LIMIT.saturating_sub(TIERED_RING_CAP_DEFAULT),
        ));
        self.scrollback = Some(scrollback);
        self.ring_buffer_size = Some(TIERED_RING_CAP_DEFAULT);
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
