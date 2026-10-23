// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Committed domain models mounted for the engine's unit tests.

#[path = "../../aterm-models-core/src/models_engine_corpus.rs"]
mod models_core;
#[path = "../../aterm-models-gui/src/models_engine_corpus.rs"]
mod models_gui;
#[path = "../../aterm-models-native/src/models_engine_corpus.rs"]
mod models_native;
#[path = "../../aterm-models-session/src/models_engine_corpus.rs"]
mod models_session;
#[path = "../../aterm-models-update/src/models_engine_corpus.rs"]
mod models_update;

pub(crate) use models_core::{ring_model, subscribe_model};
pub(crate) use models_gui::{pane_tree_model, tab_nav_model};
pub(crate) use models_native::config_catalog_snapshot_model;
pub(crate) use models_session::evict_full_model;
pub(crate) use models_update::native_update_status_reconciliation_model;
