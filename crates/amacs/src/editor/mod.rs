// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native text-editor interaction core.
//!
//! This is an editor over [`crate::store::DocumentStore`], not a terminal
//! emulator. Commands, key sequences, prefix state, selections, kill/yank state,
//! marks, undo frames, and keyboard macros are structured data. Canonical
//! bytes and commit ordering remain exclusively in the document store.
//!
//! One module per concern, so slices that add commands touch different files:
//!
//! - `command`: the closed command table, M-x completion and the `C-u` argument;
//! - `chord` and `keymap`: key chords and the keymap trie with its built-in tables;
//! - `workspace`: shared state and the reducer's entry points (`execute`,
//!   `handle_chord`, `insert_text`) and their effects and errors;
//! - `view`: one view's selections, viewport and caret motion;
//! - `minibuffer`: the modal queries;
//! - `projection`: the visible-line projection and the line index;
//! - `history`: undo and redo;
//! - `search`: incremental search;
//! - `kill`: the kill ring, kills and yanks;
//! - `macros`: keyboard macros.
//!
//! Every public item is re-exported here, so `amacs::editor::X` (aterm-gui's
//! `native_editor::X`) names it whichever module holds it. A module that
//! gains its first public item adds its line below.

mod chord;
mod command;
mod history;
mod keymap;
mod kill;
mod macros;
mod minibuffer;
mod projection;
mod search;
mod view;
mod workspace;

pub use chord::*;
pub use command::*;
pub use keymap::*;
pub use minibuffer::*;
pub use projection::*;
pub use view::*;
pub use workspace::*;

#[cfg(test)]
use crate::store::{DocumentId, DocumentStore, DocumentViewId};

/// The editor the engine's tests drive: a test store holding `text`, a fresh
/// workspace, and one view attached to the document.
#[cfg(test)]
fn editor(text: &str) -> (DocumentStore, EditorWorkspace, EditorBufferView, DocumentId) {
    let mut store = DocumentStore::for_test();
    let document = store.open("mem://editor".into(), text.into());
    let mut workspace = EditorWorkspace::new();
    let view = workspace
        .attach(&mut store, document, DocumentViewId(91))
        .unwrap();
    (store, workspace, view, document)
}
