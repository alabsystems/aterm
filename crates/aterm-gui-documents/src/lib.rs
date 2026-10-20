// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-gui's document models, extracted below the window crate:
//! nothing here reaches `App`, a window or the event loop, so a one-line edit
//! in aterm-gui recompiles none of it. aterm-gui re-exports each module under
//! its old name (`crate::native_editor`, `crate::document_store`, …).
//!
//! The editor engine (its reducer, the document store and the rope) lives in
//! `amacs` (docs/DESIGN-amacs-2026-10-19.md §3); this crate re-exports it
//! under the module names aterm-gui has always used, so aterm-gui needs no
//! edit. The Markdown reader's model stays here.
//!
//! The viewport and projection probes a test of aterm-gui itself builds
//! through are compiled in every debug build (`cfg(any(test,
//! debug_assertions))`), never into a release one. A store with a test
//! authority is amacs's own tests' alone (`DocumentStore::for_test`); a
//! test of aterm-gui opens its documents in a store aterm-gui mints
//! (`test_document_store`, under the `launcher-mint` guard).

/// The store of open documents and their views, the native editor's pure
/// reducer, and the rope every document's text lives in: amacs's engine.
pub use amacs::{editor as native_editor, store as document_store, text as native_text};

/// The Markdown reader's document model.
pub mod native_markdown;
