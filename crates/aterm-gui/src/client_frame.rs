// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's own frame where the window system draws none
//! (docs/AUDIT-third-party-2026-10-15.md, LW.5).
//!
//! On Wayland a compositor that does not grant server-side decorations (GNOME's
//! Mutter, weston: no xdg-decoration, or client mode chosen) leaves the frame to
//! the client. The winit fork used to draw sctk-adwaita's (a headerbar above the
//! surface and resize borders around it, as subsurfaces, with four subprocess
//! spawns at window creation); it draws none now, and the chrome band is the
//! header instead:
//!
//! - its right end carries the window buttons ([`CaptionButton`], laid out as
//!   [`crate::tab_bar::TabHit::Caption`] segments of a [`CaptionSlot`]);
//! - its bare band drags the window, double-clicks to maximize and restore, and
//!   right-clicks to the compositor's window menu (`handle_tab_strip_click` and
//!   the right-press arm of `on_mouse_input`);
//! - the window's own edges resize it ([`edge_at`]): a press there calls
//!   `drag_resize_window`, and the pointer shows the edge's resize cursor. The
//!   handles stay in the padding around the terminal grid ([`FrameMargins`]), so
//!   no cell's pixel is ever one.
//!
//! The band is there whatever `tab_strip_rows` says: a window that draws its own
//! frame needs a header, so the strip keeps one row while one does
//! ([`strip_rows_with_header`]). A one-tab strip paints the window's title, so
//! that row is the title, the buttons and the caption sctk-adwaita drew. In
//! fullscreen there is no header ([`ClientFrame::header`]), as there was none
//! under sctk-adwaita.
//!
//! [`ClientFrame`] is what the fork reports (`client_decorations`), read on every
//! `Resized`; `None` everywhere the window system decorates (X11, server-side
//! Wayland, macOS, Windows), where none of this runs.

use winit::window::CursorIcon;

/// One of the header's window buttons, laid out left to right as
/// [`ClientFrame::buttons`] lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CaptionButton {
    /// Minimize the window.
    Minimize,
    /// Maximize the window (shown while it is not maximized).
    Maximize,
    /// Restore a maximized window (in the maximize button's place).
    Restore,
    /// Close the window, as the window system's close would (`CloseRequested`).
    Close,
}

/// What a window's own frame offers, from the fork's
/// `WindowExtWayland::client_decorations`: present only while the window must
/// draw its decorations itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ClientFrame {
    /// The compositor can minimize the window.
    pub(crate) minimize: bool,
    /// The compositor can maximize and restore the window.
    pub(crate) maximize: bool,
    /// The compositor shows a window menu.
    pub(crate) window_menu: bool,
    /// The window is maximized (the middle button restores).
    pub(crate) maximized: bool,
    /// The edges resize the window: it is resizable and neither maximized,
    /// fullscreen nor tiled.
    pub(crate) edges: bool,
    /// The band is the window's header: in every state but fullscreen. A
    /// fullscreen window shows no window buttons and its bare band is no
    /// caption (no move, no maximize, no window menu), as sctk-adwaita hid its
    /// header there.
    pub(crate) header: bool,
}

impl ClientFrame {
    /// The frame window `window` must draw itself, or `None` where the window
    /// system decorates it.
    #[cfg(target_os = "linux")]
    pub(crate) fn of_window(window: &winit::window::Window) -> Option<Self> {
        use winit::platform::wayland::WindowExtWayland as _;
        window.client_decorations().map(|d| Self {
            minimize: d.minimize,
            maximize: d.maximize,
            window_menu: d.window_menu,
            maximized: d.maximized,
            edges: d.resizable && !d.maximized && !d.fullscreen && !d.tiled,
            header: !d.fullscreen,
        })
    }

    /// Whether the band is this frame's header ([`Self::header`]); a `fn` for
    /// `Option::is_some_and`.
    pub(crate) fn has_header(self) -> bool {
        self.header
    }

