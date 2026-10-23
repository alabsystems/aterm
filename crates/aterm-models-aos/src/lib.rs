// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm's models of Aos hosted execution (`docs/DESIGN-aos-hosted-execution-2026-10-21.md`).

mod models_publication;

pub use models_publication::aos_boundary_publication_model;
