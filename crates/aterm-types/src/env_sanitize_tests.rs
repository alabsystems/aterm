// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

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
        "ATERM_DEBUG_SEAMLESS_KEEP",
        "ATERM_DEBUG_RELAUNCH_NUDGE",
        "ATERM_DEBUG_STATUS_BARS",
        "ATERM_DEBUG_CLIPBOARD_FILE",
        "ATERM_DEBUG_LOST_SHELL_NONCE",
        "ATERM_DEBUG_TURN_KEY_CLOCK_STEP_MS",
        "ATERM_DEBUG_TURN_KEY_BARRIER",
        "ATERM_DEBUG_PROMPT_CLOCK",
        "ATERM_DEBUG_FLEET_CLOCK",
        "ATERM_DEBUG_ECHO_CLOCK",
        "ATERM_DEBUG_TYPING_CLOCK",
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
        ENV_DEBUG_HOST_SLEEP,
    ] {
        assert!(is_ai_env_var(v), "{v} must be deny-listed for inheritance");
    }
}

/// Grok Build's and Kimi Code's seams and credentials stop at the hop like every other agent's:
/// a login one session runs on never leaks into a shell it opens. OpenAI's own key, which
/// no aterm seam sets, reaches the child as it always has.
#[test]
fn grok_and_kimi_seams_and_keys_are_denied_and_openais_key_is_not() {
    for v in [
        "GROK_AUTH_PATH",
        "GROK_HOME",
        "GROK_AUTH",
        "XAI_API_KEY",
        "KIMI_CODE_HOME",
        "KIMI_API_KEY",
        "MOONSHOT_API_KEY",
    ] {
        assert!(is_ai_env_var(v), "{v} must be deny-listed for inheritance");
        assert!(is_ai_env_key(std::ffi::OsStr::new(v)), "{v}");
    }
    for v in [
        "OPENAI_API_KEY",
        "GROKKING",
        "KIMIKO",
        "XAIDEN",
        "MY_GROK_AUTH_PATH",
    ] {
        assert!(!is_ai_env_var(v), "{v} must reach the child");
    }
    assert!(is_ai_env_var("CLAUDE_SECURESTORAGE_CONFIG_DIR"));
    assert!(is_ai_env_var("CODEX_HOME"));
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

/// Only the bare default word under an inherited `ATERM_CHILD` is an outer aterm's `EDITOR`.
/// NEGATIVE CONTROL: the same word with no (or an empty) `ATERM_CHILD`, and any other
/// editor under one, are the person's.
#[test]
fn only_the_default_word_under_an_aterm_tab_is_handed_down() {
    let os = std::ffi::OsStr::new;
    assert!(is_handed_down_editor(os("amacs"), Some(os("1"))));
    assert!(!is_handed_down_editor(os("amacs"), None));
    assert!(!is_handed_down_editor(os("amacs"), Some(os(""))));
    for own in ["vi", "amacs --wait", "/usr/local/bin/amacs", "Amacs", ""] {
        assert!(!is_handed_down_editor(os(own), Some(os("1"))), "{own:?}");
    }
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
