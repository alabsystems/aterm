// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The single PTY spawn + IO seam (ATERM_DESIGN WS-G).
//!
//! Every raw PTY primitive — Unix `forkpty`, `execve`, `read`, `write`,
//! `ioctl(TIOCSWINSZ)`; Windows ConPTY (`CreatePseudoConsole` + anonymous
//! pipes) — is contained HERE, in one auditable crate, so the frontend holds no
//! unsafe PTY code and there is exactly one place where a child process is
//! spawned. The master is returned as a raw `i32` because aterm's frontend
//! shares it across the input, reader, and control-socket threads (the same
//! sharing it already did); the unsafe is what moves, not the ownership model.
//!
//! Platform split (module split, no inline cfg): `src/unix.rs` is the shipping
//! POSIX seam moved verbatim; `src/windows/` is the ConPTY backend. On Unix the
//! `i32` IS the PTY master fd; on Windows it is an opaque, always-`>= 0` key
//! into a process-global session registry that owns the real HANDLEs. Every
//! public signature is identical on both platforms; Windows adds only
//! [`OwnedMaster`]/[`close_master`]/[`exit_code`] for the ownership + exit
//! status seams that `OwnedFd`/`waitpid` cover on Unix.

// Under the Trust verifier, register the `trust` tool namespace so the
// `#[cfg_attr(trust_verify, trust::skip)]` opt-out on `spawn_shell_with_pid`
// resolves; plain rustc never sets `trust_verify`, so this is inert off-Trust.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

/// A successfully spawned shell: the PTY master fd plus the child's pid. The
/// child is a session + process-group leader (`forkpty` -> `login_tty` ->
/// `setsid`), so `pid` doubles as the pgid: a `SIGHUP` to it (or `killpg`) tears
/// the whole job tree down. Exposed so the frontend can HANG UP the child first
/// — making the slave produce EOF so the reader's blocking `read(master)` returns
/// — BEFORE closing the master, instead of racing a blocked reader on the tty
/// lock (the macOS quit-hang: `close(master)` wedges in `lck_mtx_sleep` while the
/// reader sits in `read`).
///
/// Windows: `master` is the opaque ConPTY registry key (still `i32`, still
/// `>= 0`) and `pid` is the Windows process id; [`hangup`] closes the
/// pseudoconsole (the console-close analog of SIGHUP-on-controlling-tty).
#[derive(Debug, Clone, Copy)]
pub struct SpawnedShell {
    /// The PTY master fd (what `spawn_shell` returns on its own).
    pub master: i32,
    /// The child's pid == its process-group id (session leader via `login_tty`).
    pub pid: i32,
}

/// The UTF-8 locale aterm forces whenever it must guarantee UTF-8 character
/// encoding — the override [`resolve_spawn_locale`] injects for spawned children,
/// and (in aterm-gui) the locale the clipboard helper subprocesses (`pbcopy`/
/// `pbpaste`) are pinned to. `en_US.UTF-8` is guaranteed present on macOS. Kept
/// here as the single source of truth so the spawn-side and clipboard-side pins
/// cannot drift.
pub const UTF8_LOCALE: &str = "en_US.UTF-8";

/// Build the child shell's environment: the `inherited` environment with every
/// deny-listed key removed (AI-tool vars `CLAUDE*`/`ANTHROPIC_*`/`COPILOT_*`/… and
/// the containment vars `ATERM_CONTAINMENT_MODE`/`_ALLOWLIST`, via the canonical
/// [`aterm_types::domain::is_ai_env_var`]), then `env_add` applied on top —
/// overriding an existing key or appending a new one. So a deny-listed var present
/// in aterm's own environment never leaks into the spawned shell, while explicitly
/// injected vars (TERM, shell integration) are always preserved.
///
/// Called by [`spawn_shell_with_pid`] in the PARENT (before `forkpty`), so it stays
/// async-signal-safe (no child-side allocation). Non-UTF-8 keys bypass the
/// deny-list check, which is safe because every deny-listed name is ASCII. Pure in
/// its inputs so the wiring is unit-tested without mutating the process-global env
/// (the same approach `classify_write_result` uses for `write_all`'s branch ladder).
/// Shared verbatim by BOTH platform spawns (the Windows env-block builder applies
/// its case-insensitive dedupe on top of this exact output). The deny check itself
/// is case-insensitive on Windows only — see [`is_denied_env_key`].
// Skip: the `impl Iterator` parameter is CALLER-CHOSEN code — `next` is an
// open-trait dispatch on a type parameter (the genuinely-fatal class; the
// spawn call sites pass std::env::vars_os, but the signature admits any
// iterator). Deny-list logic is unit-tested; verify-only.
#[cfg_attr(trust_verify, trust::skip)]
fn build_child_env(
    inherited: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    env_add: &[(String, String)],
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    // Explicit loops instead of `filter(..).collect()` / `iter_mut().find(..)`:
    // the closure bodies those adapters lower to are absent callees for the Trust
    // panic-freedom verifier, leaving the obligations at the closure sites
    // unprovable. The loops below are the adapters' exact operational semantics —
    // `filter` keeps each passing element in encounter order (the push loop),
    // `find` yields the FIRST match and stops (the `break`), and the miss arm
    // appends — so the output Vec is element-for-element identical to the former
    // adapter pipeline on every input.
    let mut env_pairs: Vec<(std::ffi::OsString, std::ffi::OsString)> = Vec::new();
    for (k, v) in inherited {
        if !is_denied_env_key(&k) {
            env_pairs.push((k, v));
        }
    }
    for (k, v) in env_add {
        let key = std::ffi::OsString::from(k);
        let mut replaced = false;
        for pair in env_pairs.iter_mut() {
            if pair.0 == key {
                pair.1 = std::ffi::OsString::from(v);
                replaced = true;
                break;
            }
        }
        if !replaced {
            env_pairs.push((key, std::ffi::OsString::from(v)));
        }
    }
    env_pairs
}

