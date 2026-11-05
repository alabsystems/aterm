// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm's models of Aos hosted execution: a task's boundary publication
//! (`docs/DESIGN-aos-hosted-execution-2026-10-21.md` §3.9, §6), and the momentum glow's hold on
//! typed keys that wait for their echo (D12).

mod models_hold;
mod models_publication;

pub use models_hold::aos_key_hold_model;
pub use models_publication::aos_boundary_publication_model;
