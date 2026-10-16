// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal builder.

use crate::grid::Grid;
use crate::scrollback::Scrollback;

use super::Terminal;

/// Engine-default hot-ring cap for a tiered terminal: the fast tier in front
/// of the compressed store, deep enough that scrolling back over recent output
/// never decodes, small enough that the compressed tiers hold deep history.
pub const TIERED_RING_CAP_DEFAULT: usize = 1_000;

/// Builder for a [`Terminal`] with custom size and scrollback.
///
/// # Example
///
/// ```
/// use aterm_core::terminal::TerminalBuilder;
///
/// let terminal = TerminalBuilder::new()
///     .size(24, 80)
///     .ring_buffer_size(10_000)
///     .build();
/// ```
#[derive(Debug)]
pub struct TerminalBuilder {
    rows: u16,
    cols: u16,
    ring_buffer_size: Option<usize>,
    scrollback: Option<Scrollback>,
}

impl Default for TerminalBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalBuilder {
    /// A builder for a 24x80 terminal with a 10,000-line ring and no tiered store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rows: 24,
            cols: 80,
            ring_buffer_size: None,
            scrollback: None,
        }
    }

    /// Set the terminal size (rows and cols).
    #[must_use]
    pub fn size(mut self, rows: u16, cols: u16) -> Self {
        self.rows = rows;
        self.cols = cols;
        self
    }

    /// Set the in-memory ring's line cap (default 10,000).
    #[must_use]
    pub fn ring_buffer_size(mut self, size: usize) -> Self {
        self.ring_buffer_size = Some(size);
        self
    }

    /// Attach the engine-default tiered scrollback (crate-default tier sizes and memory
    /// budget) behind a [`TIERED_RING_CAP_DEFAULT`]-line hot ring, capped so the total
    /// retention limit (ring + staged + store; see
    /// [`Terminal::set_scrollback_line_limit`](super::Terminal::set_scrollback_line_limit))
    /// starts at `DEFAULT_LINE_LIMIT`.
    ///
    /// Prefer this to a bare `ring_buffer_size(N)`, which keeps raw cells (~640 B/line at
    /// 80 cols): the tiered store holds attributed history at roughly 1/3 to 1/10 of that
    /// and enables off-thread history reflow. Cold pages are LZ4; the disk spill follows
    /// the `disk-tier` feature
    /// ([`TierCapabilities::current`](crate::scrollback::TierCapabilities::current)).
    ///
    /// Without a compression-drain thread, scrolled-off lines compress inline in ~1000-line
    /// batches on the feeding thread; a throughput-critical host calls
    /// `set_compress_offload_active(true)` and runs `drain_lazy_bounded` on a worker, as the
    /// GUI session does.
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

    /// Build the terminal with the configured options.
    #[must_use]
    pub fn build(self) -> Terminal {
        let grid = match (self.ring_buffer_size, self.scrollback) {
            (Some(ring_size), Some(scrollback)) => {
                Grid::with_tiered_scrollback(self.rows, self.cols, ring_size, scrollback)
            }
            (None, Some(scrollback)) => {
                // A tiered store with no ring size gets Grid::new's 10,000-line ring.
                Grid::with_tiered_scrollback(self.rows, self.cols, 10_000, scrollback)
            }
            (Some(ring_size), None) => Grid::with_scrollback(self.rows, self.cols, ring_size),
            (None, None) => Grid::new(self.rows, self.cols),
        };

        Terminal::with_grid(grid)
    }
}
