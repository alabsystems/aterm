// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Callback registration for `Terminal` (the `set_*_callback` methods),
//! `resize()`, the resize offload and the compression-worker hooks.

use super::{ClipboardOperation, Terminal, types};
#[cfg(any(test, target_os = "linux"))]
use super::{WindowOperation, WindowResponse};
use crate::grid::ResizePolicy;

#[cfg(test)]
#[path = "resize_conpty_tests.rs"]
mod resize_conpty_tests;

impl Terminal {
    /// Resize the terminal under [`ResizePolicy::Native`] (the grid is the frame, as
    /// under a Unix PTY).
    ///
    /// The primary screen reflows (soft-wrapped lines rewrap); the alt screen does not
    /// (the app redraws after SIGWINCH). The inactive grid keeps its own mode, as in
    /// xterm, Alacritty and kitty. The grid clamps dimensions to
    /// `1..=`[`MAX_GRID_ROWS`](crate::grid::MAX_GRID_ROWS)`/`[`MAX_GRID_COLS`](crate::grid::MAX_GRID_COLS),
    /// so a hostile resize cannot request an arbitrarily large allocation.
    #[inline]
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
        // The resize journal's pre-resize facts, consumed by `finalize_resize`.
        let origin = self.resize_origin(policy);
        // Captured before the resize: afterwards both grids carry the new width.
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
        // Each saved cursor rides the resize of the grid it belongs to.
        self.lend_saved_cursors();
        // The holder fence's anchors ride a rewrap lent, as the cursors do.
        let anchors = cols_changed
            .then(|| self.lend_main_anchors(aterm_grid::LendReach::Ring))
            .flatten();
        if self.modes.alternate_screen {
            // Alt active: the app redraws its grid; the saved primary reflows.
            self.grid.resize_no_reflow_with_policy(rows, cols, policy);
            if let Some(ref mut saved_primary) = self.alt_grid {
                saved_primary.resize_with_policy(rows, cols, policy);
            }
        } else {
            // Primary active: it reflows; the alt grid does not.
            self.grid.resize_with_policy(rows, cols, policy);
            if let Some(ref mut alt) = self.alt_grid {
                alt.resize_no_reflow_with_policy(rows, cols, policy);
            }
        }
        self.take_back_saved_cursors();
        self.take_back_main_anchors(anchors);
        self.finalize_resize(cols_changed, rows_before, origin);
    }

    /// Lend the main grid's holder-fence anchors out for a width change or
    /// a re-attach ([`Grid::lend_anchors`](crate::grid::Grid::lend_anchors)),
    /// reading history no further than `reach`. `None`, free, when no drawer
    /// ever ran.
    #[inline]
    fn lend_main_anchors(
        &mut self,
        reach: aterm_grid::LendReach,
    ) -> Option<aterm_grid::AnchorLend> {
        self.main_grid_mut().lend_anchors(reach)
    }

    /// Put lent anchors back where their text landed.
    #[inline]
    fn take_back_main_anchors(&mut self, lend: Option<aterm_grid::AnchorLend>) {
        if let Some(lend) = lend {
            self.main_grid_mut().take_back_anchors(lend);
        }
    }

    /// Lend each grid its DECSC slot (xterm `sc[0]` to the primary-content
    /// grid, `sc[1]` to the alternate) for a resize or an offloaded re-attach,
    /// so the slot follows its text as the grid's own cursor does
    /// ([`Grid::lend_saved_cursor`](crate::grid::Grid::lend_saved_cursor)): the
    /// 1049 exit and DECRC then land on the cell after the saved output.
    fn lend_saved_cursors(&mut self) {
        let (active, inactive) = if self.modes.alternate_screen {
            (self.cursor_save.alt, self.cursor_save.main)
        } else {
            (self.cursor_save.main, self.cursor_save.alt)
        };
        self.grid
            .lend_saved_cursor(active.map(|saved| saved.cursor));
        if let Some(other) = self.alt_grid.as_mut() {
            other.lend_saved_cursor(inactive.map(|saved| saved.cursor));
        }
    }

    /// Take the lent slots back where the grids carried them. The rest of each
    /// slot (style, modes, charset, pending wrap) stays as saved.
    fn take_back_saved_cursors(&mut self) {
        let active = self.grid.take_lent_saved_cursor();
        let inactive = self
            .alt_grid
            .as_mut()
            .and_then(crate::grid::Grid::take_lent_saved_cursor);
        let (active_slot, inactive_slot) = if self.modes.alternate_screen {
            (&mut self.cursor_save.alt, &mut self.cursor_save.main)
        } else {
            (&mut self.cursor_save.main, &mut self.cursor_save.alt)
        };
        for (slot, carried) in [(active_slot, active), (inactive_slot, inactive)] {
            if let (Some(saved), Some(cursor)) = (slot.as_mut(), carried) {
                saved.cursor = cursor;
            }
        }
    }

    /// Resize with the width-change rewrap of off-screen scrollback moved off the
    /// caller's thread.
    ///
    /// The visible grids resize synchronously, and the tiered scrollback of the
    /// primary-content grid (the active primary, or the saved primary while alt is up)
    /// is detached in O(1). The returned `Send` job rewraps it via
    /// [`PendingScrollbackReflow::reflow`](aterm_grid::PendingScrollbackReflow::reflow);
    /// pass the result to [`Terminal::finish_resize_offload`]. `None` when there is
    /// nothing to offload (no width change or no tiered store): then this equals
    /// [`Terminal::resize`].
    #[inline]
    pub fn resize_offloading_scrollback(
        &mut self,
        rows: u16,
        cols: u16,
    ) -> Option<aterm_grid::PendingScrollbackReflow> {
        self.resize_offloading_scrollback_with_policy(rows, cols, ResizePolicy::Native)
    }

    /// [`resize_offloading_scrollback`](Self::resize_offloading_scrollback) with
    /// a seam policy. Under [`ResizePolicy::ConPty`] the re-attach does no
    /// deficit fill, so a conhost repaint that arrives before it is never
    /// shifted.
    pub fn resize_offloading_scrollback_with_policy(
        &mut self,
        rows: u16,
        cols: u16,
        policy: ResizePolicy,
    ) -> Option<aterm_grid::PendingScrollbackReflow> {
        // Captured before the resize, as in `resize_with_policy`.
        let origin = self.resize_origin(policy);
        let cols_changed = self.grid.cols() != cols;
        let rows_before = self.grid.rows();
        // The visible grid still rewraps inline, where the image placements live.
        if cols_changed {
            self.pin_image_rows_for_width_change(cols);
        }
        // As in `resize_with_policy`; the re-attach lends the saved cursors again
        // for its deficit fill.
        self.lend_saved_cursors();
        // The anchors too, but only as far up as the history this resize
        // rewraps synchronously: a Start beyond it clamps to the oldest line
        // held back, and a span wholly beyond empties, so nothing rides the
        // job (the design's clamp before detach).
        let anchors = if cols_changed {
            let main = self.main_grid_mut();
            let reach = if main.scrollback().is_some() {
                main.anchor_reach_for_offload(rows, cols)
            } else {
                aterm_grid::LendReach::Ring
            };
            self.lend_main_anchors(reach)
        } else {
            None
        };
        let pending = if self.modes.alternate_screen {
            // Alt active: the saved primary holds the scrollback that reflows.
            self.grid.resize_no_reflow_with_policy(rows, cols, policy);
            self.alt_grid.as_mut().and_then(|saved_primary| {
                saved_primary.resize_offloading_scrollback_with_policy(rows, cols, policy)
            })
        } else {
            // Primary active: it reflows (offloaded); the alt grid does not.
            let pending = self
                .grid
                .resize_offloading_scrollback_with_policy(rows, cols, policy);
            if let Some(ref mut alt) = self.alt_grid {
                alt.resize_no_reflow_with_policy(rows, cols, policy);
            }
            pending
        };
        self.take_back_saved_cursors();
        self.take_back_main_anchors(anchors);
        self.finalize_resize(cols_changed, rows_before, origin);
        pending
    }

    /// Re-attach a scrollback store rewrapped off-thread by
    /// [`resize_offloading_scrollback`](Self::resize_offloading_scrollback) onto the
    /// primary-content grid. A no-op if that grid re-acquired a tiered store meanwhile
    /// ([`Grid::reattach_reflowed_scrollback`](crate::grid::Grid::reattach_reflowed_scrollback)).
    ///
    /// If the width changed while the reflow ran, the store is stale: it is re-detached
    /// at the current width and returned. Drive that job like the original and call this
    /// again (at most one extra pass per settled drag); dropping it without
    /// [`Self::abort_resize_offload`] wedges the detach window.
    #[must_use = "drive the returned convergence job and re-attach it (or abort), or the detach window wedges"]
    pub fn finish_resize_offload(
        &mut self,
        reflowed: aterm_grid::ReflowedScrollback,
    ) -> Option<aterm_grid::PendingScrollbackReflow> {
        // Re-attachment may move the live rows and their cursor again.
        self.transient.sync_end_caret = None;
        // The re-attach's deficit fill moves the screen down by the lines it
        // pulls back; the saved cursors and the fence's anchors move with it.
        self.lend_saved_cursors();
        let anchors = self.lend_main_anchors(aterm_grid::LendReach::Ring);
        let target = if self.modes.alternate_screen {
            self.alt_grid.as_mut()
        } else {
            Some(&mut self.grid)
        };
        let next = target.and_then(|grid| grid.reattach_reflowed_scrollback_or_redetach(reflowed));
        self.take_back_saved_cursors();
        self.take_back_main_anchors(anchors);
        next
    }

    /// Abort an offloaded resize whose reflow will never re-attach (the worker
    /// panicked or died before [`finish_resize_offload`](Self::finish_resize_offload)),
    /// returning the primary-content grid to a bounded state so the detach window
    /// does not wedge it; see
    /// [`Grid::abort_reflow_offload`](crate::grid::Grid::abort_reflow_offload). Idempotent.
    #[inline]
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
    /// LZ4 promotion off the PTY-drain path. See
    /// [`Grid::set_compress_offload_active`](aterm_grid::Grid::set_compress_offload_active).
    #[inline]
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
    #[inline]
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
    #[inline]
    pub fn drain_lazy_bounded(&mut self, max_lines: usize) -> usize {
        let target = if self.modes.alternate_screen {
            self.alt_grid.as_mut()
        } else {
            Some(&mut self.grid)
        };
        target.map_or(0, |grid| grid.drain_lazy_bounded(max_lines))
    }

    /// A pressure-relief batch on the same primary-content backlog, refusing
    /// to promote an open cut's marker. The grid rechecks each batch, including
    /// one owed before the caller released its terminal lock.
    #[inline]
    pub fn drain_lazy_pressure_bounded(&mut self, max_lines: usize) -> usize {
        let target = if self.modes.alternate_screen {
            self.alt_grid.as_mut()
        } else {
            Some(&mut self.grid)
        };
        target.map_or(0, |grid| grid.drain_lazy_pressure_bounded(max_lines))
    }

    /// One bounded drain opportunity for a host polling mid-flood (the
    /// compression worker's trickle, a wasm host's per-frame drain), on the same
    /// backlog as [`drain_lazy_bounded`](Self::drain_lazy_bounded). Skipped while
    /// the flood path is cutting that backlog and a batch arrived since the last
    /// opportunity, so one flood leaves one marker and a slow tail still gets
    /// promoted. See [`Grid::trickle_lazy_bounded`](aterm_grid::Grid::trickle_lazy_bounded).
    #[inline]
    pub fn trickle_lazy_bounded(&mut self, max_lines: usize) -> usize {
        let target = if self.modes.alternate_screen {
            self.alt_grid.as_mut()
        } else {
            Some(&mut self.grid)
        };
        target.map_or(0, |grid| grid.trickle_lazy_bounded(max_lines))
    }

    /// Shared post-resize side effects: the resize journal entry, selection
    /// handling, the DEC 2048 size report and the debug invariant check. The
    /// arguments are captured before the resize.
    fn finalize_resize(
        &mut self,
        cols_changed: bool,
        rows_before: u16,
        origin: super::resize_journal::ResizeOrigin,
    ) {
        // Even a later resize back to the same dimensions cannot reuse a
        // pre-resize cell as an ordered close's caret.
        self.transient.sync_end_caret = None;
        // Read before the journal drains the shape: a grow that handed a whole
        // resize undo back puts the pre-flap selection back below.
        let undone = self.grid.last_resize_shape().undone;
        // Journal first, from the active grid's row accounting (a separate record
        // from `take_last_resize_row_shift`, which selection custody drains).
        self.journal_resize(origin);
        // Only a width change invalidates coordinates (rewrap renumbers rows): the
        // selections clear. A rows-only resize keeps row identity: the selection is
        // relabelled by the row shift (resize undo included) and clamped.
        if cols_changed {
            // A width change never keeps a grid resize undo, so none is open.
            self.resize_undo_selection = None;
            self.text_selection.clear();
            // The parked selection's grid was rewrapped too. Cleared outright:
            // `finish_resize_offload`/`abort_resize_offload` later change that
            // grid's scrollback with no selection call (sound only while the
            // offload path is width-only).
            self.parked_text_selection.clear();
        } else {
            // A rows grow that revealed retained history moves every pre-resize row
            // `revealed` rows down, as the grid does for its cursors;
            // `adjust_for_scroll` subtracts its delta, hence `-revealed`. Below the
            // floor anchors clamp, which is only correct with this delta. A rows-only
            // shrink is not uniform: the bottom `demoted` viewport rows go to history
            // while top rows stay, the piecewise map `adjust_for_rows_shrink` applies.
            let selection_before = self.text_selection.clone();
            let revealed = i32::from(self.grid.take_last_resize_row_shift());
            let max_rows = i32::from(self.grid.rows());
            let demoted = i32::from(rows_before.saturating_sub(self.grid.rows()));
            let floor = i32::try_from(self.grid.scrollback_lines()).unwrap_or(i32::MAX);
            if demoted > 0 {
                // Here `revealed` is the demote count, not the height delta:
                // trimming trailing blank rows moves nothing.
                self.text_selection
                    .adjust_for_rows_shrink(max_rows, revealed, floor);
            } else {
                self.text_selection
                    .adjust_for_scroll(-revealed, max_rows, floor);
            }
            // Resize undo: the transform above clamps an anchor on a row the alt grid
            // demoted, so the grow would land it a row low though every cell came
            // back. Keep the selection the first shrink found and restore it when the
            // grid hands its undo back whole. Active screen only: the parked main
            // grid keeps history and opens no undo.
            self.follow_resize_undo_selection(selection_before, undone);
            // The parked selection follows the other grid, with that grid's own shift
            // and floor (the active alt grid's `scrollback_lines()` is 0 and would
            // evict every scrollback anchor); both grids ran the same height change.
            if let Some(parked_grid) = self.alt_grid.as_mut() {
                let parked_revealed = i32::from(parked_grid.take_last_resize_row_shift());
                let parked_floor =
                    i32::try_from(parked_grid.scrollback_lines()).unwrap_or(i32::MAX);
                if demoted > 0 {
                    // The parked grid's own demote count, never the active grid's.
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
        // DEC mode 2048: send an in-band size report on every resize, with the
        // reports sent unasked (`take_reports`; the emitter holds the cap of what
        // is owed to the peer against a resize storm).
        if self.modes.in_band_size_reports {
            super::state_accessors::push_in_band_size_report(
                &mut self.transient,
                self.grid.rows(),
                self.grid.cols(),
                self.iterm2.host_cell_px(),
            );
        }
        // Resize is a major grid mutation boundary: check structure (debug builds).
        #[cfg(debug_assertions)]
        self.grid.assert_structural_invariants();
    }

    /// Install the callback told about the FIRST palette set (OSC 4, numeric
    /// OSC 21) the `allow_palette_reconfigure` gate refuses in this session.
    /// It runs inside `process()` under the Terminal lock, at most once per
    /// session (RIS does not re-arm it), so a host only records the fact.
    #[inline]
    pub fn set_palette_refused_callback<F: FnMut() + Send + 'static>(&mut self, callback: F) {
        self.color.palette_refused_callback = Some(Box::new(callback));
    }

    /// Whether this session has refused a palette set (see
    /// [`Self::set_palette_refused_callback`]).
    #[inline]
    #[must_use]
    pub const fn palette_set_refused(&self) -> bool {
        self.color.palette_set_refused
    }

    /// Set bell callback.
    #[inline]
    pub fn set_bell_callback<F: FnMut() + Send + 'static>(&mut self, callback: F) {
        self.bell_callback = Some(Box::new(callback));
    }

    /// Install the host resolver for Kitty non-direct transmission mediums
    /// (`t=f`/`t=t`/`t=s`): it gets `(medium, path-or-name)` and returns the image
    /// bytes or `None`. The host owns the I/O and its security policy (whether to
    /// read at all, which paths, size caps), so the engine stays pure and wasm-safe;
    /// with no resolver installed, non-direct mediums are skipped.
    #[inline]
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
    #[inline]
    pub fn set_notification_callback<F: FnMut(&str) + Send + 'static>(&mut self, callback: F) {
        self.notifications.callback = Some(Box::new(callback));
    }

    /// Set the callback for complete OSC 99 (kitty: title, body, urgency, id)
    /// and OSC 777 (`ESC ] 777 ; notify ; title ; body ST`) notifications.
    /// See <https://sw.kovidgoyal.net/kitty/desktop-notifications/>.
    #[inline]
    pub fn set_advanced_notification_callback<F: FnMut(types::Notification) + Send + 'static>(
        &mut self,
        callback: F,
    ) {
        self.notifications.advanced_callback = Some(Box::new(callback));
    }

    /// Set the OSC 52 clipboard callback. `Set` and `Clear` return `None`;
    /// `Query` returns the contents, or `None` to deny. Queries (`Pd = ?`) are
    /// ignored unless `TerminalConfig::allow_osc52_query` is set.
    #[inline]
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
    #[inline]
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
