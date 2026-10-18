// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// Registers the `trust` tool namespace under verify builds only, so `#[trust::…]` attributes
// compile everywhere and mean something under `targo trust build`.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm's models of the native chrome (tabs, panes, settings, documents).
//!
//! Each `*_model()` is a bounded state machine written once with `aterm_spec::ty_model!` (or a
//! hand-built `aterm_spec::derive::Model` under `#[aterm_spec::model]`); writing it registers it.
//! The engine (the macro, the interpreter, `verify`, `xref`) is `aterm-spec`; `aterm-models`
//! links every domain crate for `spec-check` and the registry-wide sweeps. Only the crates whose
//! tests drive these models dev-depend on this one, so editing a model here rebuilds this crate
//! and those tests, never a library.

mod models_native;

pub use models_native::*;
