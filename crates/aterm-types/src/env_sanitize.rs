// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Environment variable sanitization for PTY spawn paths.
//!
//! AI development tools (AI Assistant, Copilot, Cursor, etc.) set env vars
//! that are meaningless — and potentially confusing — inside user terminal
//! sessions. This module contains the canonical deny-prefix list used by
//! all PTY spawn paths (Swift, Rust aterm-pty, aterm-core, Alacritty bridge),
//! and public callers reach it through `aterm_types::domain`.
//!
//! Part of #5400.

/// Prefixes for environment variables that should not leak into child shells.
///
/// All PTY spawn paths must filter these before exec.
pub const ENV_DENY_PREFIXES: &[&str] = &[
    "CLAUDE",     // CLAUDECODE, CLAUDE_CODE_*, CLAUDE_*
    "ANTHROPIC_", // ANTHROPIC_MODEL, ANTHROPIC_API_KEY, etc.
    "COPILOT_",   // GitHub Copilot
    "CODEX_",     // OpenAI Codex
    "CURSOR_",    // Cursor editor
    "AI_",        // AI development tool infrastructure vars
    "_DEVTOOL_",  // Internal development tool runtime vars
];

// ---------------------------------------------------------------------------
// Recursion-provisioning env vars (Item 4): the contract by which a launching
// aterm hands a child its fabric identity + per-op capability edges, so an outer
// agent automatically holds read/write/signal authority over the inner session
// it spawned. ALL of these are deny-listed (below) so an INHERITED copy never
// transitively leaks past one hop; each direct child gets a FRESH set re-injected
// via `env_add` (which `build_child_env` applies on top of the stripped inherited
// env). The control-socket vars are deny-listed too so a child never inherits —
// and thus never hijacks — the parent's explicit socket path.
// ---------------------------------------------------------------------------

/// The child adopts this as its ROOT session id (`s-<20hex>`), so the outer's
/// preminted edges (which name it as `dst`) authorize against the child's table.
pub const ENV_SESSION_ID: &str = "ATERM_SESSION_ID";
/// The child adopts this as its ROOT launch nonce (`<32hex>`); the parent's
/// preminted edges bind to it, so a connection presenting a stale edge token must
/// match this nonce to authorize.
///
/// CAVEAT (honest scope — see audit finding F2): on the RECURSION path this nonce
/// is PINNED, not fresh. The child adopts the injected constant, so a child that
/// exits and is re-exec'd in the SAME shell (re-inheriting this env) adopts the
/// IDENTICAL nonce — the cross-relaunch protection the bare `LaunchNonce` doc
/// describes does NOT hold here. The same-uid trust boundary + edge-token secrecy
/// are what bound authority on this path; the nonce is a binding key, not a
/// relaunch guard. (A true relaunch guard would require the child to mint a FRESH
/// nonce at adopt time and re-handshake it to the parent.)
pub const ENV_LAUNCH_NONCE: &str = "ATERM_LAUNCH_NONCE";
/// The parent session id (`s-<20hex>`) — becomes the `src` of the child's edges.
pub const ENV_PARENT_SESSION_ID: &str = "ATERM_PARENT_SESSION_ID";
/// The multiplexer signature this session was BORN into (`"<$TMUX>|<$STY>"`),
/// stamped by the spawn seam so `aterm ctl` can tell a session it is speaking
/// from a tmux/screen PANE — where flagless verbs would otherwise drive the
/// outer terminal — from one that merely inherited a pane's environment.
/// Deny-listed like the other provisioning vars: an inherited copy must never
/// survive a hop, or a fresh session would answer for its parent's birth.
pub const ENV_MUX_BASE: &str = "ATERM_MUX_BASE";
/// Path to the 0600 file holding the parent→child edge-token SECRETS (audit
/// finding F1). The bearer tokens are NOT placed in env — only this PATH is, which
/// is non-secret: a same-uid peer that cannot read 0600 files (a sandboxed
/// confused-deputy) cannot open it, restoring the same-uid/0600-file trust
/// boundary that env-inherited tokens would have defeated. File format: three lines
/// `read <64hex>` / `write <64hex>` / `signal <64hex>`.
///
/// LIFECYCLE (F1, revised): the file PERSISTS for the parent session — the child
/// reads it NON-destructively at startup and does NOT delete it. This is required
/// for the SAME-SHELL relaunch: this var is deny-listed (below), so it is never
/// INHERITED across a new aterm hop, but a child aterm that exits and is re-exec'd
/// in the SAME shell re-inherits this PINNED path and must re-read the same secrets
/// to re-install the parent edges. A consume-once delete broke every such relaunch
/// (the outer's `@child` proxy answered `ERR auth` after the first inner exited).
/// The secret now lives on disk for the parent's session lifetime — the SAME window
/// as the per-launch AUTH token file (`aterm-<pid>.token`), also 0600 in the same
/// 0700 same-uid dir — so the trust boundary (same-uid + 0600) is unchanged. The
/// PARENT owns removal (on child/session teardown); a crash leftover is inert (its
/// tokens bind a random `(sid, nonce)` never reissued, so it authorizes nothing).
/// Deny-listing keeps cross-hop inheritance stripped; only the same-shell relaunch
/// re-reads it.
pub const ENV_EDGE_TOKENS: &str = "ATERM_EDGE_TOKENS";
/// A `ReadScreen` `EdgeToken` (`<64hex>`), parent → child. FALLBACK env channel
/// used only when no private socket dir exists for the [`ENV_EDGE_TOKENS`] file
/// (then the tokens are env-visible, with the documented same-uid caveat).
pub const ENV_EDGE_READ: &str = "ATERM_EDGE_READ";
/// A `WriteInput` `EdgeToken` (`<64hex>`), parent → child. Fallback env channel.
pub const ENV_EDGE_WRITE: &str = "ATERM_EDGE_WRITE";
/// A `Signal` `EdgeToken` (`<64hex>`), parent → child. Fallback env channel.
pub const ENV_EDGE_SIGNAL: &str = "ATERM_EDGE_SIGNAL";

