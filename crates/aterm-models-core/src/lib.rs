// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// Registers the `trust` tool namespace under verify builds only, so `#[trust::…]` attributes
// compile everywhere and mean something under `targo trust build`.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm's models of the terminal core, the keeper and the selection.
//!
//! Each `*_model()` is a bounded state machine written once with `aterm_spec::ty_model!` (or a
//! hand-built `aterm_spec::derive::Model` under `#[aterm_spec::model]`); writing it registers it.
//! The engine (the macro, the interpreter, `verify`, `xref`) is `aterm-spec`; `aterm-models`
//! links every domain crate for `spec-check` and the registry-wide sweeps. Only the crates whose
//! tests drive these models dev-depend on this one, so editing a model here rebuilds this crate
//! and those tests, never a library.

mod models_alt_screen;
mod models_core;
mod models_keeper;
mod models_kernels;
mod models_resize_row_reuse;
mod models_run_loop_waker;
mod models_sandbox;
mod models_screen_generation;
mod models_text_selection_lifecycle;

pub use models_alt_screen::alt_screen_model;
pub use models_core::*;
pub use models_keeper::{
    keeper_companion_custody_model, keeper_death_judgement_model, keeper_relaunch_brake_model,
    pty_keeper_custody_model,
};
pub use models_kernels::{ascii_grapheme_fast_path_model, rle_local_compact_model};
pub use models_resize_row_reuse::resize_row_reuse_model;
pub use models_run_loop_waker::run_loop_waker_model;
pub use models_sandbox::sandbox_model;
pub use models_screen_generation::observation_screen_generation_model;
pub use models_text_selection_lifecycle::text_selection_lifecycle_model;
