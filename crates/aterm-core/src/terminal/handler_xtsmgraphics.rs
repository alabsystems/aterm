// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! XTSMGRAPHICS (`CSI ? Pi ; Pa ; Pv S`) graphics-capability query handler.

use super::handler::TerminalHandler;

use crate::sixel::{MAX_COLOR_REGISTERS, SIXEL_MAX_DIMENSION};

/// Maximum sixel dimension advertised by XTSMGRAPHICS (`1000x1000`).
const SIXEL_ADVERTISED_MAX: u16 = 1000;

const _: () = {
    let side = SIXEL_ADVERTISED_MAX as usize;
    assert!(4 * side.next_power_of_two() * side <= crate::terminal::MAX_DCS_GLOBAL_BUDGET / 2);
    assert!(side <= SIXEL_MAX_DIMENSION);
};

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
    #[inline]
    const fn from_u16(value: u16) -> Option<Self> {
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
    #[inline]
    const fn from_u16(value: u16) -> Option<Self> {
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
    /// Handle XTSMGRAPHICS `CSI ? Pi ; Pa ; Pv S`.
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
            (
                XtsmgraphicsItem::ColorRegisters,
                XtsmgraphicsAction::Read | XtsmgraphicsAction::ReadMax,
            ) => {
                let count = u16::try_from(MAX_COLOR_REGISTERS).unwrap_or(u16::MAX);
                self.send_xtsmgraphics_response(cap, pi, XtsmgraphicsStatus::Success, &[count]);
            }

            (XtsmgraphicsItem::SixelGeometry, XtsmgraphicsAction::Read) => {
                let max_dim = SIXEL_ADVERTISED_MAX;
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
                let dim = SIXEL_ADVERTISED_MAX;
                self.send_xtsmgraphics_response(cap, pi, XtsmgraphicsStatus::Success, &[dim, dim]);
            }

            (XtsmgraphicsItem::RegisGeometry, _) => {
                self.send_xtsmgraphics_response(cap, pi, XtsmgraphicsStatus::Failure, &[0]);
            }

            (_, XtsmgraphicsAction::Set | XtsmgraphicsAction::Reset) => {
                self.send_xtsmgraphics_response(cap, pi, XtsmgraphicsStatus::Failure, &[0]);
            }
        }
    }

    /// Send XTSMGRAPHICS response `CSI ? Pi ; Ps ; Pv... S`.
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
#[path = "handler_xtsmgraphics_tests.rs"]
mod tests;
