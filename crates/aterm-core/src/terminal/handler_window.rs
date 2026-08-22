// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Window operations handler for the terminal.
//!
//! This module contains handlers for XTWINOPS (window manipulation) sequences:
//! - Window state: iconify/de-iconify, raise/lower
//! - Window geometry: move, resize (pixels and cells)
//! - Maximize/fullscreen operations
//! - State queries: window state, position, size
//! - Title stack: push/pop window and icon titles
//!
//! Most operations invoke a platform callback; some (text area size,
//! title reports) can be answered directly from terminal state.
//!
//! Extracted from handler.rs as part of large files refactor.

use std::sync::Arc;

use super::TITLE_STACK_MAX_DEPTH;
use super::handler::TerminalHandler;
use super::response_capability::ResponseCapability;
use super::window_auth::{WindowMintAuthority, WindowOpsCapability};
use aterm_types::{WindowOperation, WindowResponse};

/// Decode the title stack sub-parameter: 0/default=both, 1=icon, 2=window.
#[inline]
fn title_stack_targets(params: &[u16]) -> (bool, bool) {
    match params.get(1).copied().unwrap_or(0) {
        1 => (true, false), // Icon only
        2 => (false, true), // Window only
        _ => (true, true),  // Both (default)
    }
}

impl TerminalHandler<'_> {
    /// Handle XTWINOPS - xterm window manipulation and queries.
    ///
    /// CSI Ps ; Ps ; Ps t
    ///
    /// This handles window manipulation and query operations.
    /// For manipulation operations, the platform callback is invoked.
    /// For some operations (title push/pop, text area reports), the terminal
    /// handles them directly without requiring a callback.
    ///
    /// # Capability gates (CF-003 + CF-008)
    ///
    /// The window-operations mint authority is consulted once per XTWINOPS
    /// dispatch against the host's `allow_window_ops` policy bit. When the
    /// host has authorized window ops, a [`WindowOpsCapability`] is minted
    /// and threaded down to the handlers that call
    /// [`TerminalHandler::invoke_window_callback`]; when the host has
    /// not, the mint returns `None`, no capability exists, and the
    /// callback-invoking paths are structurally unreachable.
    ///
    /// The `response_cap: &ResponseCapability` is threaded separately to
    /// gate the `send_response` calls made by the report subcommands.
    ///
    /// Title-stack sub-operations (CSI 22t / 23t) do not invoke the
    /// window callback and therefore do not require a capability; they
    /// run regardless of the policy bit.
    ///
    /// See [`super::window_auth`] and [`super::response_capability`].
    pub(super) fn handle_xtwinops(&mut self, response_cap: &ResponseCapability, params: &[u16]) {
        let ps = params.first().copied().unwrap_or(0);
        let mint_authority = WindowMintAuthority::new();
        // Engine-consulting variant (#7994): when a policy is installed,
        // a matching rule (Execute | !Execute) wins over the legacy
        // `allow_window_ops` bool. On fallthrough the bool is authoritative
        // (design §6.3 Release N backward-compat).
        let window_cap = mint_authority.try_mint_with_engine(
            self.policy.xtwinops_gate(ps),
            self.modes.allow_window_ops,
        );

        match window_cap.as_ref() {
            Some(cap) => {
                if self.handle_window_state_or_geometry(ps, params, cap)
                    || self.handle_window_reports(response_cap, ps, params, cap)
                {
                    return;
                }
            }
            None => {
                // No capability minted — host has not authorized window ops.
                //
                // Silently drop manipulation (1–10), geometry/position/size
                // queries (11–19), and title reports (20–21). These all
                // either change window state on the host side or leak
                // information back to the PTY response buffer for
                // client fingerprinting (#7454, #7643, #7876).
                //
                // Only title stack push/pop (22–23) fall through — they
                // mutate the internal title stack without touching the
                // window callback or emitting any PTY response.
                if let 1..=21 = ps {
                    return;
                }
            }
        }
        self.handle_window_title_stack(ps, params);
    }

    /// Minimum window size in pixels for resize operations (#7139).
    ///
    /// Prevents remote servers from resizing the window to unusably small
    /// dimensions (e.g., 1x1 pixel).
    const MIN_RESIZE_PIXELS: u16 = 200;

    /// Minimum window size in cells for resize operations (#7139).
    const MIN_RESIZE_CELLS: u16 = 10;

    /// Dispatch XTWINOPS state/geometry subcommands (1–10).
    ///
    /// Reached only when the [`WindowOpsCapability`] has been minted,
    /// i.e. `allow_window_ops = true`. The capability is threaded to
    /// every `invoke_window_callback` call so the compiler enforces
    /// the authorization gate at each call site; a future subcommand
    /// that forgets to request a capability will not compile.
    fn handle_window_state_or_geometry(
        &mut self,
        ps: u16,
        params: &[u16],
        cap: &WindowOpsCapability,
    ) -> bool {
        // Security: CSI t subcommands 1-2 (iconify/de-iconify), 3 (move),
        // 4 (resize pixels), 8 (resize cells), 9 (maximize), and 10 (fullscreen)
        // allow remote servers to manipulate the window. Deny move and clamp
        // resize to safe minimums (#7139).
        //
        // The `allow_window_ops = false` deny branch lives in
        // `handle_xtwinops`: when the capability is not minted, we do not
        // reach this function at all (for subcommands 1–19) or this function
        // is entered only for the fall-through path (20+), which this match
        // does not claim.
        match ps {
            // Window state manipulation — allowed when window_ops enabled
            1 => {
                self.invoke_window_callback(WindowOperation::DeIconify, cap);
            }
            2 => {
                self.invoke_window_callback(WindowOperation::Iconify, cap);
            }

            // Window move — DENIED (#7139): remote move can push window off-screen
            3 => {
                // Silently ignore move requests from remote servers.
                // A malicious server could move the window off-screen to hide it.
            }

            // Window resize (pixels) — clamp to safe minimum (#7139)
            4 => {
                let height = params
                    .get(1)
                    .copied()
                    .unwrap_or(0)
                    .max(Self::MIN_RESIZE_PIXELS);
                let width = params
                    .get(2)
                    .copied()
                    .unwrap_or(0)
                    .max(Self::MIN_RESIZE_PIXELS);
                self.invoke_window_callback(
                    WindowOperation::ResizeWindowPixels { height, width },
                    cap,
                );
            }
            5 => {
                self.invoke_window_callback(WindowOperation::RaiseWindow, cap);
            }
            6 => {
                self.invoke_window_callback(WindowOperation::LowerWindow, cap);
            }
            7 => {
                self.invoke_window_callback(WindowOperation::RefreshWindow, cap);
            }

            // Window resize (cells) — clamp to safe minimum (#7139)
            8 => {
                let rows = params
                    .get(1)
                    .copied()
                    .unwrap_or(0)
                    .max(Self::MIN_RESIZE_CELLS);
                let cols = params
                    .get(2)
                    .copied()
                    .unwrap_or(0)
                    .max(Self::MIN_RESIZE_CELLS);
                self.invoke_window_callback(WindowOperation::ResizeWindowCells { rows, cols }, cap);
            }

            // Maximize/fullscreen (9-10) — allowed when window_ops enabled
            9 => {
                let sub = params.get(1).copied().unwrap_or(0);
                let op = Self::maximize_operation(sub);
                if let Some(op) = op {
                    self.invoke_window_callback(op, cap);
                }
            }
            10 => {
                let sub = params.get(1).copied().unwrap_or(0);
                let op = Self::fullscreen_operation(sub);
                if let Some(op) = op {
                    self.invoke_window_callback(op, cap);
                }
            }
            _ => return false,
        }
        true
    }

    /// Dispatch XTWINOPS report subcommands (11–21).
    ///
    /// Reached only when the [`WindowOpsCapability`] has been minted.
    /// `response_cap` gates `send_response` for the report paths.
    fn handle_window_reports(
        &mut self,
        response_cap: &ResponseCapability,
        ps: u16,
        params: &[u16],
        cap: &WindowOpsCapability,
    ) -> bool {
        match ps {
            // Report operations (11-21)
            11 => self.report_window_state(response_cap, cap),
            13 => {
                self.report_window_position(response_cap, params.get(1).copied().unwrap_or(0), cap);
            }
            14 => self.report_window_size_pixels(
                response_cap,
                params.get(1).copied().unwrap_or(0),
                cap,
            ),
            15 => self.report_screen_size_pixels(response_cap, cap),
            16 => self.report_cell_size_pixels(response_cap, cap),
            18 => self.report_text_area_size_cells(response_cap),
            19 => self.report_screen_size_cells(response_cap, cap),
            20 => self.report_icon_label(response_cap, cap),
            21 => self.report_window_title(response_cap, cap),
            _ => return false,
        }
        true
    }

    fn handle_window_title_stack(&mut self, ps: u16, params: &[u16]) {
        match ps {
            22 => {
                let (icon, window) = title_stack_targets(params);
                self.push_title(icon, window);
            }
            23 => {
                let (icon, window) = title_stack_targets(params);
                self.pop_title(icon, window);
            }
            _ => {}
        }
    }

    #[inline]
    fn maximize_operation(sub: u16) -> Option<WindowOperation> {
        match sub {
            0 => Some(WindowOperation::RestoreMaximized),
            1 => Some(WindowOperation::MaximizeWindow),
            2 => Some(WindowOperation::MaximizeVertically),
            3 => Some(WindowOperation::MaximizeHorizontally),
            _ => None,
        }
    }

    #[inline]
    fn fullscreen_operation(sub: u16) -> Option<WindowOperation> {
        match sub {
            0 => Some(WindowOperation::UndoFullscreen),
            1 => Some(WindowOperation::EnterFullscreen),
            2 => Some(WindowOperation::ToggleFullscreen),
            _ => None,
        }
    }

    fn report_window_state(
        &mut self,
        response_cap: &ResponseCapability,
        cap: &WindowOpsCapability,
    ) {
        if let Some(WindowResponse::WindowState(iconified)) =
            self.invoke_window_callback(WindowOperation::ReportWindowState, cap)
        {
            // CSI 1 t = not iconified, CSI 2 t = iconified
            let response = format!("\x1b[{}t", if iconified { 2 } else { 1 });
            self.send_response(response_cap, response.as_bytes());
        }
    }

    fn report_window_position(
        &mut self,
        response_cap: &ResponseCapability,
        sub: u16,
        cap: &WindowOpsCapability,
    ) {
        let op = if sub == 2 {
            WindowOperation::ReportTextAreaPosition
        } else {
            WindowOperation::ReportWindowPosition
        };
        if let Some(WindowResponse::Position { x, y }) = self.invoke_window_callback(op, cap) {
            // CSI 3 ; x ; y t
            let response = format!("\x1b[3;{x};{y}t");
            self.send_response(response_cap, response.as_bytes());
        }
    }

    fn report_window_size_pixels(
        &mut self,
        response_cap: &ResponseCapability,
        sub: u16,
        cap: &WindowOpsCapability,
    ) {
        let op = if sub == 2 {
            WindowOperation::ReportWindowSizePixels
        } else {
            WindowOperation::ReportTextAreaSizePixels
        };
        if let Some(WindowResponse::SizePixels { height, width }) =
            self.invoke_window_callback(op, cap)
        {
            // CSI 4 ; height ; width t
            let response = format!("\x1b[4;{height};{width}t");
            self.send_response(response_cap, response.as_bytes());
        }
    }

    fn report_screen_size_pixels(
        &mut self,
        response_cap: &ResponseCapability,
        cap: &WindowOpsCapability,
    ) {
        if let Some(WindowResponse::SizePixels { height, width }) =
            self.invoke_window_callback(WindowOperation::ReportScreenSizePixels, cap)
        {
            // CSI 5 ; height ; width t
            let response = format!("\x1b[5;{height};{width}t");
            self.send_response(response_cap, response.as_bytes());
        }
    }

    fn report_cell_size_pixels(
        &mut self,
        response_cap: &ResponseCapability,
        cap: &WindowOpsCapability,
    ) {
        if let Some(WindowResponse::CellSize { height, width }) =
            self.invoke_window_callback(WindowOperation::ReportCellSizePixels, cap)
        {
            // CSI 6 ; height ; width t
            let response = format!("\x1b[6;{height};{width}t");
            self.send_response(response_cap, response.as_bytes());
        }
    }

    fn report_text_area_size_cells(&mut self, response_cap: &ResponseCapability) {
        // This can be answered directly from grid state.
        let rows = self.grid.rows();
        let cols = self.grid.cols();
        let response = format!("\x1b[8;{rows};{cols}t");
        self.send_response(response_cap, response.as_bytes());
    }

    fn report_screen_size_cells(
        &mut self,
        response_cap: &ResponseCapability,
        cap: &WindowOpsCapability,
    ) {
        if let Some(WindowResponse::SizeCells { rows, cols }) =
            self.invoke_window_callback(WindowOperation::ReportScreenSizeCells, cap)
        {
            // CSI 9 ; rows ; cols t
            let response = format!("\x1b[9;{rows};{cols}t");
            self.send_response(response_cap, response.as_bytes());
        }
    }

    fn report_icon_label(&mut self, response_cap: &ResponseCapability, cap: &WindowOpsCapability) {
        let label = match self.invoke_window_callback(WindowOperation::ReportIconLabel, cap) {
            Some(WindowResponse::Title(title)) => title,
            _ => self.title.icon.to_string(),
        };
        let label = Self::filter_title_for_report(&label);
        // OSC L label ST
        let response = format!("\x1b]L{label}\x1b\\");
        self.send_response(response_cap, response.as_bytes());
    }

    fn report_window_title(
        &mut self,
        response_cap: &ResponseCapability,
        cap: &WindowOpsCapability,
    ) {
        let title = match self.invoke_window_callback(WindowOperation::ReportWindowTitle, cap) {
            Some(WindowResponse::Title(title)) => title,
            _ => self.title.window.to_string(),
        };
        let title = Self::filter_title_for_report(&title);
        // OSC l title ST
        let response = format!("\x1b]l{title}\x1b\\");
        self.send_response(response_cap, response.as_bytes());
    }

    /// Invoke the window callback if set, returning the response if any.
    ///
    /// # Capability gate (CF-008)
    ///
    /// The `_cap: &WindowOpsCapability` argument is a zero-sized compile-
    /// time proof that the caller has already discharged the
    /// `allow_window_ops` policy check by minting a capability through
    /// [`super::window_auth::WindowMintAuthority::try_mint`]. Because the
    /// capability type's constructor is `pub(super)` and its seal field
    /// is private, no PTY-origin byte and no external crate can produce
    /// a capability — so reaching this function structurally implies
    /// the host authorized window operations at the dispatch frame.
    ///
    /// The capability is consumed by reference (not ownership) so a
    /// single capability can gate multiple invocations within one
    /// XTWINOPS dispatch (e.g. maximize + report round-trip) without
    /// re-minting.
    ///
    /// The underscore prefix silences the unused-variable lint: the
    /// capability has no runtime behavior — its only contribution is
    /// the type signature itself.
    pub(super) fn invoke_window_callback(
        &mut self,
        op: WindowOperation,
        _cap: &WindowOpsCapability,
    ) -> Option<WindowResponse> {
        if let Some(callback) = self.window_callback {
            callback(op)
        } else {
            None
        }
    }

    /// Push current title(s) onto the title stack.
    ///
    /// Uses `Arc<str>` cloning which is just a refcount increment - no allocation.
    fn push_title(&mut self, icon: bool, window: bool) {
        if self.title.stack.len() >= TITLE_STACK_MAX_DEPTH {
            // Stack is full, don't push more (prevents memory exhaustion)
            return;
        }
        // Store the titles to push. Arc::clone is just a refcount increment,
        // so this shares the same allocation as the current title/icon_name.
        let icon_title: Arc<str> = if icon {
            Arc::clone(&self.title.icon)
        } else {
            Arc::from("")
        };
        let window_title: Arc<str> = if window {
            Arc::clone(&self.title.window)
        } else {
            Arc::from("")
        };
        self.title.stack.push((icon_title, window_title));
    }

    /// Pop title(s) from the title stack and restore them.
    ///
    /// Re-caps at [`super::MAX_TITLE_BYTES`] for defense-in-depth, in case
    /// the stack was loaded via `set_title_stack()` with uncapped entries.
    fn pop_title(&mut self, icon: bool, window: bool) {
        if let Some((icon_title, window_title)) = self.title.stack.pop() {
            if icon && !icon_title.is_empty() {
                let b = icon_title.floor_char_boundary(super::MAX_TITLE_BYTES);
                self.title.icon = if b < icon_title.len() {
                    Arc::from(&icon_title[..b])
                } else {
                    icon_title
                };
            }
            if window && !window_title.is_empty() {
                let b = window_title.floor_char_boundary(super::MAX_TITLE_BYTES);
                let capped: Arc<str> = if b < window_title.len() {
                    Arc::from(&window_title[..b])
                } else {
                    window_title
                };
                if let Some(ref mut callback) = self.title.callback {
                    callback(&capped);
                }
                // Bump the title-change epoch only on a real value change (mirrors
                // set_title / the OSC 0/2 handler) so a host polling `title_epoch()`
                // sees the pop restore the tab title rather than showing a stale one.
                if *self.title.window != *capped {
                    self.title
                        .epoch
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                self.title.window = capped;
            }
            // Fire v3 event callback for popped titles, matching set_title behavior.
            if let Some(ref mut callback) = self.title.event_callback {
                let title_type = match (icon, window) {
                    (true, true) => aterm_types::TitleType::WindowAndIcon,
                    (true, false) => aterm_types::TitleType::IconOnly,
                    (false, true) => aterm_types::TitleType::WindowOnly,
                    (false, false) => return,
                };
                let text = match title_type {
                    aterm_types::TitleType::WindowOnly | aterm_types::TitleType::WindowAndIcon => {
                        &*self.title.window
                    }
                    _ => &*self.title.icon,
                };
                callback(title_type, text);
            }
        }
    }

    /// Filter a title string for safe reporting.
    ///
    /// Removes escape sequences and control characters to prevent
    /// title spoofing/injection attacks.
    fn filter_title_for_report(title: &str) -> String {
        title.chars().filter(|c| !c.is_control()).collect()
    }
}

