// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pure lifecycle decisions for the introspection control socket.
//!
//! The control socket grants full power over a live terminal, and every
//! running instance binds its own socket, so three decisions must be exactly
//! right and exactly shared between the server (`aterm-gui`) and the client
//! (`aterm-ctl`):
//!
//! 1. **Whether to bind at all.** `ATERM_CONTROL_SOCK=0` / `=off` (or
//!    `ATERM_NO_CONTROL_SOCK=1`) disables the socket entirely.
//! 2. **Per-instance naming.** Each instance owns `aterm-<pid>.sock` plus a
//!    matching `aterm-<pid>.token`, so a second instance never hijacks the
//!    first one's socket; a `aterm.sock` symlink points at the newest
//!    instance so single-instance usage is unchanged.
//! 3. **Stale-file tolerance.** A crashed instance leaves its files behind;
//!    they are removable exactly when their embedded pid is dead.
//!
//! Hosts read the environment / directory / `kill(pid, 0)` and pass the
//! results in; the decisions themselves stay platform-free and testable.
//!
//! Platform note: on Windows the `latest` alias ([`LATEST_SOCK_FILE`]) is a
//! regular POINTER FILE (NTFS symlinks need privilege/dev-mode) whose
//! contents are the SAME relative instance name a Unix `readlink` returns
//! (`aterm-<pid>.sock`), so [`symlink_targets_pid`], [`token_name_for_sock`],
//! and [`instance_pid`] apply to those contents unchanged — every pure
//! function here is reused as-is on both platforms (the alias mechanics live
//! in `aterm-uds::latest`).

use std::path::Path;

/// Filename of the `latest` symlink in the socket directory: points at the
/// newest instance's `aterm-<pid>.sock`, so clients with no flags reach it.
pub const LATEST_SOCK_FILE: &str = "aterm.sock";

/// Token filename used beside a socket that is NOT per-instance (an explicit
/// `$ATERM_CONTROL_SOCK` path override).
pub const SIBLING_TOKEN_FILE: &str = "aterm.token";

/// What the host should do about the control socket, decided from the
/// environment by [`socket_directive`].
#[derive(Debug, PartialEq, Eq)]
pub enum SocketDirective {
    /// Bind the per-instance default (`aterm-<pid>.sock` in the per-user dir)
    /// and maintain the `latest` symlink.
    PerInstance,
    /// Bind exactly this caller-supplied path; no symlink is maintained.
    Explicit(String),
    /// Do not bind a control socket at all.
    Disabled,
}

/// Decide the socket disposition from the values of `$ATERM_CONTROL_SOCK` and
/// `$ATERM_NO_CONTROL_SOCK` (`None` = unset).
///
/// `ATERM_CONTROL_SOCK=0` or `=off` (case-insensitive) disables the socket,
/// as does `ATERM_NO_CONTROL_SOCK` set to anything but `0`/empty. Any other
/// non-empty `ATERM_CONTROL_SOCK` value is an explicit path override; unset
/// or empty means the per-instance default.
#[must_use]
pub fn socket_directive(
    control_sock: Option<&str>,
    no_control_sock: Option<&str>,
) -> SocketDirective {
    if no_control_sock.is_some_and(|v| !v.is_empty() && v != "0") {
        return SocketDirective::Disabled;
    }
    match control_sock {
        Some(v) if v == "0" || v.eq_ignore_ascii_case("off") => SocketDirective::Disabled,
        Some("") | None => SocketDirective::PerInstance,
        Some(v) => SocketDirective::Explicit(v.to_string()),
    }
}

/// The per-instance socket filename for `pid`: `aterm-<pid>.sock`. Also the
/// `latest` symlink's target — relative, so the link stays valid through any
/// path the directory is reached by.
#[must_use]
pub fn instance_sock_name(pid: u32) -> String {
    // Trust gate: concatenation instead of `format!` — runtime-argument
    // `format_args!` cannot be lowered natively. Byte-identical output.
    let mut name = String::from("aterm-");
    name.push_str(&pid.to_string());
    name.push_str(".sock");
    name
}

/// The per-instance token filename for `pid`: `aterm-<pid>.token`.
#[must_use]
pub fn instance_token_name(pid: u32) -> String {
    // Trust gate: see `instance_sock_name`.
    let mut name = String::from("aterm-");
    name.push_str(&pid.to_string());
    name.push_str(".token");
    name
}

