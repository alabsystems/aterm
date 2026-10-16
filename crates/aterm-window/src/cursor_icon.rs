// Copyright (c) 2023 Kirill Chibisov
// SPDX-License-Identifier: Apache-2.0
//
// Derived from cursor-icon 1.2.0 src/lib.rs (licensed MIT OR Apache-2.0 OR Zlib; this copy
// takes the Apache-2.0 arm), modified by the aterm project in 2026 (see the repository NOTICE).

//! The named cursor icons.
//!
//! LOCAL PATCH (aterm), 2026-10-15 (docs/AUDIT-third-party-2026-10-15.md, winit-core step 5):
//! upstream winit re-exports `cursor_icon::CursorIcon`, 35 icons; this crate defines the six aterm
//! sets, so the package left the macOS and Windows graphs. Linux still links it, through
//! smithay-client-toolkit and its decoration frame: the Wayland backend converts at that boundary
//! (`platform_impl::linux::wayland::types::cursor::sctk_icon`). The variant docs are aterm's own;
//! the names are the CSS `cursor` keywords and the fallbacks are cursor-icon's table, as
//! `tests/recorded_answers.rs` records.

/// A named cursor icon.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, Hash)]
pub enum CursorIcon {
    /// The platform's default cursor, usually an arrow.
    #[default]
    Default,
    /// A pointing hand: the thing under the cursor is a link.
    Pointer,
    /// An I-beam: text that can be selected.
    Text,
    /// A closed hand: something is being dragged.
    Grabbing,
    /// A boundary between columns that can be dragged sideways.
    ColResize,
    /// A boundary between rows that can be dragged up or down.
    RowResize,
}

impl CursorIcon {
    /// The icon's CSS `cursor` keyword, which is also its name in a freedesktop cursor theme.
    pub fn name(&self) -> &'static str {
        match self {
            CursorIcon::Default => "default",
            CursorIcon::Pointer => "pointer",
            CursorIcon::Text => "text",
            CursorIcon::Grabbing => "grabbing",
            CursorIcon::ColResize => "col-resize",
            CursorIcon::RowResize => "row-resize",
        }
    }

    /// Names the icon has in legacy Xcursor themes, to try in order when a theme has no icon
    /// under [`name`](Self::name).
    pub fn alt_names(&self) -> &'static [&'static str] {
        match self {
            CursorIcon::Default => &["left_ptr", "arrow", "top_left_arrow", "left_arrow"],
            CursorIcon::Pointer => &["hand2", "hand1", "hand", "pointing_hand"],
            CursorIcon::Text => &["xterm", "ibeam"],
            CursorIcon::Grabbing => &["closedhand"],
            CursorIcon::ColResize => &["split_h", "h_double_arrow", "sb_h_double_arrow"],
            CursorIcon::RowResize => &["split_v", "v_double_arrow", "sb_v_double_arrow"],
        }
    }
}
