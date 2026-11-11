// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Environment variable deny lists for PTY child processes.

/// Environment variable prefixes stripped before spawning a PTY child.
pub const ENV_DENY_PREFIXES: &[&str] = &[
    "CLAUDE",
    "ANTHROPIC_",
    "COPILOT_",
    "CODEX_",
    "ANTIGRAVITY_",
    "GEMINI_",
    "CURSOR_",
    "GROK_",
    "XAI_",
    "KIMI_",
    "MOONSHOT_",
    "AI_",
    "_DEVTOOL_",
];

/// Environment variable passing a child's root session ID (`s-<20hex>`).
pub const ENV_SESSION_ID: &str = "ATERM_SESSION_ID";
/// Environment variable passing a child's root launch nonce (`<32hex>`).
pub const ENV_LAUNCH_NONCE: &str = "ATERM_LAUNCH_NONCE";
/// Environment variable passing the parent session ID (`s-<20hex>`).
pub const ENV_PARENT_SESSION_ID: &str = "ATERM_PARENT_SESSION_ID";
/// Environment variable recording the multiplexer signature at spawn (`"<$TMUX>|<$STY>"`).
pub const ENV_MUX_BASE: &str = "ATERM_MUX_BASE";
/// Environment variable naming the 0600 file holding parent-to-child edge tokens.
pub const ENV_EDGE_TOKENS: &str = "ATERM_EDGE_TOKENS";
/// Retired environment variable for a `ReadScreen` `EdgeToken`.
pub const ENV_EDGE_READ: &str = "ATERM_EDGE_READ";
/// Retired environment variable for a `WriteInput` `EdgeToken`.
pub const ENV_EDGE_WRITE: &str = "ATERM_EDGE_WRITE";
/// Retired environment variable for a `Signal` `EdgeToken`.
pub const ENV_EDGE_SIGNAL: &str = "ATERM_EDGE_SIGNAL";

/// Environment variable naming the session ID a controller tab was spawned to observe.
pub const ENV_OBSERVE_SESSION_ID: &str = "ATERM_OBSERVE_SESSION_ID";

/// Environment variable naming a spawned worker's one-shot task file.
pub const ENV_TASK_FILE: &str = "ATERM_TASK_FILE";

/// Environment variable recording the resolved shell executable for a tab.
pub const ENV_TAB_SHELL: &str = "ATERM_TAB_SHELL";

/// Development-seam environment variable enabling fast harness timers in debug builds.
pub const ENV_DEV_FAST_TIMERS: &str = "ATERM_DEV_FAST_TIMERS";

/// Development-seam environment variable naming the sleep a debug build's hosted harness loops
/// read: `none`, a host that never sleeps, which every live test's instance sets.
pub const ENV_DEBUG_HOST_SLEEP: &str = "ATERM_DEBUG_HOST_SLEEP";

/// Environment variable advertising `amacs` as the default editor to shell integration.
pub const ENV_DEFAULT_EDITOR: &str = "ATERM_DEFAULT_EDITOR";

/// The word an aterm tab exports as `EDITOR` when the person set none: amacs, bare, because a
/// bundle path holds spaces that `sh -c "$EDITOR file"` would split.
pub const DEFAULT_EDITOR_WORD: &str = "amacs";

/// Environment variable every aterm tab's session carries (`ATERM_CHILD=1`).
pub const ENV_ATERM_CHILD: &str = "ATERM_CHILD";

/// Whether an inherited `EDITOR` is the default an outer aterm's tab exported rather than the
/// person's own: exactly [`DEFAULT_EDITOR_WORD`], in an environment an aterm tab handed down
/// (`aterm_child`, the inherited [`ENV_ATERM_CHILD`], is non-empty). Each aterm drops such an
/// `EDITOR` and derives its own default under its own settings, as it does
/// [`ENV_DEFAULT_EDITOR`]; a person who exports `EDITOR=amacs` in their startup files gets it back
/// when those files run again.
#[must_use]
pub fn is_handed_down_editor(
    editor: &std::ffi::OsStr,
    aterm_child: Option<&std::ffi::OsStr>,
) -> bool {
    editor == DEFAULT_EDITOR_WORD && aterm_child.is_some_and(|value| !value.is_empty())
}