/// The sid a CONTROLLER session was spawned to observe (session connections,
/// `SESSION_CONNECTIONS.md` §2.3/§6): the "New Controller Session" presets and
/// `spawn connected=controller of=<sid>` inject it so the supervisor's tooling
/// knows which session it holds a connection over. IDENTITY ONLY — never a
/// token (design §1.4#3): authority stays in the origin's `EdgeTable`, held by
/// the spawning process's `ConnectionRecord` store. Deny-listed (below) so the
/// hint never leaks past one hop — a grandchild is not the controller.
pub const ENV_OBSERVE_SESSION_ID: &str = "ATERM_OBSERVE_SESSION_ID";

// ---------------------------------------------------------------------------
// L3 network-drive selectors (aterm-gui `net_listen`): the bind address + the
// operator's TLS cert/key PATHS that opt a ROOT instance into a network control
// endpoint. ALL deny-listed so a nested aterm never (a) inherits the address and
// stands up a SECOND network-reachable Owner-control surface, nor (b) fans the
// operator's private-key path into every descendant. Only a top-level process
// the operator explicitly configured ever sees them.
// ---------------------------------------------------------------------------

/// The network-drive listener bind address (e.g. `0.0.0.0:7100`). Deny-listed.
pub const ENV_NET_LISTEN: &str = "ATERM_NET_LISTEN";
/// Path to the operator's server certificate (DER) for the network listener.
pub const ENV_NET_CERT: &str = "ATERM_NET_CERT";
/// Path to the operator's server private key (PKCS#8 DER) for the listener.
pub const ENV_NET_KEY: &str = "ATERM_NET_KEY";

/// Exact env vars that should not leak into child shells.
///
/// These are denied by exact name because other `ATERM_*` variables are
/// required for shell integration inside the child shell. Beyond the containment
/// vars, the recursion-provisioning identity/edge vars and the control-socket
/// selectors are denied so they are never INHERITED across a hop (each direct
/// child is re-injected a fresh set; see the consts above and `build_child_env`).
pub const ENV_DENY_VARS: &[&str] = &[
    "ATERM_CONTAINMENT_MODE",
    // (There is no `ATERM_CONTAINMENT_ALLOWLIST`. It was deny-listed here and
    // advertised in aterm-gui(1) as a containment knob, but no parser, field, or
    // env read for it has ever existed in `aterm-containment` — the allowlist is
    // loaded from TOML via `AllowlistConfig`, never from the environment.
    // Denying a name nothing reads defends nothing and documented a knob users
    // could not use.)
    // Control-socket selectors: never inherit, so a nested aterm rebinds its OWN
    // per-instance socket and never unlinks/steals the parent's explicit path.
    "ATERM_CONTROL_SOCK",
    "ATERM_NO_CONTROL_SOCK",
    // Network-drive selectors: never inherit, so a nested aterm cannot open a
    // second network control surface and the operator's key path is not fanned
    // into every descendant (only the explicitly-configured root binds).
    ENV_NET_LISTEN,
    ENV_NET_CERT,
    ENV_NET_KEY,
    // Recursion provisioning (re-injected fresh per direct child via env_add).
    ENV_SESSION_ID,
    ENV_LAUNCH_NONCE,
    ENV_PARENT_SESSION_ID,
    ENV_MUX_BASE,
    ENV_EDGE_TOKENS,
    ENV_EDGE_READ,
    ENV_EDGE_WRITE,
    ENV_EDGE_SIGNAL,
    // Controller-spawn observation hint (session connections): one hop only —
    // a descendant that did not receive it fresh is not the controller.
    ENV_OBSERVE_SESSION_ID,
];