    /// The header's buttons, left to right: minimize, maximize (restore while
    /// maximized), close, each only where the compositor offers it. Close is
    /// always there while there is a header; in fullscreen there are none. This
    /// is the order and the defaults sctk-adwaita used when it could not read
    /// GNOME's `button-layout` (which it read by spawning `dbus-send`; aterm does
    /// not read it).
    pub(crate) fn buttons(self) -> impl Iterator<Item = CaptionButton> {
        let maximize = if self.maximized {
            CaptionButton::Restore
        } else {
            CaptionButton::Maximize
        };
        [
            self.minimize.then_some(CaptionButton::Minimize),
            self.maximize.then_some(maximize),
            Some(CaptionButton::Close),
        ]
        .into_iter()
        .flatten()
        .filter(move |_| self.header)
    }
}

/// The strip rows the windows get for the `configured` `tab_strip_rows`: one at
/// least while a window draws its own frame and needs its header (`header`, any
/// window's [`ClientFrame::has_header`]), since the strip is that header; exactly
/// `configured` everywhere else. The strip is global, and every window of one
/// compositor is decorated the same way.
#[must_use]
pub(crate) fn strip_rows_with_header(configured: u16, header: bool) -> u16 {
    if header {
        configured.max(1)
    } else {
        configured
    }
}

/// One button's slot in logical px: Adwaita's 24 px round button and 10 px
/// between neighbours.
const BUTTON_SLOT_LOGICAL_PX: f32 = 34.0;

/// The round button's diameter in logical px (Adwaita's, and sctk-adwaita's).
pub(crate) const BUTTON_LOGICAL_PX: f32 = 24.0;

/// The header's buttons as the strip lays them out at its right end: which
/// buttons, left to right, and the whole cells each one's slot takes. A strip
/// cache-key term, so a change of buttons, of the maximized state or of the
/// slot width repaints the band.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct CaptionSlot {
    buttons: [Option<CaptionButton>; 3],
    button_cols: u16,
}

impl CaptionSlot {
    /// No buttons: the window system decorates this window.
    pub(crate) const NONE: Self = Self {
        buttons: [None; 3],
        button_cols: 0,
    };

    /// The slot for `frame` on a strip of `cell_w`-px cells at `scale`: each
    /// button takes [`BUTTON_SLOT_LOGICAL_PX`] rounded up to whole cells.
    /// [`Self::NONE`] without a frame, a header or a cell width.
    pub(crate) fn measure(frame: Option<ClientFrame>, cell_w: usize, scale: f32) -> Self {
        let Some(frame) = frame.filter(|frame| frame.header && cell_w > 0) else {
            return Self::NONE;
        };
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let cols = ((BUTTON_SLOT_LOGICAL_PX * scale).round() / cell_w as f32).ceil();
        let button_cols = if cols.is_finite() && cols >= 1.0 {
            cols.min(f32::from(u16::MAX)) as u16
        } else {
            1
        };
        let mut buttons = [None; 3];
        for (slot, button) in buttons.iter_mut().zip(frame.buttons()) {
            *slot = Some(button);
        }
        Self {
            buttons,
            button_cols,
        }
    }

    /// The buttons, left to right.
    pub(crate) fn buttons(self) -> impl Iterator<Item = CaptionButton> {
        self.buttons.into_iter().flatten()
    }

    /// The cells one button's slot takes.
    pub(crate) fn button_cols(self) -> u16 {
        self.button_cols
    }

    /// The cells the whole slot takes.
    pub(crate) fn cols(self) -> u16 {
        let n = u16::try_from(self.buttons().count()).unwrap_or(u16::MAX);
        n.saturating_mul(self.button_cols)
    }

    /// The slot on a `cols`-wide strip that must keep `floor` cells left of it
    /// for the tabs and the `+`: minimize goes first, then maximize; close stays
    /// while it fits at all, and on a strip too narrow even for it there are no
    /// buttons (the window's edges and the compositor's own keys still work).
    #[must_use]
    pub(crate) fn fit(self, cols: u16, floor: u16) -> Self {
        let mut slot = self;
        for drop in [
            CaptionButton::Minimize,
            CaptionButton::Maximize,
            CaptionButton::Restore,
        ] {
            if slot.cols().saturating_add(floor) <= cols {
                return slot;
            }
            for b in &mut slot.buttons {
                if *b == Some(drop) {
                    *b = None;
                }
            }
        }
        if slot.cols() < cols { slot } else { Self::NONE }
    }
}

