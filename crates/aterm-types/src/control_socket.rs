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
//! 2. **Per-socket naming.** Each instance owns `aterm-<pid>.sock` plus a
//!    matching `aterm-<pid>.token`, so a second instance never hijacks the
//!    first one's socket; a `aterm.sock` symlink points at the newest
//!    instance so single-instance usage is unchanged. An instance on an
//!    explicit `$ATERM_CONTROL_SOCK` path pairs the same way — its token is
//!    named after ITS socket ([`token_name_for_sock`]) — because the two
//!    private headless instances an agent boots in one scratch directory
//!    otherwise shared one token file and locked each other's clients out.
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

/// The LEGACY shared token filename: the ONE file every explicit-socket
/// instance in a directory used to write, and therefore to overwrite for each
/// other — the second private headless instance in a scratch directory took
/// the first one's credential with it, and every client of the first was
/// refused `ERR auth` while it was still listening (F9).
///
/// Nothing writes it any more; [`token_name_for_sock`] gives every socket a
/// token of its own. It survives as the LAST candidate a client reads
/// ([`token_names_for_sock`]) so a client from this build still authenticates
/// against an instance from a build that wrote it.
pub const SIBLING_TOKEN_FILE: &str = "aterm.token";

/// Prefix that keeps the token of an oddly-named socket out of the two
/// RESERVED names — [`SIBLING_TOKEN_FILE`] and the per-instance
/// `aterm-<pid>.token`. Appending `.token` to a socket named `aterm` yields
/// the first and to one named `aterm-4242` the second, so those names (any
/// name not ending in `.sock`) are prefixed instead. Without it the rule
/// stops being injective, which is the one property the whole fix rests on.
const EXPLICIT_TOKEN_PREFIX: &str = "aterm-sock-";

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
    if env_flag_engaged(no_control_sock) {
        return SocketDirective::Disabled;
    }
    match control_sock {
        Some(v) if v == "0" || v.eq_ignore_ascii_case("off") => SocketDirective::Disabled,
        Some("") | None => SocketDirective::PerInstance,
        Some(v) => SocketDirective::Explicit(v.to_string()),
    }
}

