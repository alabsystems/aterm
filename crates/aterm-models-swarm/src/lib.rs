// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! Formal models of the ALab swarm protocols: ledger publishing
//! (`docs/DESIGN-alab-swarm-2026-10-27.md` §3.4), and join phrases, the membership log, and
//! log-backed Commons control (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.2, §3.3.4, and §4.1
//! F18).

mod models_commons_log;
mod models_join_phrase;
mod models_ledger_publish;
mod models_membership;

pub use models_commons_log::commons_log_model;
pub use models_join_phrase::join_phrase_model;
pub use models_ledger_publish::ledger_publish_model;
pub use models_membership::membership_model;