/// Exact environment variable names stripped before spawning a PTY child.
pub const ENV_DENY_VARS: &[&str] = &[
    "ATERM_DEBUG_SEAMLESS_REEXEC",
    "ATERM_DEBUG_SEAMLESS_KEEP",
    "ATERM_DEBUG_RELAUNCH_NUDGE",
    "ATERM_DEBUG_STATUS_BARS",
    "ATERM_DEBUG_PASTE_FILE",
    "ATERM_DEBUG_CLIPBOARD_FILE",
    "ATERM_DEBUG_LOST_SHELL_NONCE",
    "ATERM_DEBUG_TURN_KEY_CLOCK_STEP_MS",
    "ATERM_DEBUG_TURN_KEY_BARRIER",
    "ATERM_DEBUG_PROMPT_CLOCK",
    "ATERM_DEBUG_FLEET_CLOCK",
    "ATERM_DEBUG_ECHO_CLOCK",
    "ATERM_DEBUG_TYPING_CLOCK",
    "ATERM_UPDATE_ROOT",
    "ATERM_DEV_CURL",
    "ATERM_HANDOFF_READY_TIMEOUT_MS",
    "ATERM_HANDOFF_PROOF_TIMEOUT_MS",
    "ATERM_NUDGE_CLASS",
    "ATERM_RESIZE_RUN_GAP_MS",
    "ATERM_SLOG_RING",
    "ATERM_DEBUG_DIE_BEFORE_DIAL",
    "ATERM_DEBUG_HEADLESS_RESTORE",
    "ATERM_DEBUG_QUIT_BYE",
    "ATERM_DEBUG_HEADLESS_PKG_LANE",
    ENV_DEV_FAST_TIMERS,
    ENV_DEBUG_HOST_SLEEP,
    "ATERM_UPDATE_TOKEN",
    "ATPKG_TOKEN",
    ENV_SESSION_ID,
    ENV_LAUNCH_NONCE,
    ENV_PARENT_SESSION_ID,
    ENV_MUX_BASE,
    ENV_EDGE_TOKENS,
    ENV_EDGE_READ,
    ENV_EDGE_WRITE,
    ENV_EDGE_SIGNAL,
    ENV_OBSERVE_SESSION_ID,
    ENV_TAB_SHELL,
    ENV_TASK_FILE,
    ENV_DEFAULT_EDITOR,
    "ATERM_LINK_BROKER",
    "ATERM_LINK_CAP_FILE",
    "ATERM_LINK_FLEET",
    "ATERM_FABRIC_COMMAND",
];

/// The variables that mean an agent is running this command: its tools carry them.
pub const AGENT_MARKERS: &[&str] = &[
    "CLAUDECODE",
    "CODEX_THREAD_ID",
    "CODEX_SANDBOX",
    "ATERM_AI_REVIEW",
];

/// The first [`AGENT_MARKERS`] variable set, if any.
#[must_use]
pub fn agent_marker(var: &dyn Fn(&str) -> Option<String>) -> Option<&'static str> {
    AGENT_MARKERS
        .iter()
        .copied()
        .find(|k| var(k).is_some_and(|v| !v.is_empty()))
}

/// Returns `true` if `key` matches a deny-listed AI or provisioning environment variable.
#[must_use]
#[inline]
pub fn is_ai_env_var(key: &str) -> bool {
    let mut denied = false;
    for k in ENV_DENY_VARS {
        if *k == key {
            denied = true;
            break;
        }
    }
    if denied {
        return true;
    }
    for prefix in ENV_DENY_PREFIXES {
        if key.starts_with(prefix) {
            return true;
        }
    }
    false
}

/// Classifies an OS environment variable name against [`is_ai_env_var`].
#[must_use]
pub fn is_ai_env_key(key: &std::ffi::OsStr) -> bool {
    let key = key.to_string_lossy();
    #[cfg(windows)]
    {
        is_ai_env_var(&key.to_ascii_uppercase())
    }
    #[cfg(not(windows))]
    {
        is_ai_env_var(&key)
    }
}

#[cfg(test)]
#[path = "env_sanitize_tests.rs"]
mod tests;
