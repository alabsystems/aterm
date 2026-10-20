// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The committed models the engine's own unit tests run on (the ring, the session kernel, the
//! config catalog, …), mounted from their domain crates' sources rather than copied, so a unit
//! test here exercises the model that ships. Each domain crate keeps these few in a file of their
//! own, `models_engine_corpus.rs`, that names only `aterm_spec::…` paths and its own items, which
//! is what lets it compile here as well as in its crate (`aterm_spec` is this crate's own alias).
//! Mounting only those files keeps every other model edit out of this crate's tests and out of
//! the `verify` change cone a mount seeds (aterm-spec and all its dependents). Test builds only:
//! no library carries a model.

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
