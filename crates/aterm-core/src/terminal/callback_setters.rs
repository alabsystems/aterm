// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Callback registration for `Terminal`: the `set_*_callback` methods,
//! `resize()` and the resize offload.

use super::{ClipboardOperation, Terminal, types};
#[cfg(any(test, target_os = "linux"))]
use super::{WindowOperation, WindowResponse};
use crate::grid::ResizePolicy;

#[cfg(test)]
#[path = "resize_conpty_tests.rs"]
mod resize_conpty_tests;

impl Terminal {
    /// Resize the terminal.
    ///
    /// The primary screen reflows (soft-wrapped lines rewrap); the alt screen
    /// does not (the app redraws after SIGWINCH). The inactive grid in
    /// `alt_grid` uses its own mode, as in xterm, Alacritty and kitty.
    ///
    /// Dimensions are clamped by the grid to
    /// `1..=`[`MAX_GRID_ROWS`](crate::grid::MAX_GRID_ROWS)`/`[`MAX_GRID_COLS`](crate::grid::MAX_GRID_COLS),
    /// so a hostile resize cannot request an arbitrarily large allocation.
    ///
    /// This is [`resize_with_policy`](Self::resize_with_policy) under
    /// [`ResizePolicy::Native`]: the grid is the frame, as under a Unix PTY.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.resize_with_policy(rows, cols, ResizePolicy::Native);
    }

    /// [`resize`](Self::resize) with the seam policy the PTY backend requires.
    ///
    /// The host picks the policy from what sits behind the PTY: under Windows
    /// ConPTY conhost repaints the whole viewport after every resize,
    /// row-0-anchored, so the history/viewport seam accounting must match that
    /// repaint or lines are lost or duplicated (see [`ResizePolicy`]). Both grids
    /// get the policy, since conhost repaints the alt screen the same way.
    pub fn resize_with_policy(&mut self, rows: u16, cols: u16, policy: ResizePolicy) {
        // The resize journal's pre-resize facts (geometry, screen, open 2026
        // frame), consumed by `finalize_resize` — see `resize_journal`.
        let origin = self.resize_origin(policy);
        // Captured BEFORE the resize: afterwards both grids carry the new width.
        let cols_changed = self.grid.cols() != cols;
        // `finalize_resize` needs the shrink amount; the grid publishes only the
        // grow shift. Both grids are resized to the same `rows`.
        let rows_before = self.grid.rows();
        // Inline images live as cell extras on rows with no glyph, and reflow
        // copies extras only within `Row::len()`: pin image rows' lengths first
        // (see `image_reflow`).
        if cols_changed {
            self.pin_image_rows_for_width_change(cols);
        }
        if self.modes.alternate_screen {
            // Alt screen active: don't reflow current grid (app-managed content).
            // Saved primary grid should reflow normally.
            self.grid.resize_no_reflow_with_policy(rows, cols, policy);
            if let Some(ref mut saved_primary) = self.alt_grid {
                saved_primary.resize_with_policy(rows, cols, policy);
            }
        } else {
            // Primary screen active: reflow current grid.
            // Alt grid (if present) should not be reflowed.
            self.grid.resize_with_policy(rows, cols, policy);
            if let Some(ref mut alt) = self.alt_grid {
                alt.resize_no_reflow_with_policy(rows, cols, policy);
            }
        }
        self.finalize_resize(cols_changed, rows_before, origin);
    }

    /// Resize with the width-change rewrap of off-screen scrollback moved off
    /// the caller's thread.
    ///
    /// The visible grids resize synchronously (bounded by the viewport), and the
    /// tiered scrollback of the primary-content grid (the active primary, or the
    /// saved primary while alt is up) is detached in O(1). The returned `Send`
    /// job rewraps it via [`PendingScrollbackReflow::reflow`]; pass the result to
    /// [`Terminal::finish_resize_offload`]. `None` when there is nothing to
    /// offload (no width change or no tiered store): then this equals
    /// [`Terminal::resize`].
    pub fn resize_offloading_scrollback(
        &mut self,
        rows: u16,
        cols: u16,
    ) -> Option<aterm_grid::PendingScrollbackReflow> {
        self.resize_offloading_scrollback_with_policy(rows, cols, ResizePolicy::Native)
    }

    /// [`resize_offloading_scrollback`](Self::resize_offloading_scrollback) with
    /// a seam policy, the offloaded twin of
    /// [`resize_with_policy`](Self::resize_with_policy). Under
    /// [`ResizePolicy::ConPty`] the re-attach does no deficit fill, so a conhost
    /// repaint that arrives before the re-attach is never shifted.
    pub fn resize_offloading_scrollback_with_policy(
        &mut self,
        rows: u16,
        cols: u16,
        policy: ResizePolicy,
    ) -> Option<aterm_grid::PendingScrollbackReflow> {
        // Captured BEFORE the resize, as in `resize`.
        let origin = self.resize_origin(policy);
        let cols_changed = self.grid.cols() != cols;
        let rows_before = self.grid.rows();
        // Same image pin as the synchronous path: the offloaded resize still
        // rewraps the visible grid inline, which is where the placements live.
        if cols_changed {
            self.pin_image_rows_for_width_change(cols);
        }
        let pending = if self.modes.alternate_screen {
            // Alt active: the alt grid is app-managed; the saved primary holds
            // the scrollback that reflows.
            self.grid.resize_no_reflow_with_policy(rows, cols, policy);
            self.alt_grid.as_mut().and_then(|saved_primary| {
                saved_primary.resize_offloading_scrollback_with_policy(rows, cols, policy)
            })
        } else {
            // Primary active: current grid reflows (offloaded); alt is unreflowed.
            let pending = self
                .grid
                .resize_offloading_scrollback_with_policy(rows, cols, policy);
            if let Some(ref mut alt) = self.alt_grid {
                alt.resize_no_reflow_with_policy(rows, cols, policy);
            }
            pending
        };
        self.finalize_resize(cols_changed, rows_before, origin);
        pending
    }

    /// Re-attach a scrollback store rewrapped off-thread by
    /// [`resize_offloading_scrollback`](Self::resize_offloading_scrollback) onto
    /// the primary-content grid. A no-op if that grid re-acquired a tiered store
    /// meanwhile (see [`Grid::reattach_reflowed_scrollback`]).
    ///
    /// If the width changed while the reflow ran, the store is stale: it is
    /// re-detached at the current width and returned. Drive that job like the
    /// original and call this again (at most one extra pass per settled drag);
    /// dropping it without [`Self::abort_resize_offload`] would wedge the detach
    /// window.
    #[must_use = "drive the returned convergence job and re-attach it (or abort), or the detach window wedges"]
    pub fn finish_resize_offload(
        &mut self,
        reflowed: aterm_grid::ReflowedScrollback,
    ) -> Option<aterm_grid::PendingScrollbackReflow> {
        let target = if self.modes.alternate_screen {
            self.alt_grid.as_mut()
        } else {
            Some(&mut self.grid)
        };
        target.and_then(|grid| grid.reattach_reflowed_scrollback_or_redetach(reflowed))
    }

    /// Abort an offloaded resize whose reflow will never re-attach (the worker
    /// panicked or died before [`finish_resize_offload`](Self::finish_resize_offload)),
    /// returning the primary-content grid to a bounded state so the detach
    /// window does not wedge it; see [`Grid::abort_reflow_offload`]. Idempotent.
    pub fn abort_resize_offload(&mut self) {
        let target = if self.modes.alternate_screen {
            self.alt_grid.as_mut()
        } else {
            Some(&mut self.grid)
        };
        if let Some(grid) = target {
            grid.abort_reflow_offload();
        }
    }

    /// Attach or detach the off-thread compression worker on the
    /// primary-content grid (the one owning the tiered scrollback). While
    /// attached, ingest defers lazy-buffer draining to the worker, keeping
    /// LZ4/zstd promotion off the PTY-drain path. See
    /// [`Grid::set_compress_offload_active`](aterm_grid::Grid::set_compress_offload_active).
    pub fn set_compress_offload_active(&mut self, active: bool) {
        let target = if self.modes.alternate_screen {
            self.alt_grid.as_mut()
        } else {
            Some(&mut self.grid)
        };
        if let Some(grid) = target {
            grid.set_compress_offload_active(active);
        }
    }

    /// Lines staged in the primary-content grid's lazy buffer awaiting
    /// off-thread promotion (the compression worker's backlog; 0 if none).
    #[must_use]
    pub fn lazy_backlog_len(&self) -> usize {
        if self.modes.alternate_screen {
            self.alt_grid
                .as_ref()
                .map_or(0, aterm_grid::Grid::lazy_backlog_len)
        } else {
            self.grid.lazy_backlog_len()
        }
    }

    /// Drain up to `max_lines` of the primary-content grid's staged backlog into
    /// its tiered store, returning the lines still staged. The caller holds the
    /// terminal lock; the worker drives this per batch to keep each hold short.
    /// See [`Grid::drain_lazy_bounded`](aterm_grid::Grid::drain_lazy_bounded).
    pub fn drain_lazy_bounded(&mut self, max_lines: usize) -> usize {
        let target = if self.modes.alternate_screen {
            self.alt_grid.as_mut()
        } else {
            Some(&mut self.grid)
        };
        target.map_or(0, |grid| grid.drain_lazy_bounded(max_lines))
    }

    /// One bounded drain opportunity for a host polling mid-flood (the
    /// compression worker's trickle, a wasm host's per-frame drain), on the same
    /// backlog as [`drain_lazy_bounded`](Self::drain_lazy_bounded). Skipped while
    /// the flood path is cutting that backlog and a batch arrived since the last
    /// opportunity, so one flood leaves one marker and a slow tail still gets
    /// promoted. See [`Grid::trickle_lazy_bounded`](aterm_grid::Grid::trickle_lazy_bounded).
    pub fn trickle_lazy_bounded(&mut self, max_lines: usize) -> usize {
        let target = if self.modes.alternate_screen {
            self.alt_grid.as_mut()
        } else {
            Some(&mut self.grid)
        };
        target.map_or(0, |grid| grid.trickle_lazy_bounded(max_lines))
    }

    /// Shared post-resize side effects: the resize journal entry, selection
    /// handling, the DEC 2048 in-band size report and the debug invariant check.
    /// `cols_changed`, `rows_before` and `origin` are captured by the caller
    /// before the resize.
    fn finalize_resize(
        &mut self,
        cols_changed: bool,
        rows_before: u16,
        origin: super::resize_journal::ResizeOrigin,
    ) {
        // Read before the journal drains the shape: a grow that handed a whole
        // resize undo back puts the pre-flap selection back below.
        let undone = self.grid.last_resize_shape().undone;
        // Journal first, from the active grid's row accounting (a separate record
        // from `take_last_resize_row_shift`, which selection custody drains).
        self.journal_resize(origin);
        // Only a width change invalidates coordinates (rewrap renumbers rows). A
        // rows-only resize keeps row identity, so it re-runs the existing range
        // check with delta 0, the new live-bottom row count and the scrollback
        // floor.
        if cols_changed {
            // A width change never keeps a grid resize undo, so none is open.
            self.resize_undo_selection = None;
            self.text_selection.clear();
            // The parked selection's grid was rewrapped too. Cleared
            // unconditionally because `finish_resize_offload` /
            // `abort_resize_offload` later mutate that grid's scrollback with no
            // selection call; sound only while the offload path is width-only.
            self.parked_text_selection.clear();
        } else {
            // A rows grow that revealed retained history moves every pre-resize
            // row `revealed` rows down, as `Grid::resize_with_reflow_mode` does for
            // the cursors; the selection lives on the Terminal, so it follows here.
            // `adjust_for_scroll` subtracts its delta, so the move is `-revealed`.
            // Read from the active grid; each grid records its own shift. Below the
            // floor anchors clamp, which is only correct with this delta.
            //
            // A rows-only shrink is not a uniform delta: `adjust_row_count_rows_only`
            // pushes the bottom `demoted` viewport rows into history while top rows
            // stay, so scrollback anchors gain `demoted` lines above them.
            // `adjust_for_rows_shrink` is that piecewise map (see its doc).
            let selection_before = self.text_selection.clone();
            let revealed = i32::from(self.grid.take_last_resize_row_shift());
            let max_rows = i32::from(self.grid.rows());
            let demoted = i32::from(rows_before.saturating_sub(self.grid.rows()));
            let floor = i32::try_from(self.grid.scrollback_lines()).unwrap_or(i32::MAX);
            if demoted > 0 {
                // `revealed` is the shrink's actual relabel distance (the demote
                // count), not the height delta: trimming trailing blank rows moves
                // nothing.
                self.text_selection
                    .adjust_for_rows_shrink(max_rows, revealed, floor);
            } else {
                self.text_selection
                    .adjust_for_scroll(-revealed, max_rows, floor);
            }
            // Resize undo: the transform above clamps an anchor on a row the alt
            // grid demoted, so the grow would land it a row low even though every
            // cell came back. Keep the selection the first shrink found and put it
            // back when the grid hands its undo back whole. Active screen only; the
            // parked (main) selection's grid keeps history and opens no undo.
            self.follow_resize_undo_selection(selection_before, undone);
            // The parked selection gets the same treatment against the other grid,
            // with that grid's floor (the active alt grid's `scrollback_lines()` is 0
            // and would evict every scrollback anchor). `max_rows` and `demoted` are
            // shared: both grids ran the same height change.
            if let Some(parked_grid) = self.alt_grid.as_mut() {
                let parked_revealed = i32::from(parked_grid.take_last_resize_row_shift());
                let parked_floor =
                    i32::try_from(parked_grid.scrollback_lines()).unwrap_or(i32::MAX);
                if demoted > 0 {
                    // The parked grid ran the same shapes and recorded its own
                    // demote count — read it, never the active grid's.
                    self.parked_text_selection.adjust_for_rows_shrink(
                        max_rows,
                        parked_revealed,
                        parked_floor,
                    );
                } else {
                    self.parked_text_selection.adjust_for_scroll(
                        -parked_revealed,
                        max_rows,
                        parked_floor,
                    );
                }
            }
        }
        // DEC mode 2048: send an in-band size report on every resize. This writes
        // the response buffer directly, so honor its cap: a resize storm against
        // a host not draining responses must not grow it past
        // MAX_RESPONSE_BUFFER_SIZE. The report is <= 48 bytes.
        if self.modes.in_band_size_reports
            && self.transient.response_buffer.len().saturating_add(48)
                <= super::MAX_RESPONSE_BUFFER_SIZE
        {
            let (rows, cols) = (self.grid.rows(), self.grid.cols());
            let (cw, ch) = self.iterm2.cell_px;
            super::state_accessors::push_in_band_size_report(
                &mut self.transient.response_buffer,
                rows,
                cols,
                cw,
                ch,
            );
        }
        // INTEGRITY-SELFCHECK: resize/reflow is the other major grid mutation
        // boundary. Debug builds only.
        #[cfg(debug_assertions)]
        self.grid.assert_structural_invariants();
    }

    /// Set bell callback.
    pub fn set_bell_callback<F: FnMut() + Send + 'static>(&mut self, callback: F) {
        self.bell_callback = Some(Box::new(callback));
    }

    /// Install the host resolver for Kitty non-direct transmission mediums
    /// (`t=f`/`t=t`/`t=s`). The engine hands the host `(medium, path-or-name)`
    /// and gets the image bytes or `None`. The host owns the I/O and the security
    /// policy (whether to read files or shared memory at all, which paths, size
    /// caps); with no resolver installed, non-direct mediums are skipped. The
    /// engine stays pure and wasm-safe.
    pub fn set_kitty_file_resolver<F>(&mut self, resolver: F)
    where
        F: Fn(crate::terminal::kitty_graphics::KittyMedium, &str) -> Option<Vec<u8>>
            + Send
            + 'static,
    {
        self.kitty_file_resolver = Some(Box::new(resolver));
    }

    /// Set the desktop notification callback for OSC 9 without a subcommand
    /// (`ESC ] 9 ; message BEL|ST`); it receives the message.
    pub fn set_notification_callback<F: FnMut(&str) + Send + 'static>(&mut self, callback: F) {
        self.notifications.callback = Some(Box::new(callback));
    }

    /// Set the callback for dynamic color changes (OSC 10/11/12, 110/111/112):
    /// it receives which color changed, its new value and whether it was set,
    /// reset or made dynamic.
    #[cfg(test)]
    pub fn set_color_change_callback<F>(&mut self, callback: F)
    where
        F: FnMut(super::ColorTarget, aterm_types::Rgb, super::ColorChangeOp) + Send + 'static,
    {
        self.color.change_callback = Some(Box::new(callback));
    }

    /// Set the callback consulted first for dynamic color queries (OSC 10/11/12
    /// `?`), for a host that renders colors other than the palette's. `Some`
    /// answers the query; `None` falls back to the palette.
    #[cfg(test)]
    pub fn set_color_query_callback<F>(&mut self, callback: F)
    where
        F: FnMut(super::ColorTarget) -> Option<aterm_types::Rgb> + Send + 'static,
    {
        self.color.query_callback = Some(Box::new(callback));
    }

    /// Set the callback for complete OSC 99 (kitty: title, body, urgency, id)
    /// and OSC 777 (`ESC ] 777 ; notify ; title ; body ST`) notifications.
    /// See <https://sw.kovidgoyal.net/kitty/desktop-notifications/>.
    pub fn set_advanced_notification_callback<F: FnMut(types::Notification) + Send + 'static>(
        &mut self,
        callback: F,
    ) {
        self.notifications.advanced_callback = Some(Box::new(callback));
    }

    /// Set the OSC 52 clipboard callback. `Set` and `Clear` return `None`;
    /// `Query` returns the contents, or `None` to deny. Queries (`Pd = ?`) are
    /// ignored unless `TerminalConfig::allow_osc52_query` is set.
    pub fn set_clipboard_callback<F>(&mut self, callback: F)
    where
        F: FnMut(ClipboardOperation) -> Option<String> + Send + 'static,
    {
        self.clipboard.callback = Some(Box::new(callback));
    }

    /// Set the XTWINOPS (`CSI t`) callback: perform a manipulation and return
    /// `None`, or answer a query with its `WindowResponse`.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use aterm_core::terminal::{Terminal, WindowOperation, WindowResponse};
    ///
    /// let mut terminal = Terminal::new(24, 80);
    /// terminal.set_window_callback(|op| {
    ///     match op {
    ///         WindowOperation::ReportTextAreaSizeCells => {
    ///             Some(WindowResponse::SizeCells { rows: 24, cols: 80 })
    ///         }
    ///         WindowOperation::Iconify => {
    ///             // Minimize window (platform-specific)
    ///             None
    ///         }
    ///         _ => None,
    ///     }
    /// });
    /// ```
    #[cfg(any(test, target_os = "linux"))]
    pub fn set_window_callback<F>(&mut self, callback: F)
    where
        F: FnMut(WindowOperation) -> Option<WindowResponse> + Send + 'static,
    {
        self.window_callback = Some(Box::new(callback));
    }
}

