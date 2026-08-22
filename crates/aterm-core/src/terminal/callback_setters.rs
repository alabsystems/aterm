// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Callback registration API for Terminal.
//!
//! Contains all `set_*_callback` methods, `resize()`, and related state queries.
//! Extracted from mod.rs to reduce file size.

use super::{
    ClipboardOperation, CopyToClipboardOperation, Terminal, WindowOperation, WindowResponse, types,
};

impl Terminal {
    /// Resize the terminal.
    ///
    /// The active grid is resized with reflow appropriate to its type:
    /// - Primary screen: reflow enabled (soft-wrapped lines unwrap/rewrap)
    /// - Alt screen: reflow disabled (app manages layout, redraws after SIGWINCH)
    ///
    /// The inactive grid (saved in `alt_grid`) uses the opposite reflow mode.
    /// This matches xterm, Alacritty, kitty, and Terminal behavior (#4164).
    ///
    /// Dimensions are clamped by the grid to
    /// `1..=`[`MAX_GRID_ROWS`](crate::grid::MAX_GRID_ROWS)`/`[`MAX_GRID_COLS`](crate::grid::MAX_GRID_COLS)
    /// (§5.8 ingress bound), so a hostile resize cannot request an
    /// arbitrarily large cell allocation.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        // Captured BEFORE the resize: afterwards both grids carry the new width.
        let cols_changed = self.grid.cols() != cols;
        if self.modes.alternate_screen {
            // Alt screen active: don't reflow current grid (app-managed content).
            // Saved primary grid should reflow normally.
            self.grid.resize_no_reflow(rows, cols);
            if let Some(ref mut saved_primary) = self.alt_grid {
                saved_primary.resize(rows, cols);
            }
        } else {
            // Primary screen active: reflow current grid.
            // Alt grid (if present) should not be reflowed.
            self.grid.resize(rows, cols);
            if let Some(ref mut alt) = self.alt_grid {
                alt.resize_no_reflow(rows, cols);
            }
        }
        self.finalize_resize(cols_changed);
    }

    /// Resize, but move the width-change off-screen scrollback rewrap OFF the
    /// caller's thread (the L0 whole-Mac-freeze fix).
    ///
    /// The visible grid(s) are resized synchronously (bounded by the viewport)
    /// and the unbounded tiered scrollback of the primary-content grid (the
    /// active primary, or the saved primary while an alt screen is up) is
    /// detached in O(1). The returned `Send` job carries that history for
    /// off-thread rewrap via [`PendingScrollbackReflow::reflow`]; pass the result
    /// to [`Terminal::finish_resize_offload`] to re-attach it. Returns `None`
    /// when there is nothing to offload (no width change / no tiered store), in
    /// which case this is equivalent to [`Terminal::resize`].
    pub fn resize_offloading_scrollback(
        &mut self,
        rows: u16,
        cols: u16,
    ) -> Option<aterm_grid::PendingScrollbackReflow> {
        // Captured BEFORE the resize, as in `resize`.
        let cols_changed = self.grid.cols() != cols;
        let pending = if self.modes.alternate_screen {
            // Alt active: current (alt) grid is app-managed; the SAVED PRIMARY
            // holds the scrollback that reflows.
            self.grid.resize_no_reflow(rows, cols);
            self.alt_grid
                .as_mut()
                .and_then(|saved_primary| saved_primary.resize_offloading_scrollback(rows, cols))
        } else {
            // Primary active: current grid reflows (offloaded); alt is unreflowed.
            let pending = self.grid.resize_offloading_scrollback(rows, cols);
            if let Some(ref mut alt) = self.alt_grid {
                alt.resize_no_reflow(rows, cols);
            }
            pending
        };
        self.finalize_resize(cols_changed);
        pending
    }

    /// Re-attach an off-thread-rewrapped scrollback store from a prior
    /// [`resize_offloading_scrollback`](Self::resize_offloading_scrollback), onto
    /// the primary-content grid (active primary, or the saved primary while an
    /// alt screen is up). A no-op if that grid re-acquired a tiered store while
    /// the reflow ran (see [`Grid::reattach_reflowed_scrollback`]).
    ///
    /// CONVERGENCE (RFL-3): if the grid's width changed while the reflow ran
    /// (a superseding drag step, throttled to detach nothing at the time), the
    /// just-attached store is wrapped at a stale width; it is immediately
    /// re-detached at the CURRENT width and returned. Drive the returned job
    /// exactly like the original (`reflow`/`reflow_step`, then this method
    /// again) — at most one extra pass per settled drag. Dropping it without
    /// [`Self::abort_resize_offload`] would wedge the detach window — hence
    /// the `must_use`.
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

    /// Abort an in-flight offloaded resize whose reflow will NEVER re-attach — the
    /// worker panicked or its thread died before
    /// [`finish_resize_offload`](Self::finish_resize_offload). Returns the
    /// primary-content grid (active primary, or the saved primary under an alt
    /// screen) to a bounded state so the detach window does not wedge the grid for
    /// the rest of the session; see [`Grid::abort_reflow_offload`]. Idempotent /
    /// a no-op if the grid already re-attached.
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

    /// THRU-5: attach (or detach) the off-thread compression worker on the
    /// primary-content grid (active primary, or the saved primary under an alt
    /// screen) — the grid that owns the tiered scrollback. While attached, the
    /// reader-thread ingest path defers lazy-buffer draining to the worker so the
    /// LZ4/zstd promotion spike stays off the PTY-drain critical path. See
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

    /// THRU-5: lines staged in the primary-content grid's lazy buffer awaiting
    /// off-thread promotion — the compression worker's backlog (0 if none). The
    /// worker polls this to decide whether to keep draining.
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

    /// THRU-5: drain up to `max_lines` of the primary-content grid's staged
    /// backlog into its tiered store (bounded LZ4/zstd promotion), returning the
    /// number of lines still staged afterward. The caller holds the term lock;
    /// this is the worker's per-batch drive that keeps each hold short. See
    /// [`Grid::drain_lazy_bounded`](aterm_grid::Grid::drain_lazy_bounded).
    pub fn drain_lazy_bounded(&mut self, max_lines: usize) -> usize {
        let target = if self.modes.alternate_screen {
            self.alt_grid.as_mut()
        } else {
            Some(&mut self.grid)
        };
        target.map_or(0, |grid| grid.drain_lazy_bounded(max_lines))
    }

    /// Shared post-resize side effects for [`resize`](Self::resize) and the
    /// offloaded path: selection invalidation, the DEC-2048 in-band size report,
    /// and the debug structural-invariant self-check.
    fn finalize_resize(&mut self, cols_changed: bool) {
        // SELECTION CUSTODY Phase 3: only a WIDTH change invalidates coordinates.
        //
        // This used to clear unconditionally, on the reasoning that "reflow
        // invalidates all row/col coordinates (#4056)". True for a width change —
        // rewrap renumbers rows — but a ROWS-ONLY resize (window height, font zoom,
        // a horizontal divider drag) does not rewrap anything: row identity is
        // intact and only `visible_rows` moved. Clearing there threw away a
        // selection for a resize that could not have invalidated it.
        //
        // The rows-only case re-runs the EXISTING, kani-proven range check rather
        // than a second implementation of the same bounds reasoning: delta 0, the
        // new live-bottom row count, and the actual scrollback floor.
        //
        // `cols_changed` must be captured by the CALLER before the resize — by the
        // time we run, both grids already carry the new width and the old one is
        // gone.
        if cols_changed {
            self.text_selection.clear();
        } else {
            // A rows-GROW that revealed retained history re-labelled the newest ring
            // lines as the top of the viewport, so every pre-resize row — the
            // selection's anchors included — now sits `revealed` rows FURTHER DOWN.
            // `Grid::resize_with_reflow_mode` already follows that shift for the
            // cursor and the saved cursor; the selection is compensated here, because
            // it lives on the Terminal rather than the Grid.
            //
            // `adjust_for_scroll`'s delta is SUBTRACTED from each anchor row, so
            // moving content DOWN by `revealed` is a delta of `-revealed`. Passing 0
            // (as this did when the narrowing first landed) leaves the anchors above
            // their content: the highlight covers different text and a copy returns
            // something the user never selected. Read from the ACTIVE grid — `resize`
            // resizes both grids and each records its own shift.
            let revealed = i32::from(self.grid.take_last_resize_row_shift());
            let max_rows = i32::from(self.grid.rows());
            let floor = i32::try_from(self.grid.scrollback_lines()).unwrap_or(i32::MAX);
            self.text_selection
                .adjust_for_scroll(-revealed, max_rows, floor);
        }
        // DEC mode 2048: emit an in-band size report on every resize so a
        // subscribed app (neovim 0.10+) learns the new geometry without an ioctl.
        // Honor the response-buffer cap: like the DECSET arm, this writes the
        // buffer directly (not via the capped send_response sink), so a resize
        // storm against a host that is not draining responses must not grow it
        // past MAX_RESPONSE_BUFFER_SIZE. The report is <= 48 bytes.
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
        // INTEGRITY-SELFCHECK (M7): resize/reflow is the other major grid mutation
        // boundary; validate the structural invariants here too in debug builds.
        // Free in release; covered by the reflow proptests (no false-fail).
        #[cfg(debug_assertions)]
        self.grid.assert_structural_invariants();
    }

    /// Set bell callback.
    pub fn set_bell_callback<F: FnMut() + Send + 'static>(&mut self, callback: F) {
        self.bell_callback = Some(Box::new(callback));
    }

    /// Install the host resolver for Kitty NON-DIRECT transmission mediums (`t=f`/
    /// `t=t`/`t=s`). The engine hands the host `(medium, path-or-name)` and the host
    /// returns the raw image bytes (or `None` to reject). The HOST owns the I/O and
    /// the security policy — whether to read host files / shared memory off a terminal
    /// escape at all (fail-closed: default is no resolver ⇒ non-direct mediums are
    /// skipped), which paths are permitted, and any size cap. The engine itself never
    /// touches the filesystem or shared memory, so it stays pure and wasm-safe.
    pub fn set_kitty_file_resolver<F>(&mut self, resolver: F)
    where
        F: Fn(crate::terminal::kitty_graphics::KittyMedium, &str) -> Option<Vec<u8>>
            + Send
            + 'static,
    {
        self.kitty_file_resolver = Some(Box::new(resolver));
    }

    /// Clear bell callback.
    #[allow(
        dead_code,
        reason = "cleared via the FFI app-callback layer (ffi_bridge/)"
    )]
    pub(crate) fn clear_bell_callback(&mut self) {
        self.bell_callback = None;
    }

    /// Set cursor style change callback (DECSCUSR).
    ///
    /// The callback is invoked when a DECSCUSR sequence changes the cursor style.
    /// The UI layer should use this to start/stop cursor blink timers and update
    /// cursor rendering.
    pub fn set_cursor_style_callback<F: FnMut(aterm_types::CursorStyle) + Send + 'static>(
        &mut self,
        callback: F,
    ) {
        self.cursor_style_callback = Some(Box::new(callback));
    }

    /// Set buffer activation callback.
    ///
    /// The callback is invoked when the terminal switches between the main and
    /// alternate screen buffers. The boolean parameter is `true` when switching
    /// to the alternate screen, `false` when switching back to the main screen.
    ///
    /// This is useful for SwiftTerm integration where `bufferActivated` callback
    /// needs to be notified of buffer switches (e.g., when vim/less starts).
    pub fn set_buffer_activation_callback<F: FnMut(bool) + Send + 'static>(&mut self, callback: F) {
        self.buffer_activation_callback = Some(Box::new(callback));
    }

    /// Clear buffer activation callback.
    #[allow(
        dead_code,
        reason = "cleared via the FFI app-callback layer (ffi_bridge/)"
    )]
    pub(crate) fn clear_buffer_activation_callback(&mut self) {
        self.buffer_activation_callback = None;
    }

    /// Set title change callback.
    pub fn set_title_callback<F: FnMut(&str) + Send + 'static>(&mut self, callback: F) {
        self.title.callback = Some(Box::new(callback));
    }

    /// Clear title change callback.
    #[allow(
        dead_code,
        reason = "cleared via the FFI app-callback layer (ffi_bridge/)"
    )]
    pub(crate) fn clear_title_callback(&mut self) {
        self.title.callback = None;
    }

    /// Set title event callback with type discriminator (v3).
    ///
    /// The callback receives the title type (WindowAndIcon, IconOnly, WindowOnly)
    /// and the title text for all OSC 0/1/2 title changes.
    pub fn set_title_event_callback<F: FnMut(aterm_types::TitleType, &str) + Send + 'static>(
        &mut self,
        callback: F,
    ) {
        self.title.event_callback = Some(Box::new(callback));
    }

    /// Clear title event callback.
    #[allow(
        dead_code,
        reason = "cleared via the FFI app-callback layer (ffi_bridge/)"
    )]
    pub(crate) fn clear_title_event_callback(&mut self) {
        self.title.event_callback = None;
    }

    /// Set desktop notification callback (OSC 9).
    ///
    /// The callback is invoked when an application sends a notification escape
    /// sequence (OSC 9 without a subcommand). The UI layer should display a
    /// system notification with the provided message.
    ///
    /// # Example
    ///
    /// ```text
    /// terminal.set_notification_callback(|message| {
    ///     // Display system notification with the message
    ///     show_notification("Terminal", message);
    /// });
    /// ```
    ///
    /// The callback receives the notification message as a `&str`.
    ///
    /// # Supported Sequences
    ///
    /// - `ESC ] 9 ; message BEL` - Simple notification (Terminal/ConEmu style)
    /// - `ESC ] 9 ; message ST`  - ST terminator variant
    pub fn set_notification_callback<F: FnMut(&str) + Send + 'static>(&mut self, callback: F) {
        self.notifications.callback = Some(Box::new(callback));
    }

    /// Clear desktop notification callback.
    #[allow(
        dead_code,
        reason = "cleared via the FFI app-callback layer (ffi_bridge/)"
    )]
    pub(crate) fn clear_notification_callback(&mut self) {
        self.notifications.callback = None;
    }

    /// Set a callback for dynamic color changes (OSC 10/11/12, OSC 110/111/112).
    ///
    /// The callback is invoked when the terminal's default foreground, background,
    /// or cursor color changes via escape sequences.
    ///
    /// # Arguments
    ///
    /// The callback receives:
    /// - `target: ColorTarget` — which color changed (foreground, background, cursor, etc.)
    /// - `color: Rgb` — the new color value
    /// - `op: ColorChangeOp` — whether the color was set, reset, or made dynamic
    ///
    /// # Example
    ///
    /// ```text
    /// use aterm_core::terminal::ColorTarget;
    ///
    /// terminal.set_color_change_callback(|target, color, op| {
    ///     match target {
    ///         ColorTarget::Foreground => update_fg(color),
    ///         ColorTarget::Background => update_bg(color),
    ///         ColorTarget::Cursor => update_cursor(color),
    ///         _ => {}
    ///     }
    /// });
    /// ```
    pub fn set_color_change_callback<F>(&mut self, callback: F)
    where
        F: FnMut(super::ColorTarget, aterm_types::Rgb, super::ColorChangeOp) + Send + 'static,
    {
        self.color.change_callback = Some(Box::new(callback));
    }

    /// Clear the color change callback.
    pub fn clear_color_change_callback(&mut self) {
        self.color.change_callback = None;
    }

    /// Set a callback for dynamic color queries (OSC 10/11/12 with `?`).
    ///
    /// When an application queries the terminal's foreground, background, or
    /// cursor color, this callback is consulted first. If it returns
    /// `Some(Rgb)`, that color is used in the escape sequence response
    /// instead of the terminal's internal palette value. Returning `None`
    /// falls back to the palette color.
    ///
    /// This is useful when the host UI renders different colors than the
    /// terminal palette stores (e.g., theme overrides, system appearance).
    ///
    /// # Arguments
    ///
    /// The callback receives a [`ColorTarget`](super::ColorTarget):
    /// - `Foreground` — OSC 10 `?`
    /// - `Background` — OSC 11 `?`
    /// - `Cursor` — OSC 12 `?`
    pub fn set_color_query_callback<F>(&mut self, callback: F)
    where
        F: FnMut(super::ColorTarget) -> Option<aterm_types::Rgb> + Send + 'static,
    {
        self.color.query_callback = Some(Box::new(callback));
    }

    /// Clear the color query callback.
    pub fn clear_color_query_callback(&mut self) {
        self.color.query_callback = None;
    }

    /// Set advanced desktop notification callback (OSC 99/777).
    ///
    /// The callback is invoked when a complete notification is received via OSC 99
    /// (kitty protocol) or OSC 777 (rxvt-unicode protocol).
    ///
    /// The kitty notification protocol supports:
    /// - Separate title and body
    /// - Urgency levels (low, normal, critical)
    /// - Notification IDs for updates
    ///
    /// OSC 777 format: `ESC ] 777 ; notify ; title ; body ST`
    ///
    /// # Example
    ///
    /// ```text
    /// use aterm_core::terminal::{Notification, NotificationUrgency};
    ///
    /// terminal.set_advanced_notification_callback(|notification| {
    ///     let title = notification.title.as_deref().unwrap_or("Terminal");
    ///     let body = notification.body.as_deref().unwrap_or("");
    ///     let urgent = matches!(notification.urgency, NotificationUrgency::Critical);
    ///     show_system_notification(title, body, urgent);
    /// });
    /// ```
    ///
    /// # Supported Sequences
    ///
    /// - `ESC ] 99 ; i=ID:p=title:d=0 ST` + `ESC ] 99 ; i=ID:p=body:d=1 ST`
    /// - `ESC ] 99 ; p=body:u=2:d=1 ST` - Single message with critical urgency
    /// - `ESC ] 777 ; notify ; title ; body ST` - rxvt-unicode style notification
    ///
    /// See <https://sw.kovidgoyal.net/kitty/desktop-notifications/> for protocol details.
    pub fn set_advanced_notification_callback<F: FnMut(types::Notification) + Send + 'static>(
        &mut self,
        callback: F,
    ) {
        self.notifications.advanced_callback = Some(Box::new(callback));
    }

    /// Clear advanced notification callback (OSC 99/777).
    pub fn clear_advanced_notification_callback(&mut self) {
        self.notifications.advanced_callback = None;
    }

    /// Set clipboard callback for OSC 52 operations.
    ///
    /// The callback is invoked when an application sends OSC 52 to set or clear
    /// the clipboard, and (optionally) when querying clipboard contents.
    ///
    /// Clipboard queries (Pd = "?") are ignored by default for security.
    ///
    /// To enable queries:
    /// - Rust API: set `TerminalConfig::allow_osc52_query = true` via
    ///   [`apply_config`](Self::apply_config).
    /// - Direct toggle: [`set_osc52_query_allowed`](Self::set_osc52_query_allowed).
    ///
    /// The callback receives a [`ClipboardOperation`] and should:
    /// - For `Set` operations: copy the content to the appropriate clipboard(s)
    /// - For `Query` operations: return the clipboard content (or None if denied)
    /// - For `Clear` operations: clear the clipboard content
    ///
    /// # Example
    ///
    /// ```text
    /// use aterm_core::terminal::{ClipboardOperation, Terminal};
    ///
    /// let mut terminal = Terminal::new(24, 80);
    /// terminal.set_clipboard_callback(|op| {
    ///     match op {
    ///         ClipboardOperation::Set { content, .. } => {
    ///             // Copy to system clipboard (platform-specific)
    ///             println!("Set clipboard: {}", content);
    ///             None
    ///         }
    ///         ClipboardOperation::Query { .. } => {
    ///             // Return clipboard content (or None to deny)
    ///             Some("clipboard content".to_string())
    ///         }
    ///         ClipboardOperation::Clear { .. } => {
    ///             // Clear clipboard
    ///             None
    ///         }
    ///     }
    /// });
    /// ```
    pub fn set_clipboard_callback<F>(&mut self, callback: F)
    where
        F: FnMut(ClipboardOperation) -> Option<String> + Send + 'static,
    {
        self.clipboard.callback = Some(Box::new(callback));
    }

    /// Set a callback for OSC 1337 named pasteboard operations.
    ///
    /// This callback handles Terminal-style clipboard operations:
    /// - `CopyToClipboard=name` + `EndCopy`: Text capture to named pasteboard
    /// - `Copy=base64`: Direct copy of base64-decoded data
    ///
    /// Named pasteboards (on macOS) include "general", "rule", "find", "font".
    /// An empty pasteboard name typically means the general (system) clipboard.
    ///
    /// # Example
    ///
    /// ```text
    /// use aterm_core::terminal::{Terminal, CopyToClipboardOperation};
    ///
    /// let mut term = Terminal::new(24, 80);
    /// term.set_copy_to_clipboard_callback(|op| {
    ///     match op {
    ///         CopyToClipboardOperation::CaptureComplete { pasteboard, content } => {
    ///             println!("Copy to pasteboard '{}': {}", pasteboard, content);
    ///         }
    ///         CopyToClipboardOperation::DirectCopy { content } => {
    ///             println!("Direct copy: {}", content);
    ///         }
    ///     }
    /// });
    /// ```
    pub fn set_copy_to_clipboard_callback<F>(&mut self, callback: F)
    where
        F: FnMut(CopyToClipboardOperation) + Send + 'static,
    {
        self.clipboard.copy_callback = Some(Box::new(callback));
    }

    /// Check if a CopyToClipboard capture is currently active.
    ///
    /// Returns `true` if OSC 1337 CopyToClipboard was received but EndCopy has
    /// not yet been processed.
    #[must_use]
    pub fn is_copy_to_clipboard_active(&self) -> bool {
        self.clipboard.copy_state.is_some()
    }

    /// Set a callback for DCS payloads.
    ///
    /// The callback receives the raw DCS data bytes (payload only) and the final byte.
    /// Payload data is capped to a fixed size to avoid unbounded buffering.
    pub fn set_dcs_callback<F>(&mut self, callback: F)
    where
        F: FnMut(&[u8], u8) + Send + 'static,
    {
        self.dcs.callback = Some(Box::new(callback));
    }

    /// Clear the DCS callback.
    pub fn clear_dcs_callback(&mut self) {
        self.dcs.callback = None;
    }

    /// Set a callback for window operations (CSI t - XTWINOPS).
    ///
    /// The callback is invoked when window manipulation or query sequences are received.
    /// For manipulation operations (iconify, move, resize), perform the operation and
    /// return `None`. For query operations (report state, position, size), return the
    /// appropriate `WindowResponse`.
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
    pub fn set_window_callback<F>(&mut self, callback: F)
    where
        F: FnMut(WindowOperation) -> Option<WindowResponse> + Send + 'static,
    {
        self.window_callback = Some(Box::new(callback));
    }

    /// Clear window callback.
    #[allow(
        dead_code,
        reason = "cleared via the FFI app-callback layer (ffi_bridge/)"
    )]
    pub(crate) fn clear_window_callback(&mut self) {
        self.window_callback = None;
    }

    /// Get the current remote host (OSC 1337 RemoteHost).
    ///
    /// Returns `Some(RemoteHost)` if in an SSH session (as reported by the shell
    /// via OSC 1337 RemoteHost=user@host), or `None` if in a local session.
    ///
    /// # Example
    ///
    /// ```text
    /// use aterm_core::terminal::Terminal;
    ///
    /// let mut term = Terminal::new(24, 80);
    /// term.process(b"\x1b]1337;RemoteHost=alice@server.example.com\x07");
    /// if let Some(host) = term.remote_host() {
    ///     println!("Connected to {}@{}", host.user, host.hostname);
    /// }
    /// ```
    #[must_use]
    pub fn remote_host(&self) -> Option<&types::RemoteHost> {
        self.iterm2.remote_host.as_ref()
    }

    /// Set a callback for remote host change events.
    ///
    /// Called when OSC 1337 RemoteHost changes the current host (connect or
    /// disconnect). The callback receives `None` when returning to local session.
    ///
    /// # Example
    ///
    /// ```
    /// use aterm_core::terminal::Terminal;
    ///
    /// let mut term = Terminal::new(24, 80);
    /// term.set_remote_host_callback(|host| {
    ///     match host {
    ///         Some(h) => println!("SSH to {}@{}", h.user, h.hostname),
    ///         None => println!("Back to local session"),
    ///     }
    /// });
    /// ```
    #[cfg(test)]
    pub fn set_remote_host_callback<F>(&mut self, callback: F)
    where
        F: FnMut(Option<&types::RemoteHost>) + Send + 'static,
    {
        self.iterm2.remote_host_callback = Some(Box::new(callback));
    }

    /// Set a callback for text sizing events (OSC 66 - Kitty protocol).
    ///
    /// Called when text sizing escape sequences are received. The operation
    /// includes scale, width, alignment parameters, and the text content.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use aterm_core::terminal::Terminal;
    /// use aterm_core::testing::set_text_sizing_callback;
    ///
    /// let mut term = Terminal::new(24, 80);
    /// set_text_sizing_callback(&mut term, |op| {
    ///     println!("Text: {}, scale: {:?}", op.text, op.scale);
    /// });
    /// ```
    #[cfg(test)]
    pub(crate) fn set_text_sizing_callback<F>(&mut self, callback: F)
    where
        F: FnMut(types::TextSizingOperation) + Send + 'static,
    {
        self.text_sizing_callback = Some(Box::new(callback));
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

    /// A width-change offload on the PRIMARY screen rewraps history off-thread and
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

    /// Claude Code runs in the ALT screen; a resize there must still offload — and
    /// preserve — the SAVED PRIMARY's scrollback (this is the exact context of the
    /// reported freeze).
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
}
