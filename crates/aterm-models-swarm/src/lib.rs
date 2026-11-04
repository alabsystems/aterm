// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! Formal models of the ALab swarm protocols: ledger publishing
//! (`docs/DESIGN-alab-swarm-2026-10-27.md` §3.4), and join phrases, the membership log,
//! log-backed Commons control, writes under a removed Mac's epoch across a heal, a vault
//! server's records file (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.2, §3.3.4, and §4.1 F18,
//! F21 and F23), the membership log's record capacity (its budgets, the proof reserve and the
//! crossed fence), and re-key convergence (one ranked minter per due, bounded by its graces).

mod models_cached_log;
mod models_cached_materialization;
mod models_commons_log;
mod models_envelope_denials;
mod models_epoch_convergence;
mod models_join_phrase;
mod models_keyed_epochs;
mod models_ledger_publish;
mod models_membership;
mod models_peer_replacement;
mod models_peer_selection;
mod models_records_capacity;
mod models_records_file;
mod models_role;

pub use models_cached_log::{
    cached_log_model, cached_publication_model, caller_custody_model, cold_read_model,
    configured_log_model,
};
pub use models_cached_materialization::cached_materialization_model;
pub use models_commons_log::commons_log_model;
pub use models_envelope_denials::envelope_denials_model;
pub use models_epoch_convergence::epoch_convergence_model;
pub use models_join_phrase::join_phrase_model;
pub use models_keyed_epochs::keyed_epochs_model;
pub use models_ledger_publish::ledger_publish_model;
pub use models_membership::membership_model;
pub use models_peer_replacement::peer_replacement_model;
pub use models_peer_selection::peer_selection_model;
pub use models_records_capacity::records_capacity_model;
pub use models_records_file::records_file_model;
pub use models_role::role_retention_model;
