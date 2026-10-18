// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The committed models the engine's own unit tests run on (the ring, the session kernel, the
//! config catalog, …), mounted from their domain crates' sources rather than copied, so a unit
//! test here exercises the model that ships. A model file names only `aterm_spec::…` paths and
//! its own items, which is what lets it compile here as well as in its crate (`aterm_spec` is
//! this crate's own alias). Test builds only: no library carries a model.
#![allow(
    dead_code,
    reason = "a unit test here names a few models of each file; the rest are its crate's"
)]

#[path = "../../aterm-models-core/src/models_core.rs"]
mod models_core;
#[path = "../../aterm-models-gui/src/models_gui.rs"]
mod models_gui;
#[path = "../../aterm-models-native/src/models_native.rs"]
mod models_native;
#[path = "../../aterm-models-session/src/models_session.rs"]
mod models_session;
#[path = "../../aterm-models-update/src/models_update.rs"]
mod models_update;

pub(crate) use models_core::{ring_model, subscribe_model};
pub(crate) use models_gui::{pane_tree_model, tab_nav_model};
pub(crate) use models_native::config_catalog_snapshot_model;
pub(crate) use models_session::evict_full_model;
pub(crate) use models_update::native_update_status_reconciliation_model;
