// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native text-editor interaction core.

mod chord;
mod command;
mod edit;
mod fill;
mod grid;
mod history;
mod keymap;
mod kill;
mod macros;
mod mark;
mod minibuffer;
mod projection;
mod search;
mod view;
mod workspace;

pub use chord::*;
pub use command::*;
pub use grid::*;
pub use history::{
    AMALGAM_CHARS, Origin, RECOVERED_FLOOR, Request, UNDO_BYTE_LIMIT, UNDO_FRAME_LIMIT,
};
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
