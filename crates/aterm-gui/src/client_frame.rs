// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's own frame where the window system draws none (docs/AUDIT-third-party-2026-10-15.md, LW.5).

use aterm_window::window::CursorIcon;

/// One of the header's window buttons, laid out left to right as [`ClientFrame::buttons`] lists them.
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

/// A button GNOME's `button-layout` names: sctk-adwaita's `ButtonKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum LayoutButton {
    Minimize,
    Maximize,
    Close,
}

/// One end of the header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Side {
    Left,
    Right,
}

/// Where the header's buttons go, each end's left to right: GNOME's `org.gnome.desktop.wm.preferences button-layout` (`appmenu:close`, `close,minimize,maximize:`) as sctk-adwaita 0.10.1 read it before LW.5 (tests/recorded/sctk-adwaita-0.10.1-frame.tsv is its answer for each value, and [`Self::parse`] is held to it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ButtonLayout {
    left: [Option<LayoutButton>; 3],
    right: [Option<LayoutButton>; 3],
}

impl ButtonLayout {
    /// The desktop has no answer, or one that names no button: minimize, maximize and close on the right, sctk-adwaita's default.
    pub(crate) const FALLBACK: Self = Self {
        left: [None; 3],
        right: [
            Some(LayoutButton::Minimize),
            Some(LayoutButton::Maximize),
            Some(LayoutButton::Close),
        ],
    };

    /// The two ends `value` names, as sctk-adwaita split it: the text after the value's last space (it read `dbus-send`'s line, whose last word is the value, so a value with spaces was read from its last one), then the first two of its `:`-separated sides.
    pub(crate) fn sides_of(value: &str) -> Option<(&str, &str)> {
        let word = value.rsplit(' ').next().unwrap_or(value);
        let mut sides = word.split(':');
        Some((sides.next()?, sides.next()?))
    }

    /// `value` as sctk-adwaita read it ([`Self::sides_of`]): on each end the first three comma-separated names, of which `close`, `maximize` and `minimize` (exactly; `appmenu`, `spacer` and any other name are skipped) are buttons, in the order written.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        let (left, right) = Self::sides_of(value)?;
        let side = |names: &str| {
            let mut out = [None; 3];
            let named = names.split(',').take(3).filter_map(|name| match name {
                "close" => Some(LayoutButton::Close),
                "maximize" => Some(LayoutButton::Maximize),
                "minimize" => Some(LayoutButton::Minimize),
                _ => None,
            });
            for (slot, button) in out.iter_mut().zip(named) {
                *slot = Some(button);
            }
            out
        };
        let layout = Self {
            left: side(left),
            right: side(right),
        };
        (layout.left.iter().chain(&layout.right).any(Option::is_some)).then_some(layout)
    }

    /// The names on `side`, left to right.
    pub(crate) fn on(self, side: Side) -> impl Iterator<Item = LayoutButton> {
        match side {
            Side::Left => self.left,
            Side::Right => self.right,
        }
        .into_iter()
        .flatten()
    }

    /// The desktop's layout: the Settings portal's `button-layout`, read once a process (sctk-adwaita read it once a frame) and kept; [`Self::FALLBACK`] without a running portal, an answer that is not a string, or one that names no button.
    #[cfg(target_os = "linux")]
    pub(crate) fn desktop() -> Self {
        static READ: std::sync::OnceLock<ButtonLayout> = std::sync::OnceLock::new();
        *READ.get_or_init(|| {
            use crate::portal_settings::{BUTTON_LAYOUT, GNOME_WM_PREFERENCES, read};
            match read(GNOME_WM_PREFERENCES, BUTTON_LAYOUT, LAYOUT_READ_BOUND) {
                Some(aterm_dbus::Value::Str(value)) => {
                    Self::parse(&value).unwrap_or(Self::FALLBACK)
                }
                _ => Self::FALLBACK,
            }
        })
    }
}

/// How long the `button-layout` read may take, the bus's handshake included:
/// it runs once, on the main thread, at the first client-decorated window's
/// first `Resized`; past it the desktop has no answer
/// ([`ButtonLayout::FALLBACK`]).
#[cfg(target_os = "linux")]
const LAYOUT_READ_BOUND: std::time::Duration = std::time::Duration::from_millis(200);

/// What a window's own frame offers, from the fork's `WindowExtWayland::client_decorations`: present only while the window must draw its decorations itself.
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
    /// The edges resize the window: it is resizable and neither maximized, fullscreen nor tiled.
    pub(crate) edges: bool,
    /// The band is the window's header: in every state but fullscreen.
    pub(crate) header: bool,
    /// Where the desktop puts the buttons ([`ButtonLayout::desktop`]).
    pub(crate) layout: ButtonLayout,
}