/// One of the frame's eight resize handles: an edge or a corner. Its own type, not
/// winit's `ResizeDirection`, which the macOS backend does not have; the frame's
/// geometry is tested on every host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum FrameEdge {
    North,
    NorthEast,
    East,
    SouthEast,
    South,
    SouthWest,
    West,
    NorthWest,
}

impl FrameEdge {
    /// The handle as the direction `drag_resize_window` takes.
    #[cfg(target_os = "linux")]
    pub(crate) fn direction(self) -> winit::window::ResizeDirection {
        use winit::window::ResizeDirection as D;
        match self {
            Self::North => D::North,
            Self::NorthEast => D::NorthEast,
            Self::East => D::East,
            Self::SouthEast => D::SouthEast,
            Self::South => D::South,
            Self::SouthWest => D::SouthWest,
            Self::West => D::West,
            Self::NorthWest => D::NorthWest,
        }
    }

    /// The cursor the handle shows: GTK's (and sctk-adwaita's) directional resize
    /// icons.
    pub(crate) fn cursor(self) -> CursorIcon {
        match self {
            Self::North => CursorIcon::NResize,
            Self::NorthEast => CursorIcon::NeResize,
            Self::East => CursorIcon::EResize,
            Self::SouthEast => CursorIcon::SeResize,
            Self::South => CursorIcon::SResize,
            Self::SouthWest => CursorIcon::SwResize,
            Self::West => CursorIcon::WResize,
            Self::NorthWest => CursorIcon::NwResize,
        }
    }
}

/// How deep the left, right and bottom resize handles reach into the window at
/// most, in logical px: half the 12 px default padding. Never past the padding
/// ([`FrameMargins`]).
const SIDE_EDGE_LOGICAL_PX: f64 = 6.0;

/// How deep the top resize handle reaches at most: above the tab cards (which
/// start about 3.5 logical px down), so a press on a card still selects its tab.
const TOP_EDGE_LOGICAL_PX: f64 = 3.0;

/// How far a corner's handle runs along each of its two edges, in logical px.
const CORNER_LOGICAL_PX: f64 = 16.0;

/// How much of the window lies outside the terminal grid along each edge, in
/// window px: the padding (and on the right and bottom the residue of a size
/// that is not whole cells), and at the top the band and the strip above the
/// grid. Each edge's handle takes at most this much ([`edge_at`]), so a press on
/// a cell, even in the first or last column or the last row at
/// `window_padding = 0`, is the grid's (a selection, a program's mouse report)
/// and never a resize. The handles sit inside the surface (the frame has no
/// shadow margin of its own), so at no padding a side has no handle; the
/// compositor's own resize (a modifier drag, the window menu) still works.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct FrameMargins {
    pub(crate) left: f64,
    pub(crate) top: f64,
    pub(crate) right: f64,
    pub(crate) bottom: f64,
}

impl FrameMargins {
    /// The margins of a `width`×`height` window whose terminal grid covers
    /// `grid_x0..grid_x1` × `grid_y0..grid_y1`, all in window px. A grid that
    /// overflows the window (a transient crop) leaves that side none.
    #[must_use]
    pub(crate) fn around_grid(
        width: f64,
        height: f64,
        grid_x0: f64,
        grid_y0: f64,
        grid_x1: f64,
        grid_y1: f64,
    ) -> Self {
        let room = |px: f64| if px.is_finite() { px.max(0.0) } else { 0.0 };
        Self {
            left: room(grid_x0),
            top: room(grid_y0),
            right: room(width - grid_x1),
            bottom: room(height - grid_y1),
        }
    }
}