#[cfg(test)]
mod tests {
    //! Regression tests for the `allow_window_ops` deny match in
    //! `handle_window_state_or_geometry`.
    //!
    //! These tests exercise the pure terminal handler without a PTY — they
    //! feed CSI sequences directly to `Terminal::process` and inspect
    //! `take_response()` for leaked PTY replies.

    use crate::terminal::Terminal;
    use aterm_policy::engine::PolicyEngine;
    use aterm_policy::{
        Defaults, OriginTag, Policy, Profile, Response, Rule, SCHEMA_VERSION, profiles,
    };

    fn window_policy(sequence: &str, response: Response) -> Policy {
        Policy {
            schema_version: SCHEMA_VERSION,
            profile: Profile::Standard,
            defaults: Defaults {
                unmatched: Response::Drop,
                shell_integration_require_nonce: false,
            },
            rules: vec![Rule {
                sequence: sequence.to_owned(),
                origin_min: OriginTag::Pty,
                response,
                rate_limit: None,
                prompt_id: None,
            }],
            rate_limits: vec![],
        }
    }

    /// CSI 21 t (report window title) MUST NOT emit any PTY response when
    /// `allow_window_ops=false`. Regression: CSI 20/21 previously fell through
    /// the deny match and leaked the title to untrusted PTY output (#7876).
    #[test]
    fn csi_21t_title_report_suppressed_when_window_ops_disabled() {
        let mut term = Terminal::new(24, 80);
        // Default is false, but set explicitly to document the invariant.
        term.modes_mut().allow_window_ops = false;
        term.set_title("secret-title");

        term.process(b"\x1b[21t");

        assert!(
            term.take_response().is_none(),
            "CSI 21 t must not leak the window title when allow_window_ops is false (#7876)"
        );
    }