impl ClientFrame {
    /// The frame window `window` must draw itself, or `None` where the window system decorates it.
    #[cfg(target_os = "linux")]
    pub(crate) fn of_window(window: &aterm_window::window::Window) -> Option<Self> {
        use aterm_window::platform::wayland::WindowExtWayland as _;
        window.client_decorations().map(|d| Self {
            minimize: d.minimize,
            maximize: d.maximize,
            window_menu: d.window_menu,
            maximized: d.maximized,
            edges: d.resizable && !d.maximized && !d.fullscreen && !d.tiled,
            header: !d.fullscreen,
            layout: ButtonLayout::desktop(),
        })
    }

    /// Whether the band is this frame's header ([`Self::header`]); a `fn` for `Option::is_some_and`.
    pub(crate) const fn has_header(self) -> bool {
        self.header
    }

    /// The header's buttons at its `side`, left to right: the ones the layout names there that the compositor offers (minimize and maximize only where it can; close always), maximize as restore while the window is maximized.
    pub(crate) fn buttons_on(self, side: Side) -> impl Iterator<Item = CaptionButton> {
        self.layout
            .on(side)
            .filter_map(move |button| match button {
                LayoutButton::Minimize => self.minimize.then_some(CaptionButton::Minimize),
                LayoutButton::Maximize if self.maximized => {
                    self.maximize.then_some(CaptionButton::Restore)
                }
                LayoutButton::Maximize => self.maximize.then_some(CaptionButton::Maximize),
                LayoutButton::Close => Some(CaptionButton::Close),
            })
            .filter(move |_| self.header)
    }

    /// Every button, the left end's then the right end's (the tests' view; drawing and hit-testing go end by end).
    #[cfg(test)]
    pub(crate) fn buttons(self) -> impl Iterator<Item = CaptionButton> {
        self.buttons_on(Side::Left)
            .chain(self.buttons_on(Side::Right))
    }
}

/// The strip rows the windows get for the `configured` `tab_strip_rows`: one at least while a window draws its own frame and needs its header (`header`, any window's [`ClientFrame::has_header`]), since the strip is that header; exactly `configured` everywhere else.
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

/// The header's buttons as the strip lays them out at its two ends: which buttons, each end's left to right, and the whole cells each one's slot takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct CaptionSlot {
    left: [Option<CaptionButton>; 3],
    right: [Option<CaptionButton>; 3],
    button_cols: u16,
}

impl CaptionSlot {
    /// No buttons: the window system decorates this window.
    pub(crate) const NONE: Self = Self {
        left: [None; 3],
        right: [None; 3],
        button_cols: 0,
    };

    /// The slot for `frame` on a strip of `cell_w`-px cells at `scale`: each button takes [`BUTTON_SLOT_LOGICAL_PX`] rounded up to whole cells.
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
        let side = |side| {
            let mut out = [None; 3];
            for (slot, button) in out.iter_mut().zip(frame.buttons_on(side)) {
                *slot = Some(button);
            }
            out
        };
        Self {
            left: side(Side::Left),
            right: side(Side::Right),
            button_cols,
        }
    }

    /// The buttons at `side`, left to right.
    pub(crate) fn on(self, side: Side) -> impl Iterator<Item = CaptionButton> {
        match side {
            Side::Left => self.left,
            Side::Right => self.right,
        }
        .into_iter()
        .flatten()
    }

    /// Every button, the left end's then the right end's (the tests' view; drawing and hit-testing go end by end).
    #[cfg(test)]
    pub(crate) fn buttons(self) -> impl Iterator<Item = CaptionButton> {
        self.on(Side::Left).chain(self.on(Side::Right))
    }

    /// The cells one button's slot takes.
    pub(crate) const fn button_cols(self) -> u16 {
        self.button_cols
    }

    /// The cells the buttons at `side` take.
    pub(crate) fn side_cols(self, side: Side) -> u16 {
        let n = u16::try_from(self.on(side).count()).unwrap_or(u16::MAX);
        n.saturating_mul(self.button_cols)
    }

    /// The cells both ends take.
    pub(crate) fn cols(self) -> u16 {
        self.side_cols(Side::Left)
            .saturating_add(self.side_cols(Side::Right))
    }

    /// The slot on a `cols`-wide strip that must keep `floor` cells between its ends for the tabs and the `+`: minimize goes first, then maximize, from both ends; close stays while it fits at all, and on a strip too narrow even for it there are no buttons (the window's edges and the compositor's own keys still work).
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
            for b in slot.left.iter_mut().chain(slot.right.iter_mut()) {
                if *b == Some(drop) {
                    *b = None;
                }
            }
            for end in [&mut slot.left, &mut slot.right] {
                let kept: Vec<CaptionButton> = end.iter().flatten().copied().collect();
                *end = [None; 3];
                for (slot, button) in end.iter_mut().zip(kept) {
                    *slot = Some(button);
                }
            }
        }
        if slot.cols() < cols { slot } else { Self::NONE }
    }
}

