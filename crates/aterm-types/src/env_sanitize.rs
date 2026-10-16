// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Environment sanitization for PTY spawn paths: the deny lists for AI-tool variables
//! (meaningless and confusing inside a user's shell) and aterm's one-hop provisioning
//! variables. Public callers reach it through `aterm_types::domain`.

/// Prefixes of environment variables every PTY spawn path filters before exec.
pub const ENV_DENY_PREFIXES: &[&str] = &[
    "CLAUDE",       // CLAUDECODE, CLAUDE_CODE_*, CLAUDE_*
    "ANTHROPIC_",   // ANTHROPIC_MODEL, ANTHROPIC_API_KEY, etc.
    "COPILOT_",     // GitHub Copilot
    "CODEX_",       // OpenAI Codex
    "ANTIGRAVITY_", // Google's Antigravity CLI (agy): its tools' ANTIGRAVITY_CSRF_TOKEN and kin
    "CURSOR_",      // Cursor editor
    "AI_",          // AI development tool infrastructure vars
    "_DEVTOOL_",    // Internal development tool runtime vars
];

// Recursion provisioning: how a launching aterm hands a child its fabric identity and
// capability edges, so an outer agent holds authority over the inner session it spawned. All
// are deny-listed so an inherited copy never passes one hop; each direct child gets a fresh
// set via `env_add`, applied on top of the stripped environment.

/// The child adopts this as its root session id (`s-<20hex>`), so the outer's preminted edges
/// (which name it as `dst`) authorize against the child's table.
pub const ENV_SESSION_ID: &str = "ATERM_SESSION_ID";
/// The child adopts this as its root launch nonce (`<32hex>`); the parent's preminted edges
/// bind to it. It is pinned, not fresh: a child re-exec'd in the same shell adopts the same
/// nonce, so `LaunchNonce`'s cross-relaunch protection does not hold here. The same-uid
/// boundary and edge-token secrecy bound authority; the nonce is a binding key.
pub const ENV_LAUNCH_NONCE: &str = "ATERM_LAUNCH_NONCE";
/// The parent session id (`s-<20hex>`): the `src` of the child's edges.
pub const ENV_PARENT_SESSION_ID: &str = "ATERM_PARENT_SESSION_ID";
/// The multiplexer signature this session was born into (`"<$TMUX>|<$STY>"`), stamped by the
/// spawn seam so `aterm ctl` can tell a session in a tmux/screen pane (where flagless verbs
/// would drive the outer terminal) from one that merely inherited a pane's environment.
pub const ENV_MUX_BASE: &str = "ATERM_MUX_BASE";
/// Path to the 0600 file holding the parent→child edge-token secrets (only the path is in
/// env, so a same-uid peer that cannot read 0600 files cannot open it). Three lines:
/// `read <64hex>` / `write <64hex>` / `signal <64hex>`.
///
/// The file persists for the parent session, beside the per-launch `aterm-<pid>.token`: the
/// child reads it without deleting it, because a child re-exec'd in the same shell
/// re-inherits the path and must re-install the parent edges. The parent removes it; a crash
/// leftover is inert (its tokens bind a random `(sid, nonce)` never reissued).
pub const ENV_EDGE_TOKENS: &str = "ATERM_EDGE_TOKENS";
/// The retired env channel for a `ReadScreen` `EdgeToken`. Nothing reads it (the secrets
/// travel in the [`ENV_EDGE_TOKENS`] file), but an older aterm's value is still a bearer
/// secret, so the name stays deny-listed.
pub const ENV_EDGE_READ: &str = "ATERM_EDGE_READ";
/// The retired env channel for a `WriteInput` `EdgeToken`; see [`ENV_EDGE_READ`].
pub const ENV_EDGE_WRITE: &str = "ATERM_EDGE_WRITE";
/// The retired env channel for a `Signal` `EdgeToken`; see [`ENV_EDGE_READ`].
pub const ENV_EDGE_SIGNAL: &str = "ATERM_EDGE_SIGNAL";

/// The sid a controller session was spawned to observe (`SESSION_CONNECTIONS.md` §2.3/§6),
/// injected by the "New Controller Session" presets and `spawn connected=controller of=<sid>`.
/// Identity only, never a token: authority stays in the origin's `EdgeTable`.
pub const ENV_OBSERVE_SESSION_ID: &str = "ATERM_OBSERVE_SESSION_ID";

/// The shell program this tab runs, as the window resolved it (Windows: an absolute path),
/// so a CLI typed in the tab (`aterm doctor`, `show-config`) can report the tab's shell; it
/// cannot see the window's per-launch `--shell` flag. Deny-listed so it describes only the
/// tab it was handed to.
pub const ENV_TAB_SHELL: &str = "ATERM_TAB_SHELL";

