// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! XTSMGRAPHICS (`CSI ? Pi ; Pa ; Pv S`), the graphics-capability query image
//! tools use to discover sixel limits
//! (<https://invisible-island.net/xterm/ctlseqs/ctlseqs.txt>).
//!
//! - `Pi` item: 1 color registers, 2 sixel geometry, 3 ReGIS geometry
//!   (unsupported).
//! - `Pa` action: 1 read, 4 read the maximum; 2 reset and 3 set fail
//!   (read-only).
//!
//! The reply is `CSI ? Pi ; Ps ; Pv S`: `Ps` 0 success, 1 bad Pi, 2 bad Pa, 3
//! failure; `Pv` the count (Pi 1) or `width ; height` (Pi 2).

use super::handler::TerminalHandler;

use crate::sixel::{MAX_COLOR_REGISTERS, SIXEL_MAX_DIMENSION};

/// The largest sixel geometry XTSMGRAPHICS advertises on either axis: xterm's
/// default `maxGraphicSize` (1000x1000), and an image the DCS path places
/// whatever the encoder. Larger ones are dropped, not clipped: the decoder
/// charges 4 bytes per pixel of its raster stride (which a column-at-a-time
/// paint grows to the next power of two) against the DCS budget, beside the
/// stream's own bytes, and refuses more than `SIXEL_MAX_PIXELS`. So the per-axis
/// [`SIXEL_MAX_DIMENSION`] (4096) is a decoder clamp, not a size that places.
const SIXEL_ADVERTISED_MAX: u16 = 1000;

// The advertised square's raster fits in half the DCS budget, leaving the other
// half for the stream's bytes, and under the decoder's per-axis clamp.
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
                // Read answers the current text area in pixels, not the maximum
                // (xterm; lsix and img2sixel size output from it). That is CSI
                // 14 t's fact, so it takes the same window-ops capability and
                // answer; without one, or before a host reports a cell box, it
                // answers the maximum. Either way capped at what places.
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
        assert_eq!(geometry_read(&mut term), "\x1b[?2;0;1000;1000S");

        // Negative control: the same host is consulted once window ops are on.
        term.modes_mut().allow_window_ops = true;
        assert_eq!(geometry_read(&mut term), "\x1b[?2;0;222;111S");
    }

    /// Every geometry XTSMGRAPHICS advertises is one the DCS path places, whatever
    /// the encoder: a solid image of the advertised maximum as one run per band
    /// under a raster declaration, painted a column at a time with none, and as
    /// sixteen colour passes per band.
    #[test]
    fn the_advertised_sixel_maximum_is_placed() {
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?2;4S");
        let reply = String::from_utf8(term.take_response().expect("an answer")).unwrap();
        let dims: Vec<usize> = reply
            .trim_start_matches("\x1b[?2;0;")
            .trim_end_matches('S')
            .split(';')
            .map(|n| n.parse().expect("a dimension"))
            .collect();
        let (w, h) = (dims[0], dims[1]);
        let bands = h / 6;
        let rle = format!(
            "\"1;1;{w};{h}#1;2;100;0;0#1{}",
            format!("!{w}~-").repeat(bands)
        );
        let columns = format!(
            "#1;2;100;0;0#1{}",
            format!("{}-", "~".repeat(w)).repeat(bands)
        );
        let pass = |c: usize| format!("#{c}{}$", "~".repeat(w));
        let passes = (0..bands)
            .map(|_| (1..=16).map(pass).collect::<String>() + "-")
            .collect::<String>();
        for (name, body) in [("rle", rle), ("columns", columns), ("passes", passes)] {
            let mut term = Terminal::new(24, 80);
            term.process(format!("\x1bP0;1;0q{body}\x1b\\").as_bytes());
            assert!(
                !term.images_row(0).is_empty(),
                "{name}: the advertised {w}x{h} placed nothing"
            );
        }
    }

    /// Window ops alone, before any host has reported a cell box, still
    /// answers the maximum rather than a guess.
    #[test]
    fn sixel_geometry_read_falls_back_without_a_cell_box() {
        let mut term = Terminal::new(24, 80);
        term.modes_mut().allow_window_ops = true;
        assert_eq!(geometry_read(&mut term), "\x1b[?2;0;1000;1000S");
    }
}
