// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Callback registration, resize handling, and scrollback offload hooks for `Terminal`.

use super::{ClipboardOperation, Terminal, types};
#[cfg(any(test, target_os = "linux"))]
use super::{WindowOperation, WindowResponse};
use crate::grid::ResizePolicy;

#[cfg(test)]
#[path = "resize_conpty_tests.rs"]
mod resize_conpty_tests;

impl Terminal {
    /// Resize the terminal under [`ResizePolicy::Native`], reflowing the primary screen and clamping dimensions.
    #[inline]
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.resize_with_policy(rows, cols, ResizePolicy::Native);
    }

    /// Resize the terminal with the seam [`ResizePolicy`] required by the PTY backend.
    pub fn resize_with_policy(&mut self, rows: u16, cols: u16, policy: ResizePolicy) {
        let origin = self.resize_origin(policy);
        let cols_changed = self.grid.cols() != cols;
        let rows_before = self.grid.rows();
        if cols_changed {
            self.pin_image_rows_for_width_change(cols);
        }
        self.lend_saved_cursors();
        let anchors = cols_changed
            .then(|| self.lend_main_anchors(aterm_grid::LendReach::Ring))
            .flatten();
        if self.modes.alternate_screen {
            self.grid.resize_no_reflow_with_policy(rows, cols, policy);
            if let Some(ref mut saved_primary) = self.alt_grid {
                saved_primary.resize_with_policy(rows, cols, policy);
            }
        } else {
            self.grid.resize_with_policy(rows, cols, policy);
            if let Some(ref mut alt) = self.alt_grid {
                alt.resize_no_reflow_with_policy(rows, cols, policy);
            }
        }
        self.take_back_saved_cursors(cols_changed || self.grid.rows() != rows_before);
        self.take_back_main_anchors(anchors);
        self.finalize_resize(cols_changed, rows_before, origin);
    }

    /// Lend the main grid's holder-fence anchors out up to `reach`.
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

    /// Lend each grid its DECSC saved cursor so it follows reflowed text during a resize.
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

    /// Take the lent saved cursor positions back from the grids, marking each slot `resized` when
    /// the resize changed the screen's size (DECRC reads it in `place_restored_cursor`).
    fn take_back_saved_cursors(&mut self, resized: bool) {
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
            let Some(saved) = slot.as_mut() else {
                continue;
            };
            if let Some(cursor) = carried {
                saved.cursor = cursor;
            }
            saved.resized |= resized;
        }
    }

    /// Resize the visible grids synchronously and detach off-screen tiered scrollback for off-thread reflow.
    #[inline]
    pub fn resize_offloading_scrollback(
        &mut self,
        rows: u16,
        cols: u16,
    ) -> Option<aterm_grid::PendingScrollbackReflow> {
        self.resize_offloading_scrollback_with_policy(rows, cols, ResizePolicy::Native)
    }

    /// [`resize_offloading_scrollback`](Self::resize_offloading_scrollback) with a seam [`ResizePolicy`].
    pub fn resize_offloading_scrollback_with_policy(
        &mut self,
        rows: u16,
        cols: u16,
        policy: ResizePolicy,
    ) -> Option<aterm_grid::PendingScrollbackReflow> {
        let origin = self.resize_origin(policy);
        let cols_changed = self.grid.cols() != cols;
        let rows_before = self.grid.rows();
        if cols_changed {
            self.pin_image_rows_for_width_change(cols);
        }
        self.lend_saved_cursors();
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
            self.grid.resize_no_reflow_with_policy(rows, cols, policy);
            self.alt_grid.as_mut().and_then(|saved_primary| {
                saved_primary.resize_offloading_scrollback_with_policy(rows, cols, policy)
            })
        } else {
            let pending = self
                .grid
                .resize_offloading_scrollback_with_policy(rows, cols, policy);
            if let Some(ref mut alt) = self.alt_grid {
                alt.resize_no_reflow_with_policy(rows, cols, policy);
            }
            pending
        };
        self.take_back_saved_cursors(cols_changed || self.grid.rows() != rows_before);
        self.take_back_main_anchors(anchors);
        self.finalize_resize(cols_changed, rows_before, origin);
        pending
    }

    /// Re-attach a scrollback store rewrapped off-thread, or return a new job if the width changed meanwhile.
    #[must_use = "drive the returned convergence job and re-attach it (or abort), or the detach window wedges"]
    pub fn finish_resize_offload(
        &mut self,
        reflowed: aterm_grid::ReflowedScrollback,
    ) -> Option<aterm_grid::PendingScrollbackReflow> {
        self.transient.sync_end_caret = None;
        self.lend_saved_cursors();
        let anchors = self.lend_main_anchors(aterm_grid::LendReach::Ring);
        let target = if self.modes.alternate_screen {
            self.alt_grid.as_mut()
        } else {
            Some(&mut self.grid)
        };
        let next = target.and_then(|grid| grid.reattach_reflowed_scrollback_or_redetach(reflowed));
        // The resize that detached the scrollback marked the slots; the re-attach changes no size.
        self.take_back_saved_cursors(false);
        self.take_back_main_anchors(anchors);
        next
    }

    /// Abort an offloaded resize whose reflow will never re-attach.
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

    /// Attach or detach the off-thread compression worker on the primary-content grid.
    #[inline]
    pub const fn set_compress_offload_active(&mut self, active: bool) {
        let target = if self.modes.alternate_screen {
            self.alt_grid.as_mut()
        } else {
            Some(&mut self.grid)
        };
        if let Some(grid) = target {
            grid.set_compress_offload_active(active);
        }
    }

    /// Lines staged in the primary-content grid's lazy buffer awaiting off-thread promotion.
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

    /// Drain up to `max_lines` of the primary-content grid's staged backlog into its tiered store.
    #[inline]
    pub fn drain_lazy_bounded(&mut self, max_lines: usize) -> usize {
        let target = if self.modes.alternate_screen {
            self.alt_grid.as_mut()
        } else {
            Some(&mut self.grid)
        };
        target.map_or(0, |grid| grid.drain_lazy_bounded(max_lines))
    }

    /// Run a pressure-relief batch on the primary-content backlog without promoting an open cut's marker.
    #[inline]
    pub fn drain_lazy_pressure_bounded(&mut self, max_lines: usize) -> usize {
        let target = if self.modes.alternate_screen {
            self.alt_grid.as_mut()
        } else {
            Some(&mut self.grid)
        };
        target.map_or(0, |grid| grid.drain_lazy_pressure_bounded(max_lines))
    }

    /// Run one bounded drain opportunity on the primary-content backlog when not mid-flood.
    #[inline]
    pub fn trickle_lazy_bounded(&mut self, max_lines: usize) -> usize {
        let target = if self.modes.alternate_screen {
            self.alt_grid.as_mut()
        } else {
            Some(&mut self.grid)
        };
        target.map_or(0, |grid| grid.trickle_lazy_bounded(max_lines))
    }

    /// Apply shared post-resize side effects (journal, selection adjustment, DEC 2048 report, and invariants).
    fn finalize_resize(
        &mut self,
        cols_changed: bool,
        rows_before: u16,
        origin: super::resize_journal::ResizeOrigin,
    ) {
        self.transient.sync_end_caret = None;
        let undone = self.grid.last_resize_shape().undone;
        self.journal_resize(origin);
        if cols_changed {
            self.resize_undo_selection = None;
            self.text_selection.clear();
            self.parked_text_selection.clear();
        } else {
            let selection_before = self.text_selection.clone();
            let revealed = i32::from(self.grid.take_last_resize_row_shift());
            let max_rows = i32::from(self.grid.rows());
            let demoted = i32::from(rows_before.saturating_sub(self.grid.rows()));
            let floor = i32::try_from(self.grid.scrollback_lines()).unwrap_or(i32::MAX);
            if demoted > 0 {
                self.text_selection
                    .adjust_for_rows_shrink(max_rows, revealed, floor);
            } else {
                self.text_selection
                    .adjust_for_scroll(-revealed, max_rows, floor);
            }
            self.follow_resize_undo_selection(selection_before, undone);
            if let Some(parked_grid) = self.alt_grid.as_mut() {
                let parked_revealed = i32::from(parked_grid.take_last_resize_row_shift());
                let parked_floor =
                    i32::try_from(parked_grid.scrollback_lines()).unwrap_or(i32::MAX);
                if demoted > 0 {
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
        if self.modes.in_band_size_reports {
            super::state_accessors::push_in_band_size_report(
                &mut self.transient,
                self.grid.rows(),
                self.grid.cols(),
                self.iterm2.host_cell_px(),
            );
        }
        #[cfg(debug_assertions)]
        self.grid.assert_structural_invariants();
    }

    /// Install the callback invoked on the first palette set refused by `allow_palette_reconfigure`.
    #[inline]
    pub fn set_palette_refused_callback<F: FnMut() + Send + 'static>(&mut self, callback: F) {
        self.color.palette_refused_callback = Some(Box::new(callback));
    }

    /// Whether this session has refused a palette set.
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

    /// Install the host resolver for Kitty non-direct transmission mediums (`t=f`/`t=t`/`t=s`).
    #[inline]
    pub fn set_kitty_file_resolver<F>(&mut self, resolver: F)
    where
        F: Fn(crate::terminal::kitty_graphics::KittyMedium, &str) -> Option<Vec<u8>>
            + Send
            + 'static,
    {
        self.kitty_file_resolver.install(Box::new(resolver));
    }

    /// Remove the Kitty non-direct medium resolver, so `t=f`/`t=t`/`t=s` are refused as medium
    /// disabled again.
    #[inline]
    pub fn clear_kitty_file_resolver(&mut self) {
        self.kitty_file_resolver.uninstall();
    }

    /// Set the desktop notification callback for OSC 9 messages.
    #[inline]
    pub fn set_notification_callback<F: FnMut(&str) + Send + 'static>(&mut self, callback: F) {
        self.notifications.sinks.set_callback(Box::new(callback));
    }

    /// Set the callback for complete OSC 99 and OSC 777 notifications.
    #[inline]
    pub fn set_advanced_notification_callback<F: FnMut(types::Notification) + Send + 'static>(
        &mut self,
        callback: F,
    ) {
        self.notifications
            .sinks
            .set_advanced_callback(Box::new(callback));
    }

    /// Set the host's admission for each program notification `allow_notifications` lets
    /// through (OSC 9, a complete OSC 99, OSC 777), asked with the program's own state report
    /// (OSC 7501, [`super::program_state`]) as it stands at the notification's byte: a report
    /// written after it in the same output is not yet taken. `false` drops the notification
    /// before either notification callback sees it.
    #[inline]
    pub fn set_notification_admission<F>(&mut self, admit: F)
    where
        F: FnMut(&super::program_state::ProgramStateReport) -> bool + Send + 'static,
    {
        self.notifications.sinks.set_admission(Box::new(admit));
    }

    /// Set the OSC 52 clipboard callback.
    #[inline]
    pub fn set_clipboard_callback<F>(&mut self, callback: F)
    where
        F: FnMut(ClipboardOperation) -> Option<String> + Send + 'static,
    {
        self.clipboard.callback = Some(Box::new(callback));
    }

    /// Set the XTWINOPS (`CSI t`) window manipulation and query callback.
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

    /// Build a terminal whose bulk history lives in the tiered store.
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

    fn visible_rows(t: &Terminal) -> Vec<String> {
        (0..t.rows())
            .map(|r| {
                t.get_line_text(i32::from(r), None)
                    .unwrap_or_default()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn retained_rows(t: &Terminal) -> Vec<String> {
        let grid = t.grid();
        let mut rows: Vec<String> = (0..grid.scrollback_lines())
            .map(|i| {
                grid.get_history_line(i)
                    .map(|l| l.to_string().trim_end().to_string())
                    .unwrap_or_default()
            })
            .collect();
        rows.extend(visible_rows(t));
        rows
    }

    /// A width-change offload on the primary screen equals a synchronous resize across detach, reflow, and re-attach.
    #[test]
    fn offloaded_resize_primary_preserves_scrollback() {
        let mut t = term_with_history(24, 80, 500);
        let mut sync = term_with_history(24, 80, 500);
        let before = t.grid.scrollback_lines();
        assert!(before > 100, "precondition: deep history ({before} lines)");

        sync.resize(24, 40);

        let pending = t
            .resize_offloading_scrollback(24, 40)
            .expect("a width change with tiered history yields an offload job");
        assert!(
            pending.line_count() > 100,
            "the detached job carries the deep history (lazy + tiered)"
        );
        assert_eq!(t.grid.cols(), 40, "visible grid resized synchronously");

        let reflowed = pending.reflow();
        assert!(
            t.finish_resize_offload(reflowed).is_none(),
            "widths agree — no convergence pass expected"
        );

        assert_eq!(
            t.grid.scrollback_lines(),
            sync.grid.scrollback_lines(),
            "offloaded primary history line count matches synchronous resize"
        );
        assert_eq!(
            retained_rows(&t),
            retained_rows(&sync),
            "offloaded primary history and visible rows match synchronous resize"
        );
        assert_eq!(
            (t.grid.cursor_row(), t.grid.cursor_col()),
            (sync.grid.cursor_row(), sync.grid.cursor_col()),
            "offloaded primary cursor matches synchronous resize"
        );
    }

    /// An alt-screen resize offloads and matches a synchronous resize on both screens.
    #[test]
    fn offloaded_resize_alt_screen_preserves_saved_primary() {
        let mut t = term_with_history(24, 80, 500);
        let mut sync = term_with_history(24, 80, 500);
        assert!(t.grid.scrollback_lines() > 100);
        t.process(b"\x1b[?1049halt-row"); // enter alt screen (primary saved off)
        sync.process(b"\x1b[?1049halt-row");

        sync.resize(24, 40);

        let pending = t
            .resize_offloading_scrollback(24, 40)
            .expect("alt-screen width change offloads the saved primary's history");
        let reflowed = pending.reflow();
        assert!(
            t.finish_resize_offload(reflowed).is_none(),
            "widths agree — no convergence pass expected"
        );
        assert_eq!(visible_rows(&t), visible_rows(&sync));
        assert_eq!(
            (t.grid.cursor_row(), t.grid.cursor_col()),
            (sync.grid.cursor_row(), sync.grid.cursor_col())
        );

        t.process(b"\x1b[?1049l"); // exit alt → saved primary is active again
        sync.process(b"\x1b[?1049l");
        assert_eq!(
            t.grid.scrollback_lines(),
            sync.grid.scrollback_lines(),
            "saved-primary history line count matches synchronous resize"
        );
        assert_eq!(
            retained_rows(&t),
            retained_rows(&sync),
            "saved-primary history and visible rows match synchronous resize"
        );
        assert_eq!(
            (t.grid.cursor_row(), t.grid.cursor_col()),
            (sync.grid.cursor_row(), sync.grid.cursor_col()),
            "restored primary cursor matches synchronous resize"
        );
    }

    /// `trickle_lazy_bounded` skips during a flood on the saved primary's backlog and promotes once idle.
    #[test]
    fn trickle_skips_a_flood_that_outruns_it_on_the_backlog_the_worker_drains() {
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
