// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm's models of the native chrome (tabs, panes, settings, documents).

mod models_engine_corpus;
mod models_native;

pub use models_engine_corpus::*;
pub use models_native::*;

pub mod models_amacs_anchor;
pub mod models_amacs_proposal;
pub mod models_amacs_ticket;
pub mod models_amacs_undo;