    /// CSI 20 t (report icon label) MUST NOT emit any PTY response when
    /// `allow_window_ops=false`. Regression from #7876.
    #[test]
    fn csi_20t_icon_label_report_suppressed_when_window_ops_disabled() {
        let mut term = Terminal::new(24, 80);
        term.modes_mut().allow_window_ops = false;
        term.set_title("secret-title");

        term.process(b"\x1b[20t");

        assert!(
            term.take_response().is_none(),
            "CSI 20 t must not leak the icon label when allow_window_ops is false (#7876)"
        );
    }

    /// Positive case: CSI 21 t DOES emit a response when
    /// `allow_window_ops=true`. Guards against over-broad denial that would
    /// also break title reporting for hosts that opt into window ops.
    #[test]
    fn csi_21t_title_report_allowed_when_window_ops_enabled() {
        let mut term = Terminal::new(24, 80);
        term.modes_mut().allow_window_ops = true;
        term.set_title("allowed-title");

        term.process(b"\x1b[21t");

        let response = term
            .take_response()
            .expect("CSI 21 t should emit a response when allow_window_ops is true");
        // Response format: OSC l <title> ST  (ESC ] l title ESC \)
        let as_str = std::str::from_utf8(&response).expect("response is valid UTF-8");
        assert!(
            as_str.contains("allowed-title"),
            "response should carry the current title; got {as_str:?}"
        );
        assert!(
            as_str.starts_with("\x1b]l"),
            "response should be an OSC l title report; got {as_str:?}"
        );
    }