#[cfg(test)]
mod offload_tests {
    use super::Terminal;
    use aterm_scrollback::Scrollback;

    /// A terminal whose bulk history lives in the tiered store (small ring → most
    /// scroll-off spills to tiered, like a real session).
    fn term_with_history(rows: u16, cols: u16, lines: usize) -> Terminal {
        let sb = Scrollback::new(64, 512, 8_000_000);
        let mut t = Terminal::with_scrollback(rows, cols, 8, sb);
        let fill = "x".repeat((cols as usize).saturating_sub(8));
        let mut buf = Vec::new();
        for i in 0..lines {
            buf.extend_from_slice(format!("L{i}-{fill}\r\n").as_bytes());
        }
        t.process(&buf);
        t
    }

    /// A width-change offload on the primary screen rewraps history off-thread and
    /// loses none of it across the detach → reflow → re-attach round trip.
    #[test]
    fn offloaded_resize_primary_preserves_scrollback() {
        let mut t = term_with_history(24, 80, 500);
        let before = t.grid.scrollback_lines();
        assert!(before > 100, "precondition: deep history ({before} lines)");

        let pending = t
            .resize_offloading_scrollback(24, 40)
            .expect("a width change with tiered history yields an offload job");
        assert!(
            pending.line_count() > 100,
            "the detached job carries the deep history (lazy + tiered)"
        );
        assert_eq!(t.grid.cols(), 40, "visible grid resized synchronously");

        // The expensive rewrap — off-thread in production, inline here.
        let reflowed = pending.reflow();
        assert!(
            t.finish_resize_offload(reflowed).is_none(),
            "widths agree — no convergence pass expected"
        );

        let after = t.grid.scrollback_lines();
        assert!(
            after > 100,
            "history preserved across the offload (before={before}, after={after})"
        );
    }