/// Returns `true` if `key` matches a deny-listed AI or containment env var.
#[must_use]
// #[inline] so the MIR crosses the crate boundary: callers' Trust gates
// (aterm-pty) bundle and VERIFY this body instead of assuming an absent
// callee. Semantics unchanged.
#[inline]
pub fn is_ai_env_var(key: &str) -> bool {
    // Explicit loops (not `slice::contains` / `Iterator::any`): both dispatch
    // element comparisons through absent std trait bodies; the loops compare
    // the same bytes in the same order — behavior-identical.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_ai_env_var_matches_deny_prefixes() {
        assert!(is_ai_env_var("CLAUDECODE"));
        assert!(is_ai_env_var("CLAUDE_CODE_ENTRYPOINT"));
        assert!(is_ai_env_var("CLAUDE_API_KEY"));
        assert!(is_ai_env_var("ANTHROPIC_MODEL"));
        assert!(is_ai_env_var("COPILOT_TOKEN"));
        assert!(is_ai_env_var("CODEX_SESSION"));
        assert!(is_ai_env_var("CURSOR_SETTINGS"));
        let ai_role = ["AI", "ROLE"].join("_");
        let ai_worker_id = ["AI", "WORKER", "ID"].join("_");
        assert!(is_ai_env_var(&ai_role));
        assert!(is_ai_env_var(&ai_worker_id));
        assert!(is_ai_env_var("_DEVTOOL_CARGO_LOCK"));
    }

    #[test]
    fn test_is_ai_env_var_strips_containment_vars_but_preserves_shell_integration_vars() {
        assert!(is_ai_env_var("ATERM_CONTAINMENT_MODE"));
        assert!(!is_ai_env_var("ATERM_SHELL_INTEGRATION_DIR"));
        assert!(!is_ai_env_var("ATERM_ORIGINAL_ZDOTDIR"));
        assert!(!is_ai_env_var("ATERM_UNSET_ZDOTDIR"));
    }

    /// OSC 133/633 capability nonce (#7937 F01-2, #7960, #8006) must survive
    /// environment sanitization so the shell-integration preamble can emit
    /// `id=<hex>` on every 133/633 sequence.
    #[test]
    fn test_aterm_shell_nonce_survives_sanitization() {
        assert!(!is_ai_env_var("ATERM_SHELL_NONCE"));
    }

    /// Item 4/5: the recursion-provisioning identity/edge vars and the
    /// control-socket selectors are denied by exact name, so an INHERITED copy
    /// never leaks past one hop (each direct child is re-injected a fresh set).
    #[test]
    fn test_recursion_provisioning_vars_are_denied_by_name() {
        for v in [
            "ATERM_CONTROL_SOCK",
            "ATERM_NO_CONTROL_SOCK",
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
        // Shell-integration ATERM_* vars are still preserved (not over-broad).
        assert!(!is_ai_env_var("ATERM_SHELL_INTEGRATION_DIR"));
    }

    /// L3 network drive: the listener bind address + the operator's TLS cert/key
    /// PATHS must be stripped on every child hop, so a nested aterm can neither
    /// open a second network control surface nor inherit the operator's key path.
    #[test]
    fn test_network_drive_selectors_are_denied_by_name() {
        for v in [ENV_NET_LISTEN, ENV_NET_CERT, ENV_NET_KEY] {
            assert!(
                is_ai_env_var(v),
                "{v} must be deny-listed so children never inherit it"
            );
        }
    }

    /// F1 (revised): the edge-token file now PERSISTS for the session so a child
    /// re-launched in the SAME shell can re-read it. That MUST NOT relax the
    /// inheritance strip: `ATERM_EDGE_TOKENS` stays deny-listed so a NEW aterm hop
    /// never inherits the path — only a same-shell relaunch (which re-inherits the
    /// pinned var because no new aterm sanitized it) re-reads it.
    #[test]
    fn test_edge_tokens_path_still_stripped_on_inheritance() {
        assert!(
            is_ai_env_var(ENV_EDGE_TOKENS),
            "ATERM_EDGE_TOKENS must stay deny-listed even though the file persists \
             for the session (cross-hop inheritance must still be stripped)"
        );
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
