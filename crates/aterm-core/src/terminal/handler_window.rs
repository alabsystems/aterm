// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! XTWINOPS window manipulation handlers and title stack.

use std::sync::Arc;

use super::TITLE_STACK_MAX_DEPTH;
use super::handler::TerminalHandler;
use super::response_capability::ResponseCapability;
use super::window_auth::{WindowMintAuthority, WindowOpsCapability};
use aterm_types::{WindowOperation, WindowResponse};

/// Decode the title stack sub-parameter: 0/default=both, 1=icon, 2=window.
#[inline]
const fn title_stack_targets(params: &[u16]) -> (bool, bool) {
    match params {
        [_, 1, ..] => (true, false), // Icon only
        [_, 2, ..] => (false, true), // Window only
        _ => (true, true),           // Both (default)
    }
}

impl TerminalHandler<'_> {
    /// Handle XTWINOPS (`CSI Ps ; Ps ; Ps t`) ([`WindowOpsCapability`], [`super::window_auth`]).
    pub(super) fn handle_xtwinops(&mut self, response_cap: &ResponseCapability, params: &[u16]) {
        let ps = params.first().copied().unwrap_or(0);
        let mint_authority = WindowMintAuthority::new();
        let window_cap = mint_authority
            .try_mint_with_engine(self.policy.xtwinops_gate(ps), self.modes.allow_window_ops);

        match window_cap.as_ref() {
            Some(cap) => {
                if self.handle_window_state_or_geometry(ps, params, cap)
                    || self.handle_window_reports(response_cap, ps, params, cap)
                {
                    return;
                }
            }
            None => {
                // nor the PTY.
                if let 1..=21 = ps {
                    return;
                }
            }
        }
        self.handle_window_title_stack(ps, params);
    }

    /// Minimum window size in pixels for resize operations.
    const MIN_RESIZE_PIXELS: u16 = 200;

    /// Minimum window size in cells for resize operations.
    const MIN_RESIZE_CELLS: u16 = 10;

    /// Dispatch XTWINOPS state/geometry subcommands (1-10) ([`WindowOpsCapability`]).
    fn handle_window_state_or_geometry(
        &mut self,
        ps: u16,
        params: &[u16],
        cap: &WindowOpsCapability,
    ) -> bool {
        match ps {
            1 => {
                self.invoke_window_callback(WindowOperation::DeIconify, cap);
            }
            2 => {
                self.invoke_window_callback(WindowOperation::Iconify, cap);
            }

            3 => {}

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

            9 => {
                let sub = params.get(1).copied().unwrap_or(0);
                if let Some(op) = Self::maximize_operation(sub) {
                    self.invoke_window_callback(op, cap);
                }
            }
            10 => {
                let sub = params.get(1).copied().unwrap_or(0);
                if let Some(op) = Self::fullscreen_operation(sub) {
                    self.invoke_window_callback(op, cap);
                }
            }
            _ => return false,
        }
        true
    }

    /// Dispatch XTWINOPS reports (11-21); `response_cap` gates replies.
    fn handle_window_reports(
        &mut self,
        response_cap: &ResponseCapability,
        ps: u16,
        params: &[u16],
        cap: &WindowOpsCapability,
    ) -> bool {
        match ps {
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
    const fn maximize_operation(sub: u16) -> Option<WindowOperation> {
        match sub {
            0 => Some(WindowOperation::RestoreMaximized),
            1 => Some(WindowOperation::MaximizeWindow),
            2 => Some(WindowOperation::MaximizeVertically),
            3 => Some(WindowOperation::MaximizeHorizontally),
            _ => None,
        }
    }

    #[inline]
    const fn fullscreen_operation(sub: u16) -> Option<WindowOperation> {
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
            let response = format!("\x1b[3;{x};{y}t");
            self.send_response(response_cap, response.as_bytes());
        }
    }

    /// CSI 14 t (text area, pixels) and CSI 14 ; 2 t (whole window, pixels).
    fn report_window_size_pixels(
        &mut self,
        response_cap: &ResponseCapability,
        sub: u16,
        cap: &WindowOpsCapability,
    ) {
        let size = if sub == 2 {
            match self.invoke_window_callback(WindowOperation::ReportWindowSizePixels, cap) {
                Some(WindowResponse::SizePixels { height, width }) => {
                    Some((u32::from(height), u32::from(width)))
                }
                _ => None,
            }
        } else {
            self.text_area_pixels(cap)
        };
        if let Some((height, width)) = size {
            let response = format!("\x1b[4;{height};{width}t");
            self.send_response(response_cap, response.as_bytes());
        }
    }

    /// The text area in pixels as `(height, width)`.
    #[inline]
    pub(super) fn text_area_pixels(&mut self, cap: &WindowOpsCapability) -> Option<(u32, u32)> {
        match self.invoke_window_callback(WindowOperation::ReportTextAreaSizePixels, cap) {
            Some(WindowResponse::SizePixels { height, width }) => {
                Some((u32::from(height), u32::from(width)))
            }
            _ => self.text_area_size_pixels(),
        }
    }

    /// The text area in pixels, or `None` while no host has reported a cell box.
    #[inline]
    fn text_area_size_pixels(&self) -> Option<(u32, u32)> {
        let (cell_w, cell_h) = self.iterm2.host_cell_px()?;
        Some((
            u32::from(self.grid.rows()).saturating_mul(u32::from(cell_h)),
            u32::from(self.grid.cols()).saturating_mul(u32::from(cell_w)),
        ))
    }

    fn report_screen_size_pixels(
        &mut self,
        response_cap: &ResponseCapability,
        cap: &WindowOpsCapability,
    ) {
        if let Some(WindowResponse::SizePixels { height, width }) =
            self.invoke_window_callback(WindowOperation::ReportScreenSizePixels, cap)
        {
            let response = format!("\x1b[5;{height};{width}t");
            self.send_response(response_cap, response.as_bytes());
        }
    }

    /// CSI 16 t: report cell size in pixels ([`super::Terminal::set_cell_pixel_size`]).
    fn report_cell_size_pixels(
        &mut self,
        response_cap: &ResponseCapability,
        cap: &WindowOpsCapability,
    ) {
        let size = match self.invoke_window_callback(WindowOperation::ReportCellSizePixels, cap) {
            Some(WindowResponse::CellSize { height, width }) => Some((height, width)),
            _ => self.iterm2.host_cell_px().map(|(w, h)| (h, w)),
        };
        if let Some((height, width)) = size {
            let response = format!("\x1b[6;{height};{width}t");
            self.send_response(response_cap, response.as_bytes());
        }
    }

    fn report_text_area_size_cells(&mut self, response_cap: &ResponseCapability) {
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
        let response = format!("\x1b]l{title}\x1b\\");
        self.send_response(response_cap, response.as_bytes());
    }

    /// Invoke the window callback if set ([`super::window_auth::WindowMintAuthority`]).
    #[inline]
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

    /// Push current title(s) onto the title stack (`Arc<str>` clones, no allocation).
    #[inline]
    fn push_title(&mut self, icon: bool, window: bool) {
        if self.title.stack.len() >= TITLE_STACK_MAX_DEPTH {
            return;
        }
        let icon_title = icon.then(|| Arc::clone(&self.title.icon));
        let window_title = window.then(|| Arc::clone(&self.title.window));
        self.title.stack.push((icon_title, window_title));
    }

    /// Pop title(s) from the title stack and restore them, re-capped at [`super::MAX_TITLE_BYTES`].
    fn pop_title(&mut self, icon: bool, window: bool) {
        if let Some((icon_title, window_title)) = self.title.stack.pop() {
            let older = |slot: fn(&super::grouped_state::TitleStackEntry) -> &Option<Arc<str>>| {
                self.title.stack.iter().rev().find_map(|e| slot(e).clone())
            };
            let icon_title = icon_title.or_else(|| older(|e| &e.0));
            let window_title = window_title.or_else(|| older(|e| &e.1));
            if icon && let Some(icon_title) = icon_title {
                let b = icon_title.floor_char_boundary(super::MAX_TITLE_BYTES);
                self.title.icon = if b < icon_title.len() {
                    Arc::from(&icon_title[..b])
                } else {
                    icon_title
                };
            }
            if window && let Some(window_title) = window_title {
                let b = window_title.floor_char_boundary(super::MAX_TITLE_BYTES);
                let capped: Arc<str> = if b < window_title.len() {
                    Arc::from(&window_title[..b])
                } else {
                    window_title
                };
                if *self.title.window != *capped {
                    self.title
                        .epoch
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                self.title.window = capped;
            }
        }
    }

    /// Filter a title for reporting: strip control characters against title injection.
    #[inline]
    fn filter_title_for_report(title: &str) -> String {
        title.chars().filter(|c| !c.is_control()).collect()
    }
}

#[cfg(test)]
mod tests {

    //! Tests for the `allow_window_ops` gate in XTWINOPS, feeding CSI sequences
    //! to `Terminal::process` and inspecting `take_response()`.
    use crate::terminal::Terminal;
    use aterm_policy::engine::PolicyEngine;
    use aterm_policy::{OriginTag, Policy, Response, profiles};

    fn window_policy(sequence: &str, response: Response) -> Policy {
        crate::terminal::policy_bridge::one_rule_policy(
            sequence,
            response,
            OriginTag::Pty,
            Response::Drop,
        )
    }

    /// CSI 21 t / CSI 20 t (report window title / icon label) emit no response
    /// when `allow_window_ops=false`.
    #[test]
    fn csi_21t_title_report_suppressed_when_window_ops_disabled() {
        for query in [b"\x1b[21t".as_slice(), b"\x1b[20t".as_slice()] {
            let mut term = Terminal::new(24, 80);
            term.modes_mut().allow_window_ops = false;
            term.set_title("secret-title");

            term.process(query);

            assert!(
                term.take_response().is_none(),
                "{query:?} must not leak the title when allow_window_ops is false"
            );
        }
    }

    /// CSI 21 t does respond when `allow_window_ops=true`.
    #[test]
    fn csi_21t_title_report_allowed_when_window_ops_enabled() {
        let mut term = Terminal::new(24, 80);
        term.modes_mut().allow_window_ops = true;
        term.set_title("allowed-title");

        term.process(b"\x1b[21t");

        let response = term
            .take_response()
            .expect("CSI 21 t should emit a response when allow_window_ops is true");
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

    /// CSI 22 t (title stack push) is still processed when
    /// `allow_window_ops=false`: it only mutates the internal stack.
    #[test]
    fn csi_22t_title_push_allowed_when_window_ops_disabled() {
        let mut term = Terminal::new(24, 80);
        term.modes_mut().allow_window_ops = false;
        term.set_title("original-title");

        term.process(b"\x1b[22t");
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

    /// CSI 23 t (title stack pop) restoring a different window title bumps
    /// `title_epoch()`, as set_title and OSC 0/2 do.
    #[test]
    fn a_title_pushed_and_popped_slot_by_slot_comes_back() {
        for (before, vim) in [
            (
                "orig",
                &b"\x1b[22;2t\x1b[22;1t\x1b]2;file.txt - VIM\x07\x1b[23;2t\x1b[23;1t"[..],
            ),
            (
                "orig",
                b"\x1b[22;2t\x1b[22;1t\x1b]2;file.txt - VIM\x07\x1b]2;Thanks for flying Vim\x07\
                  \x1b[23;2t\x1b[23;1t\x1b[22;2t\x1b[22;1t\x1b[23;2t\x1b[23;1t",
            ),
            ("", b"\x1b[22t\x1b]2;prog\x07\x1b[23t"),
        ] {
            let mut term = Terminal::new(24, 80);
            term.process(format!("\x1b]2;{before}\x07").as_bytes());
            term.process(vim);
            assert_eq!(term.title(), before, "{}", String::from_utf8_lossy(vim));
        }
    }

    #[test]
    fn csi_23t_title_pop_bumps_epoch_on_window_change() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]2;foo\x07");
        let after_foo = term.title_epoch();
        term.process(b"\x1b[22t");
        term.process(b"\x1b]2;bar\x07");
        let after_bar = term.title_epoch();
        assert!(
            after_bar > after_foo,
            "setting a fresh title should have advanced the epoch"
        );
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

    /// A window with `allow_window_ops` alone — no window callback, exactly
    /// the shape of a real GUI session, whose callback declines reports.
    fn windowed_term(rows: u16, cols: u16) -> Terminal {
        let mut term = Terminal::new(rows, cols);
        term.modes_mut().allow_window_ops = true;
        term
    }

    fn response_string(term: &mut Terminal) -> Option<String> {
        term.take_response()
            .map(|r| String::from_utf8(r).expect("XTWINOPS reports are ASCII"))
    }

    /// CSI 16 t reports the host's cell box, height first (`CSI 6 ; h ; w t`).
    #[test]
    fn csi_16t_reports_the_host_reported_cell_box() {
        let mut term = windowed_term(24, 80);
        term.set_cell_pixel_size(9, 19);

        term.process(b"\x1b[16t");

        assert_eq!(
            response_string(&mut term).as_deref(),
            Some("\x1b[6;19;9t"),
            "CSI 16 t must answer the real cell box, height then width"
        );
    }

    /// The text area tracks the grid, not the size at construction — a report
    /// after a resize describes the terminal the application is looking at.
    #[test]
    fn csi_14t_follows_the_grid_across_a_resize() {
        let mut term = windowed_term(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.resize(30, 100);

        term.process(b"\x1b[14t");

        assert_eq!(
            response_string(&mut term).as_deref(),
            Some("\x1b[4;600;1000t")
        );
    }

    /// No fabrication: a terminal no host has measured answers nothing, never
    /// the 8x16 placeholder (`CSI 6 ; 16 ; 8 t`).
    #[test]
    fn the_pixel_reports_stay_silent_when_no_host_has_measured_a_cell() {
        for query in [b"\x1b[14t".as_slice(), b"\x1b[16t".as_slice()] {
            let mut term = windowed_term(24, 80);
            term.process(query);
            assert_eq!(
                response_string(&mut term),
                None,
                "{query:?} must not pass the 8x16 placeholder off as a measurement"
            );
        }
        let mut term = windowed_term(24, 80);
        term.set_cell_pixel_size(8, 16);
        term.process(b"\x1b[16t");
        assert_eq!(response_string(&mut term).as_deref(), Some("\x1b[6;16;8t"));
    }

    /// A zero axis is not a cell box any font could have, and reporting one
    /// invites a divide-by-zero in the application.
    #[test]
    fn a_zero_axis_cell_box_is_not_a_measurement() {
        let mut term = windowed_term(24, 80);
        term.set_cell_pixel_size(0, 19);
        term.process(b"\x1b[16t");
        assert_eq!(response_string(&mut term), None);
    }

    /// Reports the engine cannot answer stay silent even with a measured cell
    /// box and window ops authorized: `14 ; 2 t` whole-window pixels, `15 t`
    /// screen pixels, `19 t` screen cells, `11 t` iconified state, `13 t`
    /// window position.
    #[test]
    fn reports_with_no_in_core_truth_stay_silent_rather_than_guess() {
        for query in [
            b"\x1b[14;2t".as_slice(),
            b"\x1b[15t".as_slice(),
            b"\x1b[19t".as_slice(),
            b"\x1b[11t".as_slice(),
            b"\x1b[13t".as_slice(),
            b"\x1b[13;2t".as_slice(),
        ] {
            let mut term = windowed_term(24, 80);
            term.set_cell_pixel_size(9, 19);
            term.process(query);
            assert_eq!(
                response_string(&mut term),
                None,
                "{query:?} has no in-core truth; silence is the honest answer"
            );
        }
    }

    /// The in-core fallbacks live inside the capability gate: an unauthorized host
    /// still leaks no font metrics to the PTY.
    #[test]
    fn the_pixel_reports_are_still_gated_by_allow_window_ops() {
        for query in [b"\x1b[14t".as_slice(), b"\x1b[16t".as_slice()] {
            let mut term = Terminal::new(24, 80);
            term.modes_mut().allow_window_ops = false;
            term.set_cell_pixel_size(9, 19);
            term.process(query);
            assert_eq!(
                response_string(&mut term),
                None,
                "{query:?} must not answer without window-ops authorization"
            );
        }
    }

    /// A host that can answer synchronously still wins: the callback's value is
    /// used verbatim and the in-core fallback never runs.
    #[test]
    fn a_host_that_answers_a_pixel_report_overrides_the_in_core_fallback() {
        let mut term = windowed_term(24, 80);
        term.set_cell_pixel_size(9, 19);
        term.set_window_callback(|op| match op {
            aterm_types::WindowOperation::ReportCellSizePixels => {
                Some(aterm_types::WindowResponse::CellSize {
                    height: 40,
                    width: 20,
                })
            }
            _ => None,
        });

        term.process(b"\x1b[16t");

        assert_eq!(
            response_string(&mut term).as_deref(),
            Some("\x1b[6;40;20t"),
            "the host's own metrics take precedence over the engine's"
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
