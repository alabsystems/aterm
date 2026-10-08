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
// Recursion-provisioning env vars: the contract by which a launching
// aterm hands a child its fabric identity + per-op capability edges, so an outer
// agent automatically holds read/write/signal authority over the inner session
// it spawned. ALL of these are deny-listed (below) so an INHERITED copy never
// transitively leaks past one hop; each direct child gets a FRESH set re-injected
// via `env_add` (which `build_child_env` applies on top of the stripped inherited
// env).
// ---------------------------------------------------------------------------

/// The child adopts this as its ROOT session id (`s-<20hex>`), so the outer's
/// preminted edges (which name it as `dst`) authorize against the child's table.
pub const ENV_SESSION_ID: &str = "ATERM_SESSION_ID";
/// The child adopts this as its ROOT launch nonce (`<32hex>`); the parent's
/// preminted edges bind to it, so a connection presenting a stale edge token must
/// match this nonce to authorize.
///
/// CAVEAT: on the recursion path this nonce is PINNED, not fresh: a child
/// re-exec'd in the same shell adopts the identical nonce, so the cross-relaunch
/// protection `LaunchNonce` describes does NOT hold here. The same-uid boundary
/// and edge-token secrecy bound authority; the nonce is a binding key, not a
/// relaunch guard.
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
/// Path to the 0600 file holding the parent→child edge-token SECRETS. Only this
/// non-secret PATH is in env: a same-uid peer that cannot read 0600 files (a
/// sandboxed confused deputy) cannot open it. File format: three lines
/// `read <64hex>` / `write <64hex>` / `signal <64hex>`.
///
/// LIFECYCLE: the file PERSISTS for the parent session; the child reads it
/// non-destructively and never deletes it, because a child re-exec'd in the same
/// shell re-inherits this path and must re-read the secrets to re-install the
/// parent edges (a consume-once delete made the outer's `@child` proxy answer
/// `ERR auth` after the first inner exited). It lives as long as, and beside,
/// the per-launch auth token file (`aterm-<pid>.token`, 0600 in a 0700 dir). The
/// PARENT removes it; a crash leftover is inert (its tokens bind a random
/// `(sid, nonce)` never reissued).
pub const ENV_EDGE_TOKENS: &str = "ATERM_EDGE_TOKENS";
/// The RETIRED env channel for a `ReadScreen` `EdgeToken` (`<64hex>`). Nothing
/// reads it (the secrets travel only in the [`ENV_EDGE_TOKENS`] file), but a
/// value an older aterm injected is still a bearer secret, so the name stays
/// deny-listed.
pub const ENV_EDGE_READ: &str = "ATERM_EDGE_READ";
/// The retired env channel for a `WriteInput` `EdgeToken`; see [`ENV_EDGE_READ`].
pub const ENV_EDGE_WRITE: &str = "ATERM_EDGE_WRITE";
/// The retired env channel for a `Signal` `EdgeToken`; see [`ENV_EDGE_READ`].
pub const ENV_EDGE_SIGNAL: &str = "ATERM_EDGE_SIGNAL";

/// The sid a CONTROLLER session was spawned to observe (session connections,
/// `SESSION_CONNECTIONS.md` §2.3/§6): the "New Controller Session" presets and
/// `spawn connected=controller of=<sid>` inject it so the supervisor's tooling
/// knows which session it holds a connection over. IDENTITY ONLY — never a
/// token (design §1.4#3): authority stays in the origin's `EdgeTable`, held by
/// the spawning process's `ConnectionRecord` store. Deny-listed (below) so the
/// hint never leaks past one hop — a grandchild is not the controller.
pub const ENV_OBSERVE_SESSION_ID: &str = "ATERM_OBSERVE_SESSION_ID";