/// One of the frame's eight resize handles: an edge or a corner.
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
    pub(crate) fn direction(self) -> aterm_window::window::ResizeDirection {
        use aterm_window::window::ResizeDirection as D;
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

    /// The cursor the handle shows: GTK's (and sctk-adwaita's) directional resize icons.
    pub(crate) const fn cursor(self) -> CursorIcon {
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
/// most, in logical px: half the 12 px default padding.
const SIDE_EDGE_LOGICAL_PX: f64 = 6.0;

/// How deep the top resize handle reaches at most: above the tab cards (which
/// start about 3.5 logical px down), so a press on a card still selects its tab.
const TOP_EDGE_LOGICAL_PX: f64 = 3.0;

/// How far a corner's handle runs along each of its two edges, in logical px.
const CORNER_LOGICAL_PX: f64 = 16.0;

/// How much of the window lies outside the terminal grid along each edge, in window px: the padding (and on the right and bottom the residue of a size that is not whole cells), and at the top the band and the strip above the grid.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct FrameMargins {
    pub(crate) left: f64,
    pub(crate) top: f64,
    pub(crate) right: f64,
    pub(crate) bottom: f64,
}

impl FrameMargins {
    /// The margins of a `width`×`height` window whose terminal grid covers `grid_x0..grid_x1` × `grid_y0..grid_y1`, all in window px.
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

/// The resize handle under window pixel `(x, y)` of a `width`×`height` window at `scale`, or `None` inside the frame's interior.
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

/// How far the pointer must travel from a bare-band press before it becomes a window move, in logical px.
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
        layout: ButtonLayout::FALLBACK,
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

    /// sctk-adwaita 0.10.1's own answers (tests/recorder-adwaita): for each
    /// `button-layout` value, or none, the two ends its config read split it
    /// into, and the buttons each end showed under each set of compositor
    /// capabilities.
    const RECORDED: &str = include_str!("../tests/recorded/sctk-adwaita-0.10.1-frame.tsv");

    fn field<'a>(row: &[&'a str], name: &str) -> &'a str {
        row.iter()
            .find_map(|f| f.strip_prefix(name)?.strip_prefix('='))
            .unwrap_or_else(|| panic!("no {name} in {row:?}"))
    }

    fn names(buttons: impl Iterator<Item = CaptionButton>) -> String {
        let names: Vec<&str> = buttons
            .map(|b| match b {
                CaptionButton::Minimize => "minimize",
                CaptionButton::Maximize => "maximize",
                CaptionButton::Restore => "restore",
                CaptionButton::Close => "close",
            })
            .collect();
        if names.is_empty() {
            "-".into()
        } else {
            names.join(",")
        }
    }

    #[test]
    fn the_layout_reads_every_value_as_sctk_adwaita_did() {
        let mut rows = 0;
        for line in RECORDED.lines().filter(|l| l.starts_with("layout\t")) {
            let row: Vec<&str> = line.split('\t').collect();
            let value = match field(&row, "value") {
                "<no portal>" => None,
                quoted => {
                    let v = quoted
                        .strip_prefix('"')
                        .and_then(|v| v.strip_suffix('"'))
                        .expect("a quoted value");
                    assert!(!v.contains('\\'), "no escapes in the corpus: {v}");
                    Some(v)
                }
            };
            let read = value.and_then(ButtonLayout::sides_of);
            let read_s = read.map_or("-".to_string(), |(l, r)| format!("{l}|{r}"));
            assert_eq!(read_s, field(&row, "read"), "{line}");
            let frame = ClientFrame {
                maximize: field(&row, "maximize") == "1",
                minimize: field(&row, "minimize") == "1",
                layout: value
                    .and_then(ButtonLayout::parse)
                    .unwrap_or(ButtonLayout::FALLBACK),
                ..FRAME
            };
            assert_eq!(
                names(frame.buttons_on(Side::Left)),
                field(&row, "left"),
                "{line}"
            );
            assert_eq!(
                names(frame.buttons_on(Side::Right)),
                field(&row, "right"),
                "{line}"
            );
            rows += 1;
        }
        assert_eq!(rows, 92, "every recorded row was read");
    }

    #[test]
    fn the_fallback_is_minimize_maximize_close_on_the_right() {
        let gnome = ButtonLayout::parse("appmenu:close").expect("a layout");
        assert_ne!(gnome, ButtonLayout::FALLBACK);
        let frame = ClientFrame {
            layout: gnome,
            ..FRAME
        };
        assert_eq!(names(frame.buttons()), "close");
        assert_eq!(
            names(FRAME.buttons_on(Side::Right)),
            "minimize,maximize,close"
        );
        assert_eq!(names(FRAME.buttons_on(Side::Left)), "-");
        for nothing in [
            "",
            ":",
            "appmenu:",
            "minimize,maximize,close",
            "Close:Minimize",
        ] {
            assert_eq!(ButtonLayout::parse(nothing), None, "{nothing:?}");
        }
    }

    #[test]
    fn a_slot_lays_out_both_ends_and_sheds_from_both() {
        let layout = ButtonLayout::parse("close,minimize:maximize").expect("a layout");
        let frame = ClientFrame { layout, ..FRAME };
        let slot = CaptionSlot::measure(Some(frame), 9, 1.0);
        assert_eq!(names(slot.on(Side::Left)), "close,minimize");
        assert_eq!(names(slot.on(Side::Right)), "maximize");
        assert_eq!(slot.side_cols(Side::Left), 8);
        assert_eq!(slot.side_cols(Side::Right), 4);
        assert_eq!(slot.cols(), 12);
        let fit = slot.fit(14, 6);
        assert_eq!(names(fit.buttons()), "close,maximize");
        assert_eq!(fit.side_cols(Side::Left), 4);
        let fit = slot.fit(10, 6);
        assert_eq!(names(fit.buttons()), "close");
        assert_eq!(slot.fit(4, 6), CaptionSlot::NONE);
        let maximized = ClientFrame {
            maximized: true,
            ..frame
        };
        assert_eq!(
            names(CaptionSlot::measure(Some(maximized), 9, 1.0).on(Side::Right)),
            "restore"
        );
    }

    #[test]
    fn a_window_that_draws_its_own_frame_keeps_a_header_row() {
        assert_eq!(strip_rows_with_header(0, true), 1);
        assert_eq!(strip_rows_with_header(1, true), 1);
        assert_eq!(strip_rows_with_header(3, true), 3);
        assert_eq!(strip_rows_with_header(0, false), 0);
        assert_eq!(strip_rows_with_header(2, false), 2);
    }

    #[test]
    fn a_slot_takes_whole_cells_at_every_scale() {
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
        assert_eq!(
            slot.fit(5, 6).buttons().collect::<Vec<_>>(),
            [CaptionButton::Close]
        );
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
        assert_eq!(at(12.0, 1.0), Some(FrameEdge::NorthWest));
        assert_eq!(at(1.0, 12.0), Some(FrameEdge::NorthWest));
        assert_eq!(at(740.0, 520.0), Some(FrameEdge::SouthEast));
        assert_eq!(at(735.0, 2.0), Some(FrameEdge::NorthEast));
        assert_eq!(at(3.0, 520.0), Some(FrameEdge::SouthWest));
        assert_eq!(at(300.0, 300.0), None);
        assert_eq!(at(12.0, 300.0), None);
        assert_eq!(at(300.0, 4.0), None);
        assert_eq!(at(-1.0, 300.0), None);
        assert_eq!(at(744.0, 300.0), None);
        assert_eq!(at(f64::NAN, 300.0), None);
    }

    #[test]
    fn no_handle_reaches_past_the_padding_into_the_grid() {
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
        assert_eq!(at(0.0, 300.0), None);
        assert_eq!(at(719.5, 300.0), None);
        assert_eq!(at(300.0, 524.5), None);
        assert_eq!(at(0.0, 524.0), None, "the bottom-left cell");
        assert_eq!(at(300.0, 1.0), Some(FrameEdge::North));
        assert_eq!(at(0.0, 1.0), Some(FrameEdge::NorthWest));
        let thin = FrameMargins::around_grid(724.0, 527.0, 2.0, 21.0, 722.0, 525.0);
        let at = |x, y| edge_at(x, y, 724.0, 527.0, 1.0, thin);
        assert_eq!(at(1.5, 300.0), Some(FrameEdge::West));
        assert_eq!(at(2.0, 300.0), None);
        assert_eq!(at(722.0, 300.0), Some(FrameEdge::East));
        assert_eq!(at(721.5, 300.0), None);
        assert_eq!(at(300.0, 525.0), Some(FrameEdge::South));
        assert_eq!(at(300.0, 524.5), None);
        assert_eq!(
            edge_at(5.5, 300.0, 744.0, 528.0, 1.0, PADDED),
            Some(FrameEdge::West)
        );
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
