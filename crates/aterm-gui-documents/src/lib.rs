// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Document models and `amacs` editor re-exports for `aterm-gui`.

/// Re-exports of the `amacs` editor reducer, document store, and rope.
pub use amacs::{editor as native_editor, store as document_store, text as native_text};

/// The Markdown reader's document model.
pub mod native_markdown;
