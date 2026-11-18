// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The spawned-session environment names (`crate::env_sanitize`), re-exported under the path
//! their callers use.

pub use crate::env_sanitize::{
    AGENT_MARKERS, DEFAULT_EDITOR_WORD, DEFAULT_ENV_STRIP, ENV_ATERM_CHILD, ENV_CLAUDE_TOKEN,
    ENV_CLAUDE_TOKEN_MINTED, ENV_DEBUG_HOST_SLEEP, ENV_DEFAULT_EDITOR, ENV_DENY_VARS,
    ENV_DEV_FAST_TIMERS, ENV_EDGE_TOKENS, ENV_LAUNCH_NONCE, ENV_MUX_BASE, ENV_OBSERVE_SESSION_ID,
    ENV_PARENT_SESSION_ID, ENV_SESSION_ID, ENV_TAB_SHELL, ENV_TASK_FILE, EnvStrip, EnvStripRefusal,
    PROTECTED_ENV_NAMES, admit_env_strip_entry, agent_marker, effective_env_strip,
    install_env_strip, is_ai_env_key, is_ai_env_var, is_handed_down_editor,
};