    /// CSI 22 t (title stack push) MUST still be processed when
    /// `allow_window_ops=false` — it only mutates the internal stack and does
    /// not emit any PTY response. This guards against an over-broad fix to
    /// #7876 that would also block the title stack.
    #[test]
    fn csi_22t_title_push_allowed_when_window_ops_disabled() {
        let mut term = Terminal::new(24, 80);
        term.modes_mut().allow_window_ops = false;
        term.set_title("original-title");

        // Push current title onto the stack.
        term.process(b"\x1b[22t");
        // Change title, then pop — the pop should restore "original-title".
        term.set_title("replaced-title");
        term.process(b"\x1b[23t");

        assert!(
            term.take_response().is_none(),
            "CSI 22 t and CSI 23 t must never emit a PTY response"
        );
        assert_eq!(
            term.title(),
            "original-title",
            "title stack push/pop must still function when allow_window_ops is false"
        );
    }

    /// CSI 23 t (title stack pop) restoring a DIFFERENT window title MUST bump
    /// `title_epoch()`, mirroring set_title / OSC 0/2 — otherwise a host polling
    /// the epoch keeps showing the stale tab title after the pop. Regression:
    /// pop_title's window branch restored the value without bumping the epoch.
    #[test]
    fn csi_23t_title_pop_bumps_epoch_on_window_change() {
        let mut term = Terminal::new(24, 80);
        // OSC 2 set window title "foo".
        term.process(b"\x1b]2;foo\x07");
        let after_foo = term.title_epoch();
        // Push the current title, then set a different one.
        term.process(b"\x1b[22t");
        term.process(b"\x1b]2;bar\x07");
        let after_bar = term.title_epoch();
        assert!(
            after_bar > after_foo,
            "setting a fresh title should have advanced the epoch"
        );
        // Pop: window title goes back to "foo" — a real change, so the epoch bumps.
        term.process(b"\x1b[23t");
        assert_eq!(
            term.title(),
            "foo",
            "CSI 23 t must restore the pushed title"
        );
        assert!(
            term.title_epoch() > after_bar,
            "the pop restored a different title, so title_epoch() must advance \
             (host would otherwise show a stale tab title)"
        );
    }

