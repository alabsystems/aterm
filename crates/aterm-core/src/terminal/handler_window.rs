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
                // No capability: drop 1-21 (manipulation, geometry queries, title
                // reports), which change host window state or leak data for
                // fingerprinting. The title stack touches neither the callback
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
            let resp = callback(op);
            if resp.is_some() {
                self.transient.host_io_ordinal = self.transient.host_io_ordinal.wrapping_add(1);
            }
            resp
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
#[path = "handler_window_tests.rs"]
mod tests;
