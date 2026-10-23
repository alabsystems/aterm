// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm's models of the release cutter and its trust facts.

mod models_release;
mod models_release_publish_once;
mod models_trust_facts;

pub use models_release::*;
pub use models_release_publish_once::release_publish_once_model;
pub use models_trust_facts::trust_facts_reuse_model;