    #[test]
    fn csi_21t_policy_rule_can_enable_specific_report_without_legacy_bool() {
        let mut term = Terminal::new(24, 80);
        term.modes_mut().allow_window_ops = false;
        term.set_title("policy-title");
        term.apply_policy_engine(PolicyEngine::new(window_policy(
            "CSI 21 t",
            Response::Execute,
        )));

        term.process(b"\x1b[21t");

        let response = term
            .take_response()
            .expect("policy rule should allow CSI 21 t even when legacy bool is false");
        let as_str = std::str::from_utf8(&response).expect("response is valid UTF-8");
        assert!(as_str.contains("policy-title"));
    }

    #[test]
    fn csi_21t_policy_rule_does_not_overgrant_other_xtwinops_reports() {
        let mut term = Terminal::new(24, 80);
        term.modes_mut().allow_window_ops = false;
        term.set_title("policy-title");
        term.apply_policy_engine(PolicyEngine::new(window_policy(
            "CSI 21 t",
            Response::Execute,
        )));

        term.process(b"\x1b[20t");

        assert!(
            term.take_response().is_none(),
            "CSI 21 t policy rule must not accidentally authorize CSI 20 t"
        );
    }

    #[test]
    fn standard_profile_wildcard_does_not_overgrant_xtwinops_manipulation() {
        use std::sync::{Arc, Mutex};

        let mut term = Terminal::new(24, 80);
        term.modes_mut().allow_window_ops = false;
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = Arc::clone(&captured);
        term.set_window_callback(move |op| {
            captured_clone.lock().expect("poisoned").push(op);
            None
        });
        term.apply_policy_engine(PolicyEngine::new(profiles::standard()));

        term.process(b"\x1b[1t");

        assert!(
            captured.lock().expect("poisoned").is_empty(),
            "standard wildcard Execute must not reopen XTWINOPS manipulation"
        );
    }
}
