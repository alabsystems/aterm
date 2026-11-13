// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm's models of the first-launch setup run.

mod models_clt_episode;
mod models_setup_ledger;

pub use models_clt_episode::{clt_episode_liveness, clt_episode_model};
pub use models_setup_ledger::{setup_kept_key_model, setup_ledger_liveness, setup_ledger_model};
