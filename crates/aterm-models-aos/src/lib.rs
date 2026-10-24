// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm's models of Aos hosted execution: a task's boundary publication
//! (`docs/DESIGN-aos-hosted-execution-2026-10-21.md` §3.9, §6) and M5b's worker-lease ledger
//! (§11.5, §11.7).

mod models_lease;
mod models_publication;

pub use models_lease::{aos_worker_lease_liveness, aos_worker_lease_model};
pub use models_publication::aos_boundary_publication_model;
