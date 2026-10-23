// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm's model of an Aos hosted task's boundary publication (`docs/DESIGN-aos-hosted-execution-2026-10-21.md` §3.9, §6).

mod models_publication;

pub use models_publication::aos_boundary_publication_model;