    /// On the alt screen a resize still offloads and preserves the saved
    /// primary's scrollback.
    #[test]
    fn offloaded_resize_alt_screen_preserves_saved_primary() {
        let mut t = term_with_history(24, 80, 500);
        let before = t.grid.scrollback_lines();
        assert!(before > 100);
        t.process(b"\x1b[?1049h"); // enter alt screen (primary saved off)

        let pending = t
            .resize_offloading_scrollback(24, 40)
            .expect("alt-screen width change offloads the saved primary's history");
        let reflowed = pending.reflow();
        assert!(
            t.finish_resize_offload(reflowed).is_none(),
            "widths agree — no convergence pass expected"
        );

        t.process(b"\x1b[?1049l"); // exit alt → saved primary is active again
        let after = t.grid.scrollback_lines();
        assert!(
            after > 100,
            "saved-primary history preserved across an alt-screen offload \
             (before={before}, after={after})"
        );
    }

    /// The compression worker's trickle works the same backlog its drain does
    /// (the saved primary's under an alt screen): an opportunity right after a
    /// flood that outran it skips, and the next, with nothing new, promotes a
    /// batch (the cut's marker first).
    #[test]
    fn trickle_skips_a_flood_that_outruns_it_on_the_backlog_the_worker_drains() {
        // A 2 MB budget holds fewer staged rows than the 20k floor, so the floor
        // is the cap; the store's 100k limit is far away, so drops are a real cut.
        let sb = Scrollback::new(64, 512, 2_000_000);
        let mut t = Terminal::with_scrollback(24, 80, 8, sb);
        t.set_compress_offload_active(true);
        let mut buf = Vec::new();
        for i in 0..22_000 {
            buf.extend_from_slice(format!("L{i}\r\n").as_bytes());
        }
        t.process(&buf);
        let backlog = t.lazy_backlog_len();
        assert!(
            t.scrollback_truncated_lines() > 0,
            "precondition: a flood past the cap is cutting"
        );

        t.process(b"\x1b[?1049h");
        assert_eq!(
            t.trickle_lazy_bounded(256),
            backlog,
            "22k lines arrived since the last opportunity: skipped, on the saved \
             primary's backlog"
        );
        assert_eq!(
            t.trickle_lazy_bounded(256),
            backlog - 256,
            "nothing arrived since: a batch is promoted"
        );
    }
}