/// Parse the owning pid out of a per-instance filename (`aterm-<pid>.sock` or
/// `aterm-<pid>.token`). `None` for anything else — notably the fixed
/// [`LATEST_SOCK_FILE`] / [`SIBLING_TOKEN_FILE`] names, which must never be
/// treated as instance-owned.
#[must_use]
// Skip: `str::parse` absent std body; fail-closed.
#[cfg_attr(trust_verify, trust::skip)]
pub fn instance_pid(name: &str) -> Option<u32> {
    let stem = name
        .strip_suffix(".sock")
        .or_else(|| name.strip_suffix(".token"))?;
    let pid = stem.strip_prefix("aterm-")?;
    // Digits only: keep `u32::parse`'s `+` tolerance from matching odd names.
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok()
}

/// The token filename that authenticates the socket named `sock_name`: a
/// per-instance socket pairs with its per-instance token, anything else falls
/// back to the sibling [`SIBLING_TOKEN_FILE`].
#[must_use]
pub fn token_name_for_sock(sock_name: &str) -> String {
    match instance_pid(sock_name) {
        Some(pid) => instance_token_name(pid),
        None => SIBLING_TOKEN_FILE.to_string(),
    }
}

/// The `sock <abs-path>` line of a discovery graph entry (`<dir>/graph/<sid>`,
/// written by the server, read by the server's sibling forward AND by
/// `aterm-ctl`'s in-session self-location). ONE parser for the on-disk format
/// so the two ends can never drift; the nonce line is server-only and parsed
/// beside this. `None` for a missing/empty `sock` line.
#[must_use]
pub fn graph_entry_sock(body: &str) -> Option<String> {
    body.lines()
        .find_map(|l| l.strip_prefix("sock "))
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
}

/// The `pid <n>` line of a discovery graph entry — the pid of the HOSTING
/// instance that wrote the entry. Written so `aterm-ctl`'s `instances`/`ls`
/// discovery can report a pid even for an EXPLICIT-`$ATERM_CONTROL_SOCK`
/// instance whose socket filename does NOT encode one (`instance_pid` only
/// recovers a pid from the `aterm-<pid>.sock` naming). ONE parser, shared with
/// the writer, so the two ends can never drift. Additive: an older entry with
/// no `pid` line — or a malformed value — parses to `None`, and the caller
/// falls back to a `0` placeholder rather than dropping the instance.
#[must_use]
// Skip: `str::parse` absent std body; fail-closed like `instance_pid`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn graph_entry_pid(body: &str) -> Option<u32> {
    let raw = body.lines().find_map(|l| l.strip_prefix("pid "))?.trim();
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    raw.parse().ok()
}

/// From a socket-directory listing, the per-instance files whose owning pid
/// is dead — stale leftovers of crashed instances, safe to remove. Files of
/// live pids (including the caller's own) and non-instance names are kept.
#[must_use]
pub fn stale_instance_files(names: &[&str], pid_alive: &dyn Fn(u32) -> bool) -> Vec<String> {
    names
        .iter()
        .filter(|n| matches!(instance_pid(n), Some(pid) if !pid_alive(pid)))
        .map(|n| (*n).to_string())
        .collect()
}