/// The resize handle under window pixel `(x, y)` of a `width`×`height` window at
/// `scale`, or `None` inside the frame's interior. The handles sit inside the
/// surface (the frame has no shadow margin of its own): a strip along each edge,
/// and an L along each pair of edges for each corner, each no deeper than that
/// edge's `margins` (so never over the grid).
#[must_use]
pub(crate) fn edge_at(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    scale: f64,
    margins: FrameMargins,
) -> Option<FrameEdge> {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    if !(x.is_finite() && y.is_finite()) || x < 0.0 || y < 0.0 || x >= width || y >= height {
        return None;
    }
    let depth =
        |nominal: f64, margin: f64| nominal.min(if margin.is_finite() { margin } else { 0.0 });
    let left = depth(SIDE_EDGE_LOGICAL_PX * scale, margins.left);
    let right = depth(SIDE_EDGE_LOGICAL_PX * scale, margins.right);
    let top = depth(TOP_EDGE_LOGICAL_PX * scale, margins.top);
    let bottom = depth(SIDE_EDGE_LOGICAL_PX * scale, margins.bottom);
    let corner = CORNER_LOGICAL_PX * scale;
    let (from_right, from_bottom) = (width - x, height - y);
    // The grid's first cell starts at `left` (`top`) and its last ends `right`
    // (`bottom`) before the far edge, as the pointer's cell mapping floors it, so
    // these hold only off the grid, and a zero-deep edge takes nothing.
    let on_left = x < left;
    let on_right = from_right <= right;
    let on_top = y < top;
    let on_bottom = from_bottom <= bottom;
    let near_left = x < corner;
    let near_right = from_right <= corner;
    let near_top = y < corner;
    let near_bottom = from_bottom <= corner;
    let edge = if (on_top && near_left) || (on_left && near_top) {
        FrameEdge::NorthWest
    } else if (on_top && near_right) || (on_right && near_top) {
        FrameEdge::NorthEast
    } else if (on_bottom && near_left) || (on_left && near_bottom) {
        FrameEdge::SouthWest
    } else if (on_bottom && near_right) || (on_right && near_bottom) {
        FrameEdge::SouthEast
    } else if on_top {
        FrameEdge::North
    } else if on_bottom {
        FrameEdge::South
    } else if on_left {
        FrameEdge::West
    } else if on_right {
        FrameEdge::East
    } else {
        return None;
    };
    Some(edge)
}

