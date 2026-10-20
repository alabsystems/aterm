// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The spawned-session environment names (`crate::env_sanitize`), re-exported under the path
//! their callers use.

pub use crate::env_sanitize::{
    ENV_DEFAULT_EDITOR, ENV_DENY_PREFIXES, ENV_DENY_VARS, ENV_DEV_FAST_TIMERS, ENV_EDGE_TOKENS,
    ENV_LAUNCH_NONCE, ENV_MUX_BASE, ENV_OBSERVE_SESSION_ID, ENV_PARENT_SESSION_ID, ENV_SESSION_ID,
    ENV_TAB_SHELL, ENV_TASK_FILE, is_ai_env_key, is_ai_env_var,
};
