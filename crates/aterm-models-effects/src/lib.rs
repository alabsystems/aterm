// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// Registers the `trust` tool namespace under verify builds only, so `#[trust::…]` attributes
// compile everywhere and mean something under `targo trust build`.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm's models of effects, cursor fx, ribbons and the console pet.
//!
//! Each `*_model()` is a bounded state machine written once with `aterm_spec::ty_model!` (or a
//! hand-built `aterm_spec::derive::Model` under `#[aterm_spec::model]`); writing it registers it.
//! The engine (the macro, the interpreter, `verify`, `xref`) is `aterm-spec`; `aterm-models`
//! links every domain crate for `spec-check` and the registry-wide sweeps. Only the crates whose
//! tests drive these models dev-depend on this one, so editing a model here rebuilds this crate
//! and those tests, never a library.

mod models_console_life;
mod models_effects;
mod models_fx;
mod models_pet_observation_admission;
mod models_rainbow_composer_newline;
mod models_rainbow_continuity;
mod models_rainbow_short_wrap;
mod models_ribbon_follow;
mod models_ribbon_row_hold;
mod models_same_caret_echo;
mod models_tricks;

pub use models_console_life::*;
pub use models_effects::*;
pub use models_fx::*;
pub use models_pet_observation_admission::*;
pub use models_rainbow_composer_newline::rainbow_composer_newline_gate_model;
pub use models_rainbow_continuity::*;
pub use models_rainbow_short_wrap::rainbow_short_wrap_park_model;
pub use models_ribbon_follow::*;
pub use models_ribbon_row_hold::*;
pub use models_same_caret_echo::*;
pub use models_tricks::{trick_flash_lifecycle_model, trick_listener_purity_model};