/// How far the pointer must travel from a bare-band press before it becomes a
/// window move, in logical px. A press that only clicks (or double-clicks)
/// starts no compositor grab, so the second press of a double-click reaches the
/// window; sctk-adwaita likewise moved only on the first motion.
pub(crate) const MOVE_THRESHOLD_LOGICAL_PX: f64 = 3.0;

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: ClientFrame = ClientFrame {
        minimize: true,
        maximize: true,
        window_menu: true,
        maximized: false,
        edges: true,
        header: true,
    };

    #[test]
    fn buttons_follow_the_compositor_and_the_maximized_state() {
        let all: Vec<_> = FRAME.buttons().collect();
        assert_eq!(
            all,
            [
                CaptionButton::Minimize,
                CaptionButton::Maximize,
                CaptionButton::Close
            ]
        );
        let maximized = ClientFrame {
            maximized: true,
            ..FRAME
        };
        assert_eq!(
            maximized.buttons().nth(1),
            Some(CaptionButton::Restore),
            "a maximized window's middle button restores"
        );
        let bare = ClientFrame {
            minimize: false,
            maximize: false,
            ..FRAME
        };
        assert_eq!(bare.buttons().collect::<Vec<_>>(), [CaptionButton::Close]);
        // Fullscreen: no header, so not even close.
        let fullscreen = ClientFrame {
            edges: false,
            header: false,
            ..FRAME
        };
        assert_eq!(fullscreen.buttons().count(), 0);
        assert_eq!(
            CaptionSlot::measure(Some(fullscreen), 9, 1.0),
            CaptionSlot::NONE
        );
    }

    #[test]
    fn a_window_that_draws_its_own_frame_keeps_a_header_row() {
        // `tab_strip_rows = 0` still gets the header's row…
        assert_eq!(strip_rows_with_header(0, true), 1);
        // …and a strip that has one is left as it is.
        assert_eq!(strip_rows_with_header(1, true), 1);
        assert_eq!(strip_rows_with_header(3, true), 3);
        // NEGATIVE CONTROL: where the window system decorates (or in fullscreen),
        // the configured rows, none included.
        assert_eq!(strip_rows_with_header(0, false), 0);
        assert_eq!(strip_rows_with_header(2, false), 2);
    }

    #[test]
    fn a_slot_takes_whole_cells_at_every_scale() {
        // 34 logical px: 4 cells of 9 px at scale 1, 4 of 14 px at 1.5 (51 px), 4 of
        // 18 px at 2 (68 px), 2 of 20 px.
        for (cell_w, scale, cols) in [(9, 1.0, 4), (14, 1.5, 4), (18, 2.0, 4), (20, 1.0, 2)] {
            let slot = CaptionSlot::measure(Some(FRAME), cell_w, scale);
            assert_eq!(slot.button_cols(), cols, "cell {cell_w} px at {scale}");
            assert!(
                usize::from(slot.button_cols()) * cell_w
                    >= (BUTTON_SLOT_LOGICAL_PX * scale).round() as usize,
                "the slot holds the button and its spacing"
            );
            assert_eq!(slot.cols(), 3 * cols);
        }
        assert_eq!(CaptionSlot::measure(None, 9, 1.0), CaptionSlot::NONE);
        assert_eq!(CaptionSlot::measure(Some(FRAME), 0, 1.0), CaptionSlot::NONE);
        assert_eq!(CaptionSlot::NONE.cols(), 0);
    }

    #[test]
    fn a_narrow_strip_sheds_minimize_then_maximize_and_keeps_close() {
        let slot = CaptionSlot::measure(Some(FRAME), 9, 1.0);
        assert_eq!(slot.fit(80, 6), slot, "room for all three");
        let fit = slot.fit(14, 6);
        assert_eq!(
            fit.buttons().collect::<Vec<_>>(),
            [CaptionButton::Maximize, CaptionButton::Close]
        );
        let fit = slot.fit(10, 6);
        assert_eq!(fit.buttons().collect::<Vec<_>>(), [CaptionButton::Close]);
        // Close alone, even past the floor, while it fits at all…
        assert_eq!(
            slot.fit(5, 6).buttons().collect::<Vec<_>>(),
            [CaptionButton::Close]
        );
        // …and nothing on a strip it would fill.
        assert_eq!(slot.fit(4, 6), CaptionSlot::NONE);
    }

    /// The margins of the default 744×528 test window: a 12 px pad around an
    /// 80×24 grid of 9×21 px cells, the strip and its band above it.
    const PADDED: FrameMargins = FrameMargins {
        left: 12.0,
        top: 23.0,
        right: 12.0,
        bottom: 12.0,
    };

    #[test]
    fn edges_and_corners_resize_and_the_interior_does_not() {
        let (w, h) = (744.0, 528.0);
        let at = |x, y| edge_at(x, y, w, h, 1.0, PADDED);
        assert_eq!(at(0.0, 300.0), Some(FrameEdge::West));
        assert_eq!(at(743.0, 300.0), Some(FrameEdge::East));
        assert_eq!(at(300.0, 0.0), Some(FrameEdge::North));
        assert_eq!(at(300.0, 527.0), Some(FrameEdge::South));
        assert_eq!(at(1.0, 1.0), Some(FrameEdge::NorthWest));
        // A corner runs along both of its edges.
        assert_eq!(at(12.0, 1.0), Some(FrameEdge::NorthWest));
        assert_eq!(at(1.0, 12.0), Some(FrameEdge::NorthWest));
        assert_eq!(at(740.0, 520.0), Some(FrameEdge::SouthEast));
        assert_eq!(at(735.0, 2.0), Some(FrameEdge::NorthEast));
        assert_eq!(at(3.0, 520.0), Some(FrameEdge::SouthWest));
        // The interior, the first grid column (12 px in) and a tab card's top.
        assert_eq!(at(300.0, 300.0), None);
        assert_eq!(at(12.0, 300.0), None);
        assert_eq!(at(300.0, 4.0), None);
        // Outside the window, and nonsense, is nothing.
        assert_eq!(at(-1.0, 300.0), None);
        assert_eq!(at(744.0, 300.0), None);
        assert_eq!(at(f64::NAN, 300.0), None);
    }

    #[test]
    fn no_handle_reaches_past_the_padding_into_the_grid() {
        // `window_padding = 0`: a 720×504 grid fills the window's sides and
        // bottom exactly, and the strip still sits above it.
        let (w, h) = (720.0, 525.0);
        let flush = FrameMargins::around_grid(w, h, 0.0, 21.0, 720.0, 525.0);
        assert_eq!(
            flush,
            FrameMargins {
                left: 0.0,
                top: 21.0,
                right: 0.0,
                bottom: 0.0,
            }
        );
        let at = |x, y| edge_at(x, y, w, h, 1.0, flush);
        // Column 0, the last column and the last row are the grid's.
        assert_eq!(at(0.0, 300.0), None);
        assert_eq!(at(719.5, 300.0), None);
        assert_eq!(at(300.0, 524.5), None);
        assert_eq!(at(0.0, 524.0), None, "the bottom-left cell");
        // The top still resizes: above the strip, not over the grid.
        assert_eq!(at(300.0, 1.0), Some(FrameEdge::North));
        assert_eq!(at(0.0, 1.0), Some(FrameEdge::NorthWest));
        // A 2 px pad gives a 2 px handle, never the nominal 6.
        let thin = FrameMargins::around_grid(724.0, 527.0, 2.0, 21.0, 722.0, 525.0);
        let at = |x, y| edge_at(x, y, 724.0, 527.0, 1.0, thin);
        assert_eq!(at(1.5, 300.0), Some(FrameEdge::West));
        assert_eq!(at(2.0, 300.0), None);
        assert_eq!(at(722.0, 300.0), Some(FrameEdge::East));
        assert_eq!(at(721.5, 300.0), None);
        assert_eq!(at(300.0, 525.0), Some(FrameEdge::South));
        assert_eq!(at(300.0, 524.5), None);
        // NEGATIVE CONTROL: the default padding keeps its full handles.
        assert_eq!(
            edge_at(5.5, 300.0, 744.0, 528.0, 1.0, PADDED),
            Some(FrameEdge::West)
        );
        // A grid that overflows the window (a transient crop) leaves no margin.
        assert_eq!(
            FrameMargins::around_grid(700.0, 500.0, -10.0, 21.0, 710.0, f64::NAN),
            FrameMargins {
                left: 0.0,
                top: 21.0,
                right: 0.0,
                bottom: 0.0,
            }
        );
    }

    #[test]
    fn the_handles_scale_with_the_window() {
        // At 2x a 6 logical px side handle is 12 device px deep.
        let wide = FrameMargins {
            left: 24.0,
            top: 46.0,
            right: 24.0,
            bottom: 24.0,
        };
        assert_eq!(
            edge_at(11.0, 300.0, 1488.0, 1056.0, 2.0, wide),
            Some(FrameEdge::West)
        );
        assert_eq!(edge_at(13.0, 300.0, 1488.0, 1056.0, 2.0, wide), None);
        assert_eq!(edge_at(11.0, 300.0, 1488.0, 1056.0, 1.0, wide), None);
    }

    #[test]
    fn each_handle_shows_its_own_cursor() {
        let edges = [
            FrameEdge::North,
            FrameEdge::NorthEast,
            FrameEdge::East,
            FrameEdge::SouthEast,
            FrameEdge::South,
            FrameEdge::SouthWest,
            FrameEdge::West,
            FrameEdge::NorthWest,
        ];
        let icons: std::collections::HashSet<_> =
            edges.into_iter().map(FrameEdge::cursor).collect();
        assert_eq!(icons.len(), edges.len());
        assert_eq!(FrameEdge::North.cursor(), CursorIcon::NResize);
        assert_eq!(FrameEdge::SouthWest.cursor(), CursorIcon::SwResize);
    }
}
