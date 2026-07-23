// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! XTSMGRAPHICS - xterm graphics capability query.
//!
//! Implements the `CSI ? Pi ; Pa ; Pv S` control sequence for querying
//! terminal graphics capabilities. This is used by image-capable tools
//! to discover Sixel/ReGIS limits before rendering.
//!
//! Reference: xterm control sequences (Thomas E. Dickey)
//! <https://invisible-island.net/xterm/ctlseqs/ctlseqs.txt>
//!
//! ## Parameters
//!
//! - `Pi` - Item number:
//!   - 1: Color registers (palette size)
//!   - 2: Sixel graphics geometry (width x height in pixels)
//!   - 3: ReGIS graphics geometry (not supported)
//!
//! - `Pa` - Action:
//!   - 1: Read current value
//!   - 2: Reset to default (not supported, read-only)
//!   - 3: Set to value (not supported, read-only)
//!   - 4: Read maximum value
//!
//! - `Pv` - Value (for Pa=3 set operations, ignored in read-only mode)
//!
//! ## Response
//!
//! Response format: `CSI ? Pi ; Ps ; Pv S` where:
//! - `Ps` - Status:
//!   - 0: Success
//!   - 1: Error in Pi
//!   - 2: Error in Pa
//!   - 3: Failure (unsupported action or item)
//!
//! - `Pv` - Result value(s):
//!   - For Pi=1: Single value (color count)
//!   - For Pi=2: Two values (width ; height)

use super::handler::TerminalHandler;

/// Default max color registers when sixel is disabled.
#[cfg(not(feature = "sixel"))]
const MAX_COLOR_REGISTERS: usize = 1024;
/// Default max sixel dimension when sixel is disabled. Must match the value
/// in aterm-sixel (4096) to avoid reporting inconsistent limits (#7263).
#[cfg(not(feature = "sixel"))]
const SIXEL_MAX_DIMENSION: usize = 4096;
#[cfg(feature = "sixel")]
use crate::sixel::{MAX_COLOR_REGISTERS, SIXEL_MAX_DIMENSION};

/// Status codes for XTSMGRAPHICS response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub(super) enum XtsmgraphicsStatus {
    Success = 0,
    ErrorPi = 1,
    ErrorPa = 2,
    Failure = 3,
}

/// Item numbers for XTSMGRAPHICS query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub(super) enum XtsmgraphicsItem {
    ColorRegisters = 1,
    SixelGeometry = 2,
    RegisGeometry = 3,
}

impl XtsmgraphicsItem {
    fn from_u16(value: u16) -> Option<Self> {
        match value {
            1 => Some(Self::ColorRegisters),
            2 => Some(Self::SixelGeometry),
            3 => Some(Self::RegisGeometry),
            _ => None,
        }
    }
}

/// Action types for XTSMGRAPHICS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub(super) enum XtsmgraphicsAction {
    Read = 1,
    Reset = 2,
    Set = 3,
    ReadMax = 4,
}

impl XtsmgraphicsAction {
    fn from_u16(value: u16) -> Option<Self> {
        match value {
            1 => Some(Self::Read),
            2 => Some(Self::Reset),
            3 => Some(Self::Set),
            4 => Some(Self::ReadMax),
            _ => None,
        }
    }
}

