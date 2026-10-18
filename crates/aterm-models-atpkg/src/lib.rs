// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// Registers the `trust` tool namespace under verify builds only, so `#[trust::…]` attributes
// compile everywhere and mean something under `targo trust build`.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm's models of the package manager.
//!
//! Each `*_model()` is a bounded state machine written once with `aterm_spec::ty_model!` (or a
//! hand-built `aterm_spec::derive::Model` under `#[aterm_spec::model]`); writing it registers it.
//! The engine (the macro, the interpreter, `verify`, `xref`) is `aterm-spec`; `aterm-models`
//! links every domain crate for `spec-check` and the registry-wide sweeps. Only the crates whose
//! tests drive these models dev-depend on this one, so editing a model here rebuilds this crate
//! and those tests, never a library.

mod models_atpkg_contention_release;
mod models_atpkg_flip_quiet;
mod models_atpkg_full_pass;
mod models_atpkg_head_watch_hosts;
mod models_atpkg_index_probe;
mod models_atpkg_index_publish;
mod models_atpkg_pass_stamps;
mod models_atpkg_pending_wait;
mod models_atpkg_published_spacing;
mod models_atpkg_session_index;
mod models_atpkg_session_index_retry;
mod models_atpkg_vendor_pending;

pub use models_atpkg_contention_release::atpkg_contention_release_park_model;
pub use models_atpkg_flip_quiet::atpkg_flip_quiet_model;
pub use models_atpkg_full_pass::atpkg_full_pass_rule_model;
pub use models_atpkg_head_watch_hosts::atpkg_head_watch_hosts_model;
pub use models_atpkg_index_probe::{
    atpkg_index_pending_park_model, atpkg_index_probe_completion_cadence_model,
    atpkg_index_probe_cooldown_model, atpkg_index_shared_handoff_model,
    atpkg_index_successor_selection_model, atpkg_index_wake_highwater_model,
};
pub use models_atpkg_index_publish::atpkg_index_publish_walk_model;
pub use models_atpkg_pass_stamps::atpkg_pass_stamps_model;
pub use models_atpkg_pending_wait::atpkg_pending_wait_model;
pub use models_atpkg_published_spacing::atpkg_published_spacing_model;
pub use models_atpkg_session_index::{
    atpkg_session_index_eligibility_model, atpkg_session_index_handoff_model,
};
pub use models_atpkg_session_index_retry::atpkg_session_index_retry_model;
pub use models_atpkg_vendor_pending::atpkg_vendor_pending_check_model;
