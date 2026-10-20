// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! amacs, aterm's editor engine (docs/DESIGN-amacs-2026-10-19.md).
//!
//! The headless reducer, document store and rope behind aterm's native Editor.
//! Editing commands commit through the document store; the workspace owns
//! keymaps, selections, undo, search, kill/yank and keyboard macros.
//!
//! This is an engine crate (docs/ATERM_DESIGN.md §2.7, the census's
//! `engine-platform` rule): its shipped code names no filesystem, socket, raw
//! fd, libc, thread or native link. The native GUI performs I/O and hands the
//! engine values.
//!
//! The editor reducer (`editor`), the document store (`store`) and the rope
//! (`text`) moved here from aterm-gui-documents, which re-exports them under
//! their old names (`native_editor`, `document_store`, `native_text`). The
//! input reducer (`input`) is the one place a face's key, text or paste
//! becomes an editor command; aterm's native Editor drives it today. The
//! `protocol` module has the `edit` request and ticket replies and the
//! file-URI codec, which aterm-gui's `edit` and `await edit` verbs serve. The
//! other modules, the rest of `protocol` and the `amacs-files`/`amacs-term`
//! crates are placeholders for planned config/language/protocol, file I/O and
//! terminal/client work.

pub mod config;
/// The editor's pure reducer: commands, keymaps, views and the workspace.
pub mod editor;
/// The shared input reducer: one key-to-command semantics for every face.
pub mod input;
pub mod language;
pub mod protocol;
/// The store of open documents and their views.
pub mod store;
/// The rope every document's text lives in.
pub mod text;
