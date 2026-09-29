// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The spawned-session environment names, under the path their callers use.
//!
//! This module once carried a WezTerm-style domain abstraction (`Domain`,
//! `Pane`, `DomainRegistry`, SSH/mux connection configs) that no domain ever
//! implemented; only its own tests used it, and it was deleted on 2026-09-25.
//! What remains is the re-export of the environment contract a spawn writes and
//! a launcher strips (`crate::env_sanitize`).

pub use crate::env_sanitize::{
    ENV_DENY_PREFIXES, ENV_DENY_VARS, ENV_EDGE_TOKENS, ENV_LAUNCH_NONCE, ENV_MUX_BASE,
    ENV_OBSERVE_SESSION_ID, ENV_PARENT_SESSION_ID, ENV_SESSION_ID, ENV_TAB_SHELL, is_ai_env_key,
    is_ai_env_var,
};
