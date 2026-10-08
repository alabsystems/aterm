// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! XTSMGRAPHICS (`CSI ? Pi ; Pa ; Pv S`): the graphics-capability query
//! image tools use to discover Sixel/ReGIS limits before rendering.
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
/// in aterm-sixel (4096) to avoid reporting inconsistent limits.
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
    /// Handle XTSMGRAPHICS `CSI ? Pi ; Pa ; Pv S`. Read-only: set and reset
    /// answer Failure.
    pub(super) fn handle_xtsmgraphics(
        &mut self,
        cap: &super::response_capability::ResponseCapability,
        params: &[u16],
    ) {
        let pi = params.first().copied().unwrap_or(0);
        let pa = params.get(1).copied().unwrap_or(0);

        let Some(item) = XtsmgraphicsItem::from_u16(pi) else {
            self.send_xtsmgraphics_response(cap, pi, XtsmgraphicsStatus::ErrorPi, &[0]);
            return;
        };

        let Some(action) = XtsmgraphicsAction::from_u16(pa) else {
            self.send_xtsmgraphics_response(cap, pi, XtsmgraphicsStatus::ErrorPa, &[0]);
            return;
        };

        match (item, action) {
            // The palette is always its maximum size: current == max.
            (
                XtsmgraphicsItem::ColorRegisters,
                XtsmgraphicsAction::Read | XtsmgraphicsAction::ReadMax,
            ) => {
                let count = u16::try_from(MAX_COLOR_REGISTERS).unwrap_or(u16::MAX);
                self.send_xtsmgraphics_response(cap, pi, XtsmgraphicsStatus::Success, &[count]);
            }

            (XtsmgraphicsItem::SixelGeometry, XtsmgraphicsAction::Read) => {
                // Per xterm, Pa=1 (Read) returns the current text area in
                // pixels, not the maximum; lsix and img2sixel size their output
                // from it. That is the fact CSI 14 t reports, so it rides the
                // same window-ops capability (policy engine, then
                // `allow_window_ops`) and the same answer. Without the
                // capability, or before a host has reported a cell box, it
                // answers the maximum dimension.
                let max_dim = u16::try_from(SIXEL_MAX_DIMENSION).unwrap_or(u16::MAX);
                let window_cap = super::window_auth::WindowMintAuthority::new()
                    .try_mint_with_engine(
                        self.policy.xtwinops_gate(14),
                        self.modes.allow_window_ops,
                    );
                let size = window_cap.as_ref().and_then(|wc| self.text_area_pixels(wc));
                let clamp = |px: u32| u16::try_from(px.min(u32::from(max_dim))).unwrap_or(max_dim);
                let (w, h) = size.map_or((max_dim, max_dim), |(height, width)| {
                    (clamp(width), clamp(height))
                });
                self.send_xtsmgraphics_response(cap, pi, XtsmgraphicsStatus::Success, &[w, h]);
            }

            (XtsmgraphicsItem::SixelGeometry, XtsmgraphicsAction::ReadMax) => {
                let dim = u16::try_from(SIXEL_MAX_DIMENSION).unwrap_or(u16::MAX);
                self.send_xtsmgraphics_response(cap, pi, XtsmgraphicsStatus::Success, &[dim, dim]);
            }

            // ReGIS is not supported.
            (XtsmgraphicsItem::RegisGeometry, _) => {
                self.send_xtsmgraphics_response(cap, pi, XtsmgraphicsStatus::Failure, &[0]);
            }

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

        let mut response = format!("\x1b[?{pi};{}", status as u16);
        for value in values {
            write!(response, ";{value}").ok();
        }
        response.push('S');

        self.send_response(cap, response.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use crate::terminal::Terminal;
    use aterm_types::{WindowOperation, WindowResponse};

    fn geometry_read(term: &mut Terminal) -> String {
        term.process(b"\x1b[?2;1S");
        String::from_utf8(term.take_response().expect("XTSMGRAPHICS always answers"))
            .expect("ASCII")
    }

    /// With window ops granted, the sixel-geometry read answers the real text
    /// area (the same in-core product CSI 14 t reports), width first.
    #[test]
    fn sixel_geometry_read_reports_the_text_area_with_window_ops() {
        let mut term = Terminal::new(24, 80);
        term.modes_mut().allow_window_ops = true;
        term.set_cell_pixel_size(9, 19);
        // 80 cols x 9 px = 720 wide; 24 rows x 19 px = 456 tall.
        assert_eq!(geometry_read(&mut term), "\x1b[?2;0;720;456S");
    }

    /// Without window ops the read never reaches the host: a callback that
    /// would answer is not consulted, and the reply is the maximum dimension.
    #[test]
    fn sixel_geometry_read_does_not_reach_the_host_without_window_ops() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(9, 19);
        term.set_window_callback(|op| match op {
            WindowOperation::ReportTextAreaSizePixels => Some(WindowResponse::SizePixels {
                height: 111,
                width: 222,
            }),
            _ => None,
        });
        assert_eq!(geometry_read(&mut term), "\x1b[?2;0;4096;4096S");

        // Negative control: the same host is consulted once window ops are on.
        term.modes_mut().allow_window_ops = true;
        assert_eq!(geometry_read(&mut term), "\x1b[?2;0;222;111S");
    }

    /// Window ops alone, before any host has reported a cell box, still
    /// answers the maximum rather than a guess.
    #[test]
    fn sixel_geometry_read_falls_back_without_a_cell_box() {
        let mut term = Terminal::new(24, 80);
        term.modes_mut().allow_window_ops = true;
        assert_eq!(geometry_read(&mut term), "\x1b[?2;0;4096;4096S");
    }
}