/// **THE ONE READING of a boolean `ATERM_NO_*` / veto env var** (`None` =
/// unset): engaged only by a value that is non-empty and not `"0"` — the rule
/// `$ATERM_NO_CONTROL_SOCK` has always used, promoted to a name so every
/// boolean flag shares it. Unset, EMPTY and `"0"` are "not engaged".
///
/// WHY THIS EXISTS: `var_os(..).is_some()` treats a present-but-empty variable
/// as engaged, and empty env vars travel — a shell exporting `ATERM_X=` hands
/// every descendant an is_some() veto nothing intended. That exact species
/// disabled the seamless updater on the owner's daily terminal twice over
/// (an empty `$ATERM_CONTROL_SOCK` on 2026-09-01, and the `ATERM_NO_*` family
/// audited the same day). Flag readers go through here, or they re-grow the
/// bug.
#[must_use]
pub fn env_flag_engaged(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
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

/// The pid a SOCKET filename encodes (`aterm-<pid>.sock`), and only that
/// spelling. [`instance_pid`] also accepts `aterm-<pid>.token` because the
/// stale sweep walks both kinds of leftover; the token rule must not, because
/// it answers "which file authenticates THIS socket" — handed a socket path
/// ending `.token`, the tolerant parse names the socket file ITSELF, and the
/// server would then write its capability token over its own listening
/// socket. It also keeps this rule byte-identical to the `aterm-uds` mirror,
/// which has always keyed on `.sock` alone.
fn sock_name_pid(sock_name: &str) -> Option<u32> {
    if !sock_name.ends_with(".sock") {
        return None;
    }
    instance_pid(sock_name)
}

/// The token filename that authenticates the socket named `sock_name` — the
/// one name the server WRITES beside that socket, and the first one a client
/// reads.
///
/// * `aterm-<pid>.sock` → `aterm-<pid>.token`: byte for byte the pairing
///   every release has shipped, so no default install changes.
/// * any other name — an explicit `$ATERM_CONTROL_SOCK` path — → that
///   socket's OWN filename with `.token` appended (`a.sock` → `a.sock.token`),
///   carrying [`EXPLICIT_TOKEN_PREFIX`] when the name does not end in `.sock`.
///
/// The rule is INJECTIVE over socket filenames, and that is the whole point:
/// two sockets in one directory can never name one token file. It used to
/// collapse every non-instance name onto the shared [`SIBLING_TOKEN_FILE`],
/// so a second explicit-socket instance in a directory overwrote the first
/// one's token and locked out its clients while it was still listening (F9).
///
/// Note the pid ROUND-TRIP: the digits are parsed to a `u32` and re-formatted,
/// so `aterm-01.sock` pairs with `aterm-1.token` (not `aterm-01.token`) and a
/// pid past `u32::MAX` falls through to the explicit form. That is unchanged
/// behaviour, and `pid.to_string()` never emits a leading zero, so no name the
/// server itself creates can reach it.
#[must_use]
pub fn token_name_for_sock(sock_name: &str) -> String {
    if let Some(pid) = sock_name_pid(sock_name) {
        return instance_token_name(pid);
    }
    // Trust gate: concatenation instead of `format!` — see `instance_sock_name`.
    let mut name = String::new();
    if !sock_name.ends_with(".sock") {
        name.push_str(EXPLICIT_TOKEN_PREFIX);
    }
    name.push_str(sock_name);
    name.push_str(".token");
    name
}

/// Every token filename a CLIENT may read for the socket named `sock_name`,
/// in the order it must try them. The first is always
/// [`token_name_for_sock`] — the file this build's server writes.
///
/// An explicit socket adds exactly one fallback, the legacy
/// [`SIBLING_TOKEN_FILE`]: that is what a server built BEFORE the per-socket
/// token wrote for the very same socket, so a client from this build still
/// authenticates against an older instance instead of failing for a reason
/// no message could explain. It is a read-only bridge — nothing writes that
/// name any more — and it can be deleted once no supported build does.
///
/// It cannot be used to present a DIFFERENT instance's token. A server
/// compares `AUTH` against the token it minted in memory for itself, never
/// against a file, so a shared `aterm.token` belonging to some other instance
/// yields `ERR auth` — precisely what presenting nothing yields. And it is
/// reached ONLY when the per-socket file is ABSENT, so an instance that wrote
/// its own token can never have a client of its own fall off it.
///
/// A per-instance `aterm-<pid>.sock` gets NO fallback: it has paired with
/// `aterm-<pid>.token` since the first release, so a miss there means the
/// instance is gone, and the shared file could only ever be someone else's.
#[must_use]
pub fn token_names_for_sock(sock_name: &str) -> Vec<String> {
    let mut names = Vec::with_capacity(2);
    names.push(token_name_for_sock(sock_name));
    if sock_name_pid(sock_name).is_none() {
        names.push(SIBLING_TOKEN_FILE.to_string());
    }
    names
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
        // The `latest` symlink resolves to an instance sock BEFORE the name
        // rule is applied (the client `readlink`s first), so the pairing that
        // matters for a flagless client is this one — unchanged for ever.
        assert_eq!(token_name_for_sock("aterm-7.sock"), "aterm-7.token");
        assert_eq!(token_name_for_sock("aterm-1.sock"), "aterm-1.token");
        assert_eq!(
            token_name_for_sock("aterm-4294967295.sock"),
            "aterm-4294967295.token"
        );
        // The pid ROUND-TRIPS through `u32`: leading zeros normalize, and a pid
        // past `u32::MAX` is not an instance name at all.
        assert_eq!(token_name_for_sock("aterm-01.sock"), "aterm-1.token");
        assert_eq!(
            token_name_for_sock("aterm-99999999999.sock"),
            "aterm-99999999999.sock.token"
        );
    }

    /// F9: two explicit sockets in ONE directory must not name one token file.
    /// Each carries its own socket filename, so the second instance to start
    /// cannot overwrite the first one's credential.
    #[test]
    fn explicit_sockets_get_their_own_token_each() {
        assert_eq!(token_name_for_sock("a.sock"), "a.sock.token");
        assert_eq!(token_name_for_sock("b.sock"), "b.sock.token");
        assert_ne!(token_name_for_sock("a.sock"), token_name_for_sock("b.sock"));
        // A socket literally named `aterm.sock` is only ever REACHED here when
        // it is a real socket (the alias is resolved first), and it too gets
        // its own file rather than the legacy shared one.
        assert_eq!(token_name_for_sock("aterm.sock"), "aterm.sock.token");
        assert_ne!(token_name_for_sock("aterm.sock"), SIBLING_TOKEN_FILE);
    }

    /// Odd names stay inside the rule, and — the load-bearing part — outside
    /// the two RESERVED names. A bare `.token` append would hand a socket
    /// named `aterm` the legacy shared file and one named `aterm-4242` pid
    /// 4242's file, re-creating the collision under a different spelling.
    #[test]
    fn odd_socket_names_never_land_on_a_reserved_token_name() {
        for (sock, token) in [
            ("ctl", "aterm-sock-ctl.token"),
            ("aterm", "aterm-sock-aterm.token"),
            ("aterm-4242", "aterm-sock-aterm-4242.token"),
            ("aterm-42.token", "aterm-sock-aterm-42.token.token"),
            ("a.b.c.sock", "a.b.c.sock.token"),
            ("\u{65e5}\u{672c}.sock", "\u{65e5}\u{672c}.sock.token"),
            ("aterm-.sock", "aterm-.sock.token"),
            ("aterm-+1.sock", "aterm-+1.sock.token"),
            ("", "aterm-sock-.token"),
        ] {
            assert_eq!(token_name_for_sock(sock), token, "for {sock:?}");
            assert_ne!(token_name_for_sock(sock), SIBLING_TOKEN_FILE);
            assert_eq!(
                instance_pid(&token_name_for_sock(sock)),
                None,
                "{sock:?} must not derive a per-instance token name"
            );
        }
    }

    /// The property the whole fix rests on: distinct socket filenames derive
    /// distinct token filenames, so no directory can host two sockets sharing
    /// one credential file.
    #[test]
    fn the_token_name_rule_is_injective_over_socket_names() {
        let names = [
            "aterm-1.sock",
            "aterm-2.sock",
            "a.sock",
            "b.sock",
            "aterm.sock",
            "aterm",
            "aterm-1",
            "aterm-1.token",
            "ctl",
            "",
        ];
        let mut tokens: Vec<String> = names.iter().map(|n| token_name_for_sock(n)).collect();
        tokens.sort();
        let count = tokens.len();
        tokens.dedup();
        assert_eq!(tokens.len(), count, "two socket names shared a token name");
    }

    /// A client tries the per-socket file first and the legacy shared name
    /// only for an explicit socket — never for `aterm-<pid>.sock`, whose token
    /// has never been shared and whose absence means the instance is gone.
    #[test]
    fn read_candidates_put_the_per_socket_token_first() {
        assert_eq!(token_names_for_sock("aterm-7.sock"), vec!["aterm-7.token"]);
        assert_eq!(
            token_names_for_sock("a.sock"),
            vec!["a.sock.token".to_string(), SIBLING_TOKEN_FILE.to_string()]
        );
        assert_eq!(
            token_names_for_sock("ctl"),
            vec![
                "aterm-sock-ctl.token".to_string(),
                SIBLING_TOKEN_FILE.to_string()
            ]
        );
        // The first candidate is always the name the server writes.
        for n in ["aterm-7.sock", "a.sock", "ctl", ""] {
            assert_eq!(token_names_for_sock(n)[0], token_name_for_sock(n));
        }
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