impl TerminalHandler<'_> {
    /// Handle XTSMGRAPHICS query: `CSI ? Pi ; Pa ; Pv S`
    ///
    /// This is a read-only implementation that reports terminal graphics
    /// capabilities without allowing modification.
    pub(super) fn handle_xtsmgraphics(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[u16],
    ) {
        let pi = params.first().copied().unwrap_or(0);
        let pa = params.get(1).copied().unwrap_or(0);

        // Parse item and action
        let Some(item) = XtsmgraphicsItem::from_u16(pi) else {
            // Invalid Pi - respond with status 1
            self.send_xtsmgraphics_response(cap, pi, XtsmgraphicsStatus::ErrorPi, &[0]);
            return;
        };

        let Some(action) = XtsmgraphicsAction::from_u16(pa) else {
            // Invalid Pa - respond with status 2
            self.send_xtsmgraphics_response(cap, pi, XtsmgraphicsStatus::ErrorPa, &[0]);
            return;
        };

        match (item, action) {
            // Color registers - read current/max
            (
                XtsmgraphicsItem::ColorRegisters,
                XtsmgraphicsAction::Read | XtsmgraphicsAction::ReadMax,
            ) => {
                // Report MAX_COLOR_REGISTERS for both current and max
                // (current palette size equals max in our implementation)
                let count = u16::try_from(MAX_COLOR_REGISTERS).unwrap_or(u16::MAX);
                self.send_xtsmgraphics_response(cap, pi, XtsmgraphicsStatus::Success, &[count]);
            }

            // Sixel geometry - read current text area pixel size
            (XtsmgraphicsItem::SixelGeometry, XtsmgraphicsAction::Read) => {
                // Per xterm spec, Pa=1 (Read) returns the current text area
                // dimensions in pixels, NOT the maximum. Applications like lsix
                // and img2sixel use this to size output to fit the viewport (#7470).
                //
                // Capability note (CF-008): `invoke_window_callback` now
                // requires a `WindowOpsCapability`. XTSMGRAPHICS historically
                // did not consult `allow_window_ops` — this was an existing
                // privilege conflation not covered by CF-008 (which is
                // scoped to XTWINOPS / `CSI t`). Preserve prior behavior by
                // minting a capability unconditionally here; tightening this
                // to a separate graphics-query policy bit is follow-up work
                // (tracked separately).
                let max_dim = u16::try_from(SIXEL_MAX_DIMENSION).unwrap_or(u16::MAX);
                let window_auth = super::window_auth::WindowMintAuthority::new();
                let window_cap = window_auth
                    .try_mint(true)
                    .expect("WindowMintAuthority::try_mint(true) is infallible");
                if let Some(aterm_types::WindowResponse::SizePixels { height, width }) = self
                    .invoke_window_callback(
                        aterm_types::WindowOperation::ReportTextAreaSizePixels,
                        &window_cap,
                    )
                {
                    // Clamp to SIXEL_MAX_DIMENSION — the absolute renderer limit.
                    let w = width.min(max_dim);
                    let h = height.min(max_dim);
                    self.send_xtsmgraphics_response(cap, pi, XtsmgraphicsStatus::Success, &[w, h]);
                } else {
                    // No window callback available — fall back to max dimension.
                    // This is less correct but avoids returning an error when the
                    // host hasn't registered a window callback.
                    self.send_xtsmgraphics_response(
                        cap,
                        pi,
                        XtsmgraphicsStatus::Success,
                        &[max_dim, max_dim],
                    );
                }
            }

            // Sixel geometry - read maximum
            (XtsmgraphicsItem::SixelGeometry, XtsmgraphicsAction::ReadMax) => {
                let dim = u16::try_from(SIXEL_MAX_DIMENSION).unwrap_or(u16::MAX);
                self.send_xtsmgraphics_response(cap, pi, XtsmgraphicsStatus::Success, &[dim, dim]);
            }

            // ReGIS - not supported
            (XtsmgraphicsItem::RegisGeometry, _) => {
                self.send_xtsmgraphics_response(cap, pi, XtsmgraphicsStatus::Failure, &[0]);
            }

            // Set/Reset operations - read-only, report failure
            (_, XtsmgraphicsAction::Set | XtsmgraphicsAction::Reset) => {
                self.send_xtsmgraphics_response(cap, pi, XtsmgraphicsStatus::Failure, &[0]);
            }
        }
    }

    /// Send XTSMGRAPHICS response: `CSI ? Pi ; Ps ; Pv... S`
    fn send_xtsmgraphics_response(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        pi: u16,
        status: XtsmgraphicsStatus,
        values: &[u16],
    ) {
        use std::fmt::Write;

        let mut response = String::with_capacity(32);
        // CSI ?
        response.push_str("\x1b[?");
        // Pi
        write!(response, "{pi}").ok();
        // ; Ps
        write!(response, ";{}", status as u16).ok();
        // ; Pv... (one or more values)
        for value in values {
            write!(response, ";{value}").ok();
        }
        // S
        response.push('S');

        self.send_response(cap, response.as_bytes());
    }
}