/// Exact env vars that should not leak into child shells: by exact name, because other
/// `ATERM_*` variables are needed for shell integration in the child.
pub const ENV_DENY_VARS: &[&str] = &[
    // Development seams (`dev_seam!`, read only by a dev build): an aterm launched from an
    // aterm shell never runs under a seam its parent's environment carried.
    "ATERM_DEBUG_SEAMLESS_REEXEC",
    "ATERM_DEBUG_RELAUNCH_NUDGE",
    "ATERM_DEBUG_STATUS_BARS",
    "ATERM_DEBUG_STRAIN",
    "ATERM_DEBUG_PASTE_FILE",
    "ATERM_DEBUG_CLIPBOARD_FILE",
    "ATERM_DEBUG_LOST_SHELL_NONCE",
    "ATERM_UPDATE_ROOT",
    "ATERM_HANDOFF_READY_TIMEOUT_MS",
    "ATERM_HANDOFF_PROOF_TIMEOUT_MS",
    "ATERM_NUDGE_CLASS",
    "ATERM_RESIZE_RUN_GAP_MS",
    // The successor an update launches still receives this one: LaunchServices merges the
    // launching process's environment, with no child shell in between.
    "ATERM_DEBUG_DIE_BEFORE_DIAL",
    "ATERM_DEBUG_HEADLESS_RESTORE",
    // Retired credentials stay denied: nothing reads them, but a secret an old install.sh
    // told people to export (`launchctl setenv`) outlives the knob and would reach every tab.
    "ATERM_UPDATE_TOKEN",
    "ATPKG_TOKEN",
    // Recursion provisioning (re-injected fresh per direct child via env_add).
    ENV_SESSION_ID,
    ENV_LAUNCH_NONCE,
    ENV_PARENT_SESSION_ID,
    ENV_MUX_BASE,
    ENV_EDGE_TOKENS,
    // The retired edge-secret env channel: still bearer secrets.
    ENV_EDGE_READ,
    ENV_EDGE_WRITE,
    ENV_EDGE_SIGNAL,
    // Controller-spawn observation hint: a descendant that did not receive it fresh is not
    // the controller.
    ENV_OBSERVE_SESSION_ID,
    ENV_TAB_SHELL,
    // Fabric credentials: the broker endpoint, cap file and fleet name are the node's
    // identity on the bus, so a nested aterm that inherited them could publish as the outer
    // node under its cap. An inner aterm becomes a node only when a human mints it a cap.
    "ATERM_LINK_BROKER",
    "ATERM_LINK_CAP_FILE",
    "ATERM_LINK_FLEET",
    // The fabric launch knob: inherited, it would start a second bridge as the outer node.
    // This closes the env route only; a nested aterm reads `[fabric] command` from the
    // per-user config the same, and the state dir's lock (aterm-link `StateDir::lock_bridge`)
    // keeps that second bridge standing by until the outer one exits.
    "ATERM_FABRIC_COMMAND",
];

