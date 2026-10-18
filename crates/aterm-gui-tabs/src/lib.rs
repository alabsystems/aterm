// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-gui's tab model, extracted below the window crate: the
//! stable ids, the views a tab holds, the pane tree, and which content a
//! window fronts. Nothing here reaches `App`, a window or the event loop, so a
//! one-line edit in aterm-gui recompiles none of it. aterm-gui re-exports each
//! module under its old name.

/// The content a window fronts: a terminal session or a native view.
pub mod front_content;
/// Stable view, tab and app-instance ids, the view store and the pane tree.
pub mod tab_model;