/// Whether a `latest` symlink target (relative or absolute) designates the
/// instance socket of `pid` — i.e. the link belongs to that instance and may
/// be removed on its exit.
#[must_use]
pub fn symlink_targets_pid(target: &str, pid: u32) -> bool {
    Path::new(target)
        .file_name()
        .is_some_and(|f| f == instance_sock_name(pid).as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive_disables_on_off_values() {
        assert_eq!(socket_directive(Some("0"), None), SocketDirective::Disabled);
        assert_eq!(
            socket_directive(Some("off"), None),
            SocketDirective::Disabled
        );
        assert_eq!(
            socket_directive(Some("OFF"), None),
            SocketDirective::Disabled
        );
        assert_eq!(socket_directive(None, Some("1")), SocketDirective::Disabled);
        assert_eq!(
            socket_directive(None, Some("yes")),
            SocketDirective::Disabled
        );
        // The kill switch wins even over an explicit path.
        assert_eq!(
            socket_directive(Some("/tmp/a.sock"), Some("1")),
            SocketDirective::Disabled
        );
    }

    #[test]
    fn directive_defaults_to_per_instance() {
        assert_eq!(socket_directive(None, None), SocketDirective::PerInstance);
        assert_eq!(
            socket_directive(Some(""), None),
            SocketDirective::PerInstance
        );
        // A non-disabling kill-switch value does not disable.
        assert_eq!(
            socket_directive(None, Some("0")),
            SocketDirective::PerInstance
        );
        assert_eq!(
            socket_directive(None, Some("")),
            SocketDirective::PerInstance
        );
    }

    #[test]
    fn directive_passes_explicit_path_through() {
        assert_eq!(
            socket_directive(Some("/tmp/a.sock"), None),
            SocketDirective::Explicit("/tmp/a.sock".to_string())
        );
        // `off` is only a keyword for the value itself, not for paths.
        assert_eq!(
            socket_directive(Some("/tmp/off"), None),
            SocketDirective::Explicit("/tmp/off".to_string())
        );
    }

    #[test]
    fn instance_names_roundtrip_through_pid_parse() {
        assert_eq!(instance_sock_name(42), "aterm-42.sock");
        assert_eq!(instance_token_name(42), "aterm-42.token");
        assert_eq!(instance_pid("aterm-42.sock"), Some(42));
        assert_eq!(instance_pid("aterm-42.token"), Some(42));
    }

    #[test]
    fn instance_pid_rejects_fixed_and_malformed_names() {
        assert_eq!(instance_pid(LATEST_SOCK_FILE), None);
        assert_eq!(instance_pid(SIBLING_TOKEN_FILE), None);
        assert_eq!(instance_pid("aterm-.sock"), None);
        assert_eq!(instance_pid("aterm-+5.sock"), None);
        assert_eq!(instance_pid("aterm-42.sock.tmp"), None);
        assert_eq!(instance_pid("other-42.sock"), None);
    }

    #[test]
    fn graph_entry_sock_parses_the_shared_on_disk_format() {
        assert_eq!(
            graph_entry_sock("sock /d/aterm-7.sock\nnonce abcd\n").as_deref(),
            Some("/d/aterm-7.sock")
        );
        // Missing / empty sock lines fail closed; nonce alone is not enough.
        assert_eq!(graph_entry_sock("nonce abcd\n"), None);
        assert_eq!(graph_entry_sock("sock \n"), None);
        assert_eq!(graph_entry_sock(""), None);
        // Surrounding whitespace on the path is trimmed.
        assert_eq!(
            graph_entry_sock("sock   /a/b.sock  \n").as_deref(),
            Some("/a/b.sock")
        );
    }

    #[test]
    fn graph_entry_pid_parses_hosting_pid_and_fails_closed() {
        // A full server-written entry carries the hosting pid on its own line.
        assert_eq!(
            graph_entry_pid("sock /tmp/app.sock\nnonce abcd\npid 4242\n"),
            Some(4242)
        );
        // Additive: an OLDER entry with no `pid` line parses to `None` (the
        // discovery caller falls back to a 0 placeholder, never drops it).
        assert_eq!(graph_entry_pid("sock /tmp/app.sock\nnonce abcd\n"), None);
        // Malformed values fail closed (no `+`/whitespace/non-digit tolerance).
        assert_eq!(graph_entry_pid("pid \n"), None);
        assert_eq!(graph_entry_pid("pid 12x\n"), None);
        assert_eq!(graph_entry_pid("pid +7\n"), None);
    }

    #[test]
    fn token_choice_follows_symlink_target() {
        // The `latest` symlink resolves to an instance sock; its token pairs.
        assert_eq!(token_name_for_sock("aterm-7.sock"), "aterm-7.token");
        // Non-instance names (explicit overrides, legacy fixed name) fall back.
        assert_eq!(token_name_for_sock("aterm.sock"), SIBLING_TOKEN_FILE);
        assert_eq!(token_name_for_sock("a.sock"), SIBLING_TOKEN_FILE);
    }

    #[test]
    fn stale_sweep_removes_only_dead_instances() {
        let names = [
            "aterm-100.sock",
            "aterm-100.token",
            "aterm-200.sock",
            "aterm-200.token",
            "aterm.sock",
            "aterm.token",
            "images",
        ];
        let alive = |pid: u32| pid == 200;
        let stale = stale_instance_files(&names, &alive);
        assert_eq!(stale, vec!["aterm-100.sock", "aterm-100.token"]);
        // All pids alive: nothing to sweep.
        assert!(stale_instance_files(&names, &|_| true).is_empty());
    }

    #[test]
    fn symlink_ownership_matches_pid_in_target() {
        assert!(symlink_targets_pid("aterm-42.sock", 42));
        assert!(symlink_targets_pid(
            "/run/user/1000/aterm/aterm-42.sock",
            42
        ));
        assert!(!symlink_targets_pid("aterm-42.sock", 43));
        assert!(!symlink_targets_pid("aterm.sock", 42));
        assert!(!symlink_targets_pid("", 42));
    }
}