/// Returns `true` if `key` matches a deny-listed AI or provisioning env var.
#[must_use]
// #[inline] so the MIR crosses the crate boundary and callers' Trust gates (aterm-pty) verify
// this body.
#[inline]
pub fn is_ai_env_var(key: &str) -> bool {
    // Explicit loops: `slice::contains` / `Iterator::any` dispatch through absent std trait
    // bodies.
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

/// [`is_ai_env_var`] for a key as the OS hands it over: the classifier the spawn seams apply
/// to an inherited environment (`aterm-pty`'s `build_child_env`, `aterm-gui`'s fabric bridge).
///
/// A non-Unicode key is classified by its lossy spelling, never let through: the exact names
/// are ASCII and U+FFFD equals none, while a prefix match keeps working because the lossy
/// conversion keeps every ASCII byte (`ANTHROPIC_\xffX` is denied). Windows names are
/// case-insensitive, so there the key is ASCII-uppercased first (every denied name and prefix
/// is uppercase).
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

    #[test]
    fn test_is_ai_env_var_matches_deny_prefixes() {
        assert!(is_ai_env_var("CLAUDECODE"));
        assert!(is_ai_env_var("CLAUDE_CODE_ENTRYPOINT"));
        assert!(is_ai_env_var("CLAUDE_API_KEY"));
        assert!(is_ai_env_var("ANTHROPIC_MODEL"));
        assert!(is_ai_env_var("COPILOT_TOKEN"));
        assert!(is_ai_env_var("CODEX_SESSION"));
        assert!(is_ai_env_var("ANTIGRAVITY_CSRF_TOKEN"));
        assert!(is_ai_env_var("CURSOR_SETTINGS"));
        let ai_role = ["AI", "ROLE"].join("_");
        let ai_worker_id = ["AI", "WORKER", "ID"].join("_");
        assert!(is_ai_env_var(&ai_role));
        assert!(is_ai_env_var(&ai_worker_id));
        assert!(is_ai_env_var("_DEVTOOL_CARGO_LOCK"));
    }

    #[test]
    fn test_is_ai_env_var_preserves_shell_integration_vars() {
        assert!(!is_ai_env_var("ATERM_SHELL_INTEGRATION_DIR"));
        assert!(!is_ai_env_var("ATERM_ORIGINAL_ZDOTDIR"));
        assert!(!is_ai_env_var("ATERM_UNSET_ZDOTDIR"));
    }

    /// The OSC 133/633 capability nonce survives, so the shell-integration preamble can emit
    /// `id=<hex>`.
    #[test]
    fn test_aterm_shell_nonce_survives_sanitization() {
        assert!(!is_ai_env_var("ATERM_SHELL_NONCE"));
    }

    /// The reroute seam's variables (`atpkg::reroute`) reach every child: `ATERM_REROUTE_DIR`
    /// is re-asserted on PATH after the rc files, and `__ATERM_REROUTE_PASSTHROUGH` is
    /// `--no-reroute`'s marker an upstream `cargo` hands its own spawns. Pins that `ATERM_*`
    /// is not denied by prefix.
    #[test]
    fn test_reroute_seam_vars_survive_sanitization() {
        assert!(!is_ai_env_var("__ATERM_REROUTE_PASSTHROUGH"));
        assert!(!is_ai_env_var("ATERM_REROUTE_DIR"));
    }

    /// The recursion-provisioning vars and retired edge-secret names are denied by exact name.
    #[test]
    fn test_recursion_provisioning_vars_are_denied_by_name() {
        for v in [
            ENV_SESSION_ID,
            ENV_LAUNCH_NONCE,
            ENV_PARENT_SESSION_ID,
            ENV_MUX_BASE,
            ENV_EDGE_TOKENS,
            ENV_EDGE_READ,
            ENV_EDGE_WRITE,
            ENV_EDGE_SIGNAL,
            ENV_OBSERVE_SESSION_ID,
        ] {
            assert!(is_ai_env_var(v), "{v} must be deny-listed for inheritance");
        }
        // Not over-broad: shell-integration ATERM_* vars are kept.
        assert!(!is_ai_env_var("ATERM_SHELL_INTEGRATION_DIR"));
    }

    /// The development seams and the retired credentials are denied by exact name.
    #[test]
    fn test_update_contract_vars_are_denied_by_name() {
        for v in [
            "ATERM_DEBUG_SEAMLESS_REEXEC",
            "ATERM_DEBUG_RELAUNCH_NUDGE",
            "ATERM_DEBUG_STATUS_BARS",
            "ATERM_DEBUG_STRAIN",
            "ATERM_DEBUG_CLIPBOARD_FILE",
            "ATERM_DEBUG_LOST_SHELL_NONCE",
            "ATERM_UPDATE_ROOT",
            "ATERM_HANDOFF_READY_TIMEOUT_MS",
            "ATERM_HANDOFF_PROOF_TIMEOUT_MS",
            "ATERM_DEBUG_DIE_BEFORE_DIAL",
            "ATERM_DEBUG_HEADLESS_RESTORE",
            "ATERM_NUDGE_CLASS",
            "ATERM_RESIZE_RUN_GAP_MS",
        ] {
            assert!(is_ai_env_var(v), "{v} must be deny-listed for inheritance");
        }
        for credential in ["ATERM_UPDATE_TOKEN", "ATPKG_TOKEN"] {
            assert!(
                is_ai_env_var(credential),
                "{credential} is a retired CREDENTIAL and must stay deny-listed"
            );
        }
    }

    #[test]
    fn test_is_ai_env_var_preserves_standard_vars() {
        assert!(!is_ai_env_var("PATH"));
        assert!(!is_ai_env_var("HOME"));
        assert!(!is_ai_env_var("USER"));
        assert!(!is_ai_env_var("SHELL"));
        assert!(!is_ai_env_var("TERM"));
        assert!(!is_ai_env_var("LANG"));
        assert!(!is_ai_env_var("EDITOR"));
        assert!(!is_ai_env_var("SSH_AUTH_SOCK"));
        assert!(!is_ai_env_var("HOMEBREW_PREFIX"));
        assert!(!is_ai_env_var("XDG_CONFIG_HOME"));
    }
}
