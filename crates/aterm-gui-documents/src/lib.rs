// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-gui's document models, extracted below the window crate:
//! nothing here reaches `App`, a window or the event loop, so a one-line edit
//! in aterm-gui recompiles none of it. aterm-gui re-exports each module under
//! its old name (`crate::native_editor`, `crate::document_store`, …).
//!
//! The viewport and projection probes a test of aterm-gui itself builds
//! through are compiled in every debug build (`cfg(any(test,
//! debug_assertions))`), never into a release one. A store with a test
//! authority is this crate's own tests' alone (`DocumentStore::for_test`); a
//! test of aterm-gui opens its documents in a store aterm-gui mints
//! (`test_document_store`, under the `launcher-mint` guard).

/// The store of open documents and their views.
pub mod document_store;
/// The native editor's pure reducer.
pub mod native_editor;
/// The Markdown reader's document model.
pub mod native_markdown;
/// The rope every document's text lives in.
pub mod native_text;
