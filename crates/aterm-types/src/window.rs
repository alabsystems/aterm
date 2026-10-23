// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Window operations (CSI t - XTWINOPS) for terminal control.

/// Window operation requested by `CSI t` (XTWINOPS) escape sequences.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WindowOperation {
    /// De-iconify (restore from minimized) window.
    DeIconify,
    /// Iconify (minimize) window.
    Iconify,
    /// Move window to pixel position.
    MoveWindow {
        /// X coordinate in pixels.
        x: u16,
        /// Y coordinate in pixels.
        y: u16,
    },
    /// Resize window to pixel dimensions.
    ResizeWindowPixels {
        /// Height in pixels.
        height: u16,
        /// Width in pixels.
        width: u16,
    },
    /// Raise window to front of stacking order.
    RaiseWindow,
    /// Lower window to back of stacking order.
    LowerWindow,
    /// Refresh/redraw window.
    RefreshWindow,
    /// Resize text area to character cell dimensions.
    ResizeWindowCells {
        /// Height in character cells (rows).
        rows: u16,
        /// Width in character cells (columns).
        cols: u16,
    },
    /// Restore maximized window to normal size.
    RestoreMaximized,
    /// Maximize window.
    MaximizeWindow,
    /// Maximize window vertically only.
    MaximizeVertically,
    /// Maximize window horizontally only.
    MaximizeHorizontally,
    /// Exit fullscreen mode.
    UndoFullscreen,
    /// Enter fullscreen mode.
    EnterFullscreen,
    /// Toggle fullscreen mode.
    ToggleFullscreen,
    /// Request report of window state (`CSI 1 t` or `CSI 2 t`).
    ReportWindowState,
    /// Request report of window position in pixels (`CSI 3 ; x ; y t`).
    ReportWindowPosition,
    /// Request report of text area position in pixels (`CSI 3 ; x ; y t`).
    ReportTextAreaPosition,
    /// Request report of text area size in pixels (`CSI 4 ; height ; width t`).
    ReportTextAreaSizePixels,
    /// Request report of window size in pixels (`CSI 4 ; height ; width t`).
    ReportWindowSizePixels,
    /// Request report of screen size in pixels (`CSI 5 ; height ; width t`).
    ReportScreenSizePixels,
    /// Request report of character cell size in pixels (`CSI 6 ; height ; width t`).
    ReportCellSizePixels,
    /// Request report of text area size in character cells (`CSI 8 ; rows ; cols t`).
    ReportTextAreaSizeCells,
    /// Request report of screen size in character cells (`CSI 9 ; rows ; cols t`).
    ReportScreenSizeCells,
    /// Request report of icon label (`OSC L label ST`).
    ReportIconLabel,
    /// Request report of window title (`OSC l title ST`).
    ReportWindowTitle,
    /// Push title(s) onto the stack.
    PushTitle {
        /// Push icon label to stack.
        icon: bool,
        /// Push window title to stack.
        window: bool,
    },
    /// Pop title(s) from the stack.
    PopTitle {
        /// Pop icon label from stack.
        icon: bool,
        /// Pop window title from stack.
        window: bool,
    },
}

/// Response from a window operation query.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WindowResponse {
    /// Window state: false = not iconified, true = iconified.
    WindowState(bool),
    /// Position in pixels (x, y).
    Position {
        /// X coordinate.
        x: u16,
        /// Y coordinate.
        y: u16,
    },
    /// Size in pixels (height, width).
    SizePixels {
        /// Height in pixels.
        height: u16,
        /// Width in pixels.
        width: u16,
    },
    /// Size in character cells (rows, cols).
    SizeCells {
        /// Rows (height).
        rows: u16,
        /// Columns (width).
        cols: u16,
    },
    /// Cell size in pixels (height, width).
    CellSize {
        /// Cell height in pixels.
        height: u16,
        /// Cell width in pixels.
        width: u16,
    },
    /// Title string (for icon label or window title).
    Title(String),
}
