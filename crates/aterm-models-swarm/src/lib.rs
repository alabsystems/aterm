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
//! crossed fence), re-key convergence (one ranked minter per due, bounded by its graces), and
//! gap healing (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.2 M5: a missing `Wrap` is a gap a
//! ranked holder heals, never a revocation), the mesh relay (§9 `MeshRelay`: each record appended
//! once, only current origins, the newest row the highest, every up Mac given every table record),
//! fabric routing and fleet-wide halts (§9 `FabricRouting`: mail's one hop, the proven sender,
//! dedup, revocation both ways, and a halt lifted on every member; `FabricMoves`: mail shown once
//! per session across key rotations), and the machine-number negotiation
//! (`docs/DESIGN-machine-names-2026-11-04.md` §6.6: who holds a number, the older builds' implied
//! records and migration, a machine's primary, succession, stripped and opaque entries, and the
//! automatic pick).

mod models_cached_log;
mod models_cached_materialization;
mod models_commons_log;
mod models_denied_cache;
mod models_envelope_denials;
mod models_epoch_convergence;
mod models_fabric_routing;
mod models_gap_heal;
mod models_join_phrase;
mod models_journal_append;
mod models_journal_open;
mod models_keyed_epochs;
mod models_ledger_publish;
mod models_machine_number;
mod models_membership;
mod models_mesh_relay;
mod models_peer_replacement;
mod models_peer_selection;
mod models_records_capacity;
mod models_records_file;
mod models_retained_frontier;
mod models_role;

pub use models_cached_log::{
    cached_log_model, cached_publication_model, caller_custody_model, cold_read_model,
    configured_log_model,
};
pub use models_cached_materialization::cached_materialization_model;
pub use models_commons_log::commons_log_model;
pub use models_denied_cache::fleet_denied_cache_model;
pub use models_envelope_denials::envelope_denials_model;
pub use models_epoch_convergence::epoch_convergence_model;
pub use models_fabric_routing::{
    fabric_halts_model, fabric_moves_model, fabric_routing_model, halt_lifts_everywhere_liveness,
    mail_arrives_liveness, mail_shown_liveness,
};
pub use models_gap_heal::gap_heal_model;
pub use models_join_phrase::join_phrase_model;
pub use models_journal_append::journal_append_model;
pub use models_journal_open::journal_open_model;
pub use models_keyed_epochs::keyed_epochs_model;
pub use models_ledger_publish::ledger_publish_model;
pub use models_machine_number::{
    machine_number_choose_model, machine_number_evidence_model, machine_number_group_model,
    machine_number_holder_model, machine_number_press_model, machine_number_stripped_model,
    machine_number_succession_model,
};
pub use models_membership::membership_model;
pub use models_mesh_relay::{
    PRESENCE_FAIR, SETTLES, TABLES_FAIR, every_up_mac_gets_every_table_record_liveness,
    mesh_relay_model, restarted_mac_reads_up_liveness,
};
pub use models_peer_replacement::peer_replacement_model;
pub use models_peer_selection::peer_selection_model;
pub use models_records_capacity::records_capacity_model;
pub use models_records_file::records_file_model;
pub use models_retained_frontier::{
    retained_commit_model, retained_frontier_model, retained_replay_model,
};
pub use models_role::role_retention_model;