/// Whether an inherited env key is deny-listed
/// ([`aterm_types::domain::is_ai_env_var`]). Windows env names are
/// case-insensitive, so there the key is ASCII-uppercased before the check
/// (every deny-listed name/prefix is uppercase ASCII) — otherwise a
/// non-canonical-case `anthropic_api_key` would leak into the child. Unix env
/// names are case-sensitive; the exact-case check stays.
fn is_denied_env_key(key: &std::ffi::OsStr) -> bool {
    let Some(k) = key.to_str() else {
        return false; // non-UTF-8 bypasses: every deny-listed name is ASCII
    };
    #[cfg(windows)]
    {
        aterm_types::domain::is_ai_env_var(&k.to_ascii_uppercase())
    }
    #[cfg(not(windows))]
    {
        aterm_types::domain::is_ai_env_var(k)
    }
}

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(test)]
mod tests {
    use super::*;

    // ---- env-sanitization seam wiring (the deny-list is actually APPLIED) ----
    //
    // REGRESSION: the deny-list CLASSIFIER (`is_ai_env_var`) is unit-tested in
    // aterm-types, but the PTY spawn seam never CALLED it — so AI-tool vars and the
    // containment vars leaked into every child shell. This proves `build_child_env`
    // (which `spawn_shell_with_pid` uses to build `envp`) drops the deny-listed keys
    // while keeping ordinary vars, and that `env_add` still overrides.
    #[test]
    fn build_child_env_drops_denylisted_and_keeps_overrides() {
        use std::ffi::OsString;
        let os = |s: &str| OsString::from(s);
        let inherited = vec![
            (os("PATH"), os("/usr/bin")),
            (os("ATERM_CONTAINMENT_MODE"), os("containment")),
            (os("ANTHROPIC_API_KEY"), os("secret")),
            (os("CLAUDECODE"), os("1")),
            (os("CURSOR_TRACE_ID"), os("xyz")),
            (os("TERM"), os("dumb")),
        ];
        let env_add = vec![("TERM".to_string(), "xterm-256color".to_string())];
        let out = build_child_env(inherited.into_iter(), &env_add);
        let keys: Vec<String> = out
            .iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        // Every deny-listed key (AI-tool + containment) is filtered out.
        for denied in [
            "ATERM_CONTAINMENT_MODE",
            "ANTHROPIC_API_KEY",
            "CLAUDECODE",
            "CURSOR_TRACE_ID",
        ] {
            assert!(
                !keys.contains(&denied.to_string()),
                "{denied} must be filtered from the child env, got {keys:?}"
            );
        }
        // Ordinary inherited key survives.
        assert!(
            keys.contains(&"PATH".to_string()),
            "PATH must pass through to the child"
        );
        // env_add OVERRIDES the inherited value (TERM was `dumb`, now the injected one),
        // and appears exactly once (no duplicate).
        let terms: Vec<&OsString> = out
            .iter()
            .filter(|(k, _)| k == &os("TERM"))
            .map(|(_, v)| v)
            .collect();
        assert_eq!(terms.len(), 1, "TERM must appear exactly once");
        assert_eq!(
            terms[0],
            &os("xterm-256color"),
            "env_add must override inherited TERM"
        );
    }

    // Windows env names are case-insensitive: a non-canonical-case copy of a
    // deny-listed var must be filtered too, or it leaks into the child.
    #[cfg(windows)]
    #[test]
    fn build_child_env_denies_mixed_case_keys_on_windows() {
        use std::ffi::OsString;
        let os = |s: &str| OsString::from(s);
        let inherited = vec![
            (os("Path"), os("C:\\Windows")),
            (os("anthropic_api_key"), os("secret")),
            (os("Claude_Code"), os("1")),
            (os("Aterm_Containment_Mode"), os("containment")),
            (os("cursor_trace_id"), os("xyz")),
        ];
        let out = build_child_env(inherited.into_iter(), &[]);
        let keys: Vec<String> = out
            .iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            keys,
            vec!["Path".to_string()],
            "every mixed-case deny-listed key must be filtered on Windows"
        );
    }

    // Unix env names ARE case-sensitive: the deny check stays exact-case there
    // (a lowercase `anthropic_api_key` is a DIFFERENT variable, not a leak).
    #[cfg(unix)]
    #[test]
    fn build_child_env_stays_case_sensitive_on_unix() {
        use std::ffi::OsString;
        let os = |s: &str| OsString::from(s);
        let inherited = vec![(os("anthropic_api_key"), os("distinct-var"))];
        let out = build_child_env(inherited.into_iter(), &[]);
        assert_eq!(out.len(), 1, "exact-case deny must not match on unix");
    }
}