/// The shell program THIS TAB runs, as the window resolved it for the spawn
/// (Windows: an absolute path, e.g. `C:\Windows\system32\cmd.exe`), handed to
/// every shell tab the window spawns so a CLI typed in the tab can report the
/// tab's own shell: `aterm doctor` / `show-config` cannot see the window's
/// per-launch `--shell` flag (under `--shell cmd` both named pwsh, a new
/// window's default). Deny-listed (below) so it describes
/// only the tab it was handed to: a `-e` session gets none, and an inherited
/// copy never survives a hop into a nested window's tabs.
pub const ENV_TAB_SHELL: &str = "ATERM_TAB_SHELL";

/// Exact env vars that should not leak into child shells.
///
/// These are denied by exact name because other `ATERM_*` variables are
/// required for shell integration inside the child shell. The
/// recursion-provisioning identity/edge vars are denied so they are never
/// INHERITED across a hop (each direct child is re-injected a fresh set; see the
/// consts above and `build_child_env`), and the retired edge-secret names so a
/// value an older aterm injected stops at the hop.
pub const ENV_DENY_VARS: &[&str] = &[
    // The update DEVELOPMENT SEAMS (`dev_seam!`): never inherit into child SHELLS,
    // for the same one-hop reason as the provisioning vars below — an aterm launched from an
    // aterm shell makes its own update decisions, never under a QA seam the parent's
    // environment happened to carry (an inherited-but-empty selector once rerouted a
    // daily driver's updater for its whole lifetime). Only a dev build reads these.
    "ATERM_DEBUG_SEAMLESS_REEXEC",
    "ATERM_DEBUG_RELAUNCH_NUDGE",
    "ATERM_DEBUG_STATUS_BARS",
    // The strain row's fake saturated reading: a nested aterm never inherits a
    // demo's fake load.
    "ATERM_DEBUG_STRAIN",
    // The Paste gesture's file-fed clipboard (lib.rs `debug_paste_text`), for live
    // checks of a large paste without the owner's clipboard.
    "ATERM_DEBUG_PASTE_FILE",
    // Every clipboard read and write goes to this file (clipboard.rs `route`), not the OS.
    "ATERM_DEBUG_CLIPBOARD_FILE",
    // A fresh shell spawned as a lost-nonce adopted one (spawn.rs
    // `debug_lost_shell_nonce`): a nested aterm's shells are its own.
    "ATERM_DEBUG_LOST_SHELL_NONCE",
    "ATERM_UPDATE_ROOT",
    "ATERM_HANDOFF_READY_TIMEOUT_MS",
    "ATERM_HANDOFF_PROOF_TIMEOUT_MS",
    // The render epoch's class seam (resize_epoch.rs `EpochBook::class`): the lane's
    // stand-in reads as the agent class, so a nested aterm never inherits it.
    "ATERM_NUDGE_CLASS",
    // A launched successor that dies before its rendezvous dial
    // (handoff_rendezvous.rs `debug_die_before_dial`): a nested aterm's update
    // applies are its own, never ones whose successors die by its parent's seam.
    // The successor itself still receives it: LaunchServices merges the launching
    // process's environment, and no child shell stands between them.
    "ATERM_DEBUG_DIE_BEFORE_DIAL",
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
    // The RETIRED edge-secret env channel: still bearer secrets (see ENV_EDGE_READ).
    ENV_EDGE_READ,
    ENV_EDGE_WRITE,
    ENV_EDGE_SIGNAL,
    // Controller-spawn observation hint (session connections): one hop only —
    // a descendant that did not receive it fresh is not the controller.
    ENV_OBSERVE_SESSION_ID,
    // The tab's own shell: true of the tab it was handed to, and of no other.
    ENV_TAB_SHELL,
    // FABRIC credentials (design §11.2). The bridge's broker endpoint, its
    // capability FILE and its fleet name are the node's identity on the bus: a
    // nested aterm that inherited them could publish AS the outer node, under the
    // outer node's cap, which is the confused deputy the whole `via=`/relay
    // discipline exists to avoid. An inner aterm becomes its own node only when a
    // human mints it a cap out of band — never by inheritance, and never by env.
    "ATERM_LINK_BROKER",
    "ATERM_LINK_CAP_FILE",
    "ATERM_LINK_FLEET",
    // And the fabric LAUNCH knob: an inner aterm that inherited it would start a
    // second bridge from the OUTER node's command line, cap file and state dir,
    // publishing as the outer node over the inner's sessions.
    //
    // THIS CLOSES THE ENV ROUTE ONLY. `fabric_launch::configured_command` falls
    // back to `[fabric] command` in the per-user config, which a nested aterm
    // reads identically, and `StateDir::open` takes no lock that would refuse a
    // second bridge. Closing that route needs a launcher gate (spawn only from a
    // ROOT instance) or an exclusive state-dir lock; neither exists yet.
    "ATERM_FABRIC_COMMAND",
];

