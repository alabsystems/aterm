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

/// Environment variable advertising `amacs` as the default editor to shell integration.
pub const ENV_DEFAULT_EDITOR: &str = "ATERM_DEFAULT_EDITOR";

/// Exact environment variable names stripped before spawning a PTY child.
pub const ENV_DENY_VARS: &[&str] = &[
    "ATERM_DEBUG_SEAMLESS_REEXEC",
    "ATERM_DEBUG_RELAUNCH_NUDGE",
    "ATERM_DEBUG_STATUS_BARS",
    "ATERM_DEBUG_PASTE_FILE",
    "ATERM_DEBUG_CLIPBOARD_FILE",
    "ATERM_DEBUG_LOST_SHELL_NONCE",
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
mod tests {
    use super::*;

    /// A non-UTF-8 key is classified by its lossy spelling: a denied prefix survives, an
    /// exact name cannot be forged.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_key_is_classified_by_its_lossy_spelling() {
        use std::os::unix::ffi::OsStrExt as _;
        let key = |b: &[u8]| is_ai_env_key(std::ffi::OsStr::from_bytes(b));
        assert!(key(b"ANTHROPIC_\xffX"));
        assert!(key(b"CLAUDE\xfe"));
        assert!(key(b"AI_\x80"));
        assert!(!key(b"PLAIN_\xffKEY"));
        assert!(!key(b"\xffANTHROPIC_API_KEY"));
        assert!(key(b"ATERM_SESSION_ID"));
        assert!(!key(b"ATERM_SESSION_ID\xff"));
    }

    /// What a child must never inherit: AI-tool context and keys by prefix,
    /// the recursion-provisioning identity/edge vars and the retired
    /// edge-secret names (an inherited copy never leaks past one hop), the
    /// development seams (a nested aterm never runs under its parent's QA
    /// seam), and the retired credentials, all by exact name.
    #[test]
    fn agent_context_seams_and_credentials_are_denied() {
        let ai_role = ["AI", "ROLE"].join("_");
        let ai_worker_id = ["AI", "WORKER", "ID"].join("_");
        for v in [
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_API_KEY",
            "ANTHROPIC_MODEL",
            "COPILOT_TOKEN",
            "CODEX_SESSION",
            "ANTIGRAVITY_CSRF_TOKEN",
            "GEMINI_CLI",
            "GEMINI_API_KEY",
            "CURSOR_SETTINGS",
            &ai_role,
            &ai_worker_id,
            "_DEVTOOL_CARGO_LOCK",
            ENV_SESSION_ID,
            ENV_LAUNCH_NONCE,
            ENV_PARENT_SESSION_ID,
            ENV_MUX_BASE,
            ENV_EDGE_TOKENS,
            ENV_EDGE_READ,
            ENV_EDGE_WRITE,
            ENV_EDGE_SIGNAL,
            ENV_OBSERVE_SESSION_ID,
            "ATERM_DEBUG_SEAMLESS_REEXEC",
            "ATERM_DEBUG_RELAUNCH_NUDGE",
            "ATERM_DEBUG_STATUS_BARS",
            "ATERM_DEBUG_CLIPBOARD_FILE",
            "ATERM_DEBUG_LOST_SHELL_NONCE",
            "ATERM_DEBUG_DIE_BEFORE_DIAL",
            "ATERM_DEBUG_HEADLESS_RESTORE",
            "ATERM_UPDATE_ROOT",
            "ATERM_DEV_CURL",
            "ATERM_HANDOFF_READY_TIMEOUT_MS",
            "ATERM_HANDOFF_PROOF_TIMEOUT_MS",
            "ATERM_DEBUG_QUIT_BYE",
            "ATERM_RESIZE_RUN_GAP_MS",
            "ATERM_SLOG_RING",
            "ATERM_NUDGE_CLASS",
            "ATERM_UPDATE_TOKEN",
            "ATPKG_TOKEN",
            ENV_DEV_FAST_TIMERS,
        ] {
            assert!(is_ai_env_var(v), "{v} must be deny-listed for inheritance");
        }
    }

    /// The editor hint stops at the hop, under both classifiers the spawn seams apply. The
    /// `EDITOR` it leads a shell to export reaches the child
    /// (`the_ordinary_environment_and_aterm_integration_vars_survive`).
    #[test]
    fn default_editor_hint_is_denied() {
        assert_eq!(ENV_DEFAULT_EDITOR, "ATERM_DEFAULT_EDITOR");
        assert!(is_ai_env_var(ENV_DEFAULT_EDITOR));
        assert!(is_ai_env_key(std::ffi::OsStr::new(ENV_DEFAULT_EDITOR)));
    }

    /// What every child must keep: the ordinary environment; the shell
    /// integration vars and the OSC 133/633 capability nonce; and the reroute
    /// seam (`ATERM_REROUTE_DIR`, and `__ATERM_REROUTE_PASSTHROUGH`, the
    /// `--no-reroute` marker an upstream `cargo` hands its own spawns) — so the
    /// `ATERM_*` family is never deny-listed by prefix.
    #[test]
    fn the_ordinary_environment_and_aterm_integration_vars_survive() {
        for v in [
            "PATH",
            "HOME",
            "USER",
            "SHELL",
            "TERM",
            "LANG",
            "EDITOR",
            "SSH_AUTH_SOCK",
            "HOMEBREW_PREFIX",
            "XDG_CONFIG_HOME",
            "ATERM_SHELL_INTEGRATION_DIR",
            "ATERM_ORIGINAL_ZDOTDIR",
            "ATERM_UNSET_ZDOTDIR",
            "ATERM_SHELL_NONCE",
            "__ATERM_REROUTE_PASSTHROUGH",
            "ATERM_REROUTE_DIR",
            "ATERM_GEMINI_BIN",
        ] {
            assert!(!is_ai_env_var(v), "{v} must reach the child");
        }
    }
}
