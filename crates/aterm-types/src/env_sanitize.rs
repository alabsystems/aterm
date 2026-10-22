// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Environment sanitization for PTY spawn paths: the deny lists for AI-tool variables
//! (meaningless and confusing inside a user's shell) and aterm's one-hop provisioning
//! variables. Public callers reach it through `aterm_types::domain`.

/// Prefixes of environment variables every PTY spawn path filters before exec.
///
/// No `MUSE_` prefix: Muse Code 1.4.10 gives its tools' shells `MUSE_SESSION_ID`,
/// `MUSE_THREAD_ID` and `MUSE_TOOL_USE_ID` (ids, no secret) beside
/// `CLAUDE_CODE_TOOL_USE_ID`, which `CLAUDE` strips (measured in `aterm-lab`), and the
/// word is generic.
pub const ENV_DENY_PREFIXES: &[&str] = &[
    "CLAUDE",       // CLAUDECODE, CLAUDE_CODE_*, CLAUDE_*
    "ANTHROPIC_",   // ANTHROPIC_MODEL, ANTHROPIC_API_KEY, etc.
    "COPILOT_",     // GitHub Copilot
    "CODEX_",       // OpenAI Codex
    "ANTIGRAVITY_", // Google's Antigravity CLI (agy): its tools' ANTIGRAVITY_CSRF_TOKEN and kin
    "GEMINI_",      // Google's Gemini CLI: its tools' GEMINI_CLI, GEMINI_API_KEY and kin
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

/// A WORKER's task file (`spawn agent=claude`, the sessions-as-workers design
/// §2.1): the 0600 file under `<state>/tasks/` holding the task its starter
/// gave it, named to the worker's shell so the one line aterm types at its
/// first prompt hands the file's text to the agent as argv. Deny-listed
/// (below): the task is that session's, never a nested aterm's tabs'.
pub const ENV_TASK_FILE: &str = "ATERM_TASK_FILE";

/// The shell program this tab runs, as the window resolved it (Windows: an absolute path),
/// so a CLI typed in the tab (`aterm doctor`, `show-config`) can report the tab's shell; it
/// cannot see the window's per-launch `--shell` flag. Deny-listed so it describes only the
/// tab it was handed to.
pub const ENV_TAB_SHELL: &str = "ATERM_TAB_SHELL";

/// THE FAST-TIMERS DEVELOPMENT SEAM (`dev_seam!`, so a debug build reads it and a
/// shipped one never does; `aterm_agent::harness::timings::FAST_SEAM` reads it by
/// this name): set, a live instance runs its harness's and its hosts' waits at the
/// test values (`aterm_agent::harness::Timings::FAST`) instead of the production
/// ones. The live headless suites boot their instances under it
/// (`crates/aterm/tests/support/headless_boot.rs`), except the tests whose property
/// is a production clock. Denied for inheritance ([`ENV_DENY_VARS`]): a nested aterm
/// runs its own clock.
pub const ENV_DEV_FAST_TIMERS: &str = "ATERM_DEV_FAST_TIMERS";

/// The editor hint (`docs/DESIGN-amacs-2026-10-19.md` §8.3): set to the bare word `amacs`
/// when this instance offers amacs as the default `EDITOR`, which a tab's shell integration
/// exports only when neither `EDITOR` nor `VISUAL` is set. A per-hop hint: each instance
/// re-derives it from its own config and PATH, so an inherited copy is denied
/// ([`ENV_DENY_VARS`]) and a nested aterm never offers what only its parent decided.
pub const ENV_DEFAULT_EDITOR: &str = "ATERM_DEFAULT_EDITOR";

/// Exact env vars that should not leak into child shells: by exact name, because other
/// `ATERM_*` variables are needed for shell integration in the child.
pub const ENV_DENY_VARS: &[&str] = &[
    // Development seams (`dev_seam!`, read only by a dev build): an aterm launched from an
    // aterm shell never runs under a seam its parent's environment carried.
    "ATERM_DEBUG_SEAMLESS_REEXEC",
    "ATERM_DEBUG_RELAUNCH_NUDGE",
    "ATERM_DEBUG_STATUS_BARS",
    "ATERM_DEBUG_PASTE_FILE",
    "ATERM_DEBUG_CLIPBOARD_FILE",
    "ATERM_DEBUG_LOST_SHELL_NONCE",
    "ATERM_UPDATE_ROOT",
    // The curl a dev build's update lanes run in place of /usr/bin/curl (aterm-update-core).
    "ATERM_DEV_CURL",
    "ATERM_HANDOFF_READY_TIMEOUT_MS",
    "ATERM_HANDOFF_PROOF_TIMEOUT_MS",
    "ATERM_NUDGE_CLASS",
    "ATERM_RESIZE_RUN_GAP_MS",
    // The session log's ring bounds (phase 4's world tests).
    "ATERM_SLOG_RING",
    // The successor an update launches still receives this one: LaunchServices merges the
    // launching process's environment, with no child shell in between.
    "ATERM_DEBUG_DIE_BEFORE_DIAL",
    "ATERM_DEBUG_HEADLESS_RESTORE",
    // A quit that tells its bridge `bye handoff` (aterm-gui `fabric_launch::bye_at_quit`,
    // the aterm-link roster test's stand-in for a Commit): a nested aterm's quit
    // withdraws its own sessions.
    "ATERM_DEBUG_QUIT_BYE",
    "ATERM_DEBUG_HEADLESS_PKG_LANE",
    // The test values of every harness and host wait: a nested aterm runs its
    // own clock, never the one its parent's test suite asked for.
    ENV_DEV_FAST_TIMERS,
    // RETIRED CREDENTIALS STAY DENIED. Nothing reads these, but the value is a SECRET
    // that an old install.sh told people to export (`launchctl setenv`); that export
    // outlives the knob, and without this entry would reach every program in every
    // tab. A token a shell should carry belongs in that shell's own rc.
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
    // A worker's task file: that worker's task, and no descendant's.
    ENV_TASK_FILE,
    // The editor hint: each instance re-derives it for its own tabs.
    ENV_DEFAULT_EDITOR,
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
            // The Gemini CLI drift canary's own variable is aterm's, not the CLI's.
            "ATERM_GEMINI_BIN",
        ] {
            assert!(!is_ai_env_var(v), "{v} must reach the child");
        }
    }
}