/// Returns `true` if `key` matches a deny-listed AI or provisioning env var.
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

/// [`is_ai_env_var`] for a key as the OS hands it over — the ONE classifier the
/// spawn seams apply to an inherited environment (`aterm-pty`'s
/// `build_child_env`, `aterm-gui`'s fabric bridge child).
///
/// A key that is not valid Unicode is classified by its LOSSY spelling, never
/// let through unread. Every exact deny-listed name is ASCII and U+FFFD never
/// equals one; but the [`ENV_DENY_PREFIXES`] match a key's leading bytes, and a
/// lossy conversion keeps every ASCII byte, so `ANTHROPIC_\xffX` keeps its
/// `ANTHROPIC_` and is denied.
///
/// Windows environment names are case-insensitive, so there the key is
/// ASCII-uppercased first (every deny-listed name and prefix is uppercase
/// ASCII) — otherwise `anthropic_api_key` would reach the child. Unix names are
/// case-sensitive and keep the exact-case check.
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

    /// A non-UTF-8 key is classified by its lossy spelling: a deny-listed
    /// PREFIX survives the conversion (ASCII bytes are kept), an exact name
    /// cannot be forged by one (U+FFFD is never ASCII), and a key with no
    /// deny-listed prefix passes.
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

    /// The OSC 133/633 capability nonce must survive sanitization so the
    /// shell-integration preamble can emit
    /// `id=<hex>` on every 133/633 sequence.
    #[test]
    fn test_aterm_shell_nonce_survives_sanitization() {
        assert!(!is_ai_env_var("ATERM_SHELL_NONCE"));
    }

    /// The reroute seam's two variables (`atpkg::reroute`) must reach EVERY
    /// child: `ATERM_REROUTE_DIR` is re-asserted first on PATH after the rc files
    /// ran; `__ATERM_REROUTE_PASSTHROUGH` is `aterm --no-reroute`'s marker, which
    /// an upstream `cargo` hands to its own `rustc`/`rustdoc` spawns. The update
    /// seams ARE denied by name, so this pin keeps the `ATERM_*` family from being
    /// deny-listed by prefix, which would strip `--no-reroute` from its children.
    #[test]
    fn test_reroute_seam_vars_survive_sanitization() {
        assert!(!is_ai_env_var("__ATERM_REROUTE_PASSTHROUGH"));
        assert!(!is_ai_env_var("ATERM_REROUTE_DIR"));
    }

    /// The recursion-provisioning identity/edge vars (and the retired
    /// edge-secret names) are denied by exact name, so an INHERITED copy never
    /// leaks past one hop. `ATERM_EDGE_TOKENS` stays denied although its file
    /// persists for the session: only a same-shell relaunch re-reads it.
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
        // Shell-integration ATERM_* vars are still preserved (not over-broad).
        assert!(!is_ai_env_var("ATERM_SHELL_INTEGRATION_DIR"));
    }

    /// The development seams (`dev_seam!`) and the retired credentials are
    /// denied by exact name: a nested aterm never runs under its parent's QA seam,
    /// and a token an old export still carries never reaches a tab.
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
            "ATERM_NUDGE_CLASS",
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
