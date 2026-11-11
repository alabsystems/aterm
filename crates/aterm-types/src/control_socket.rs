// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pure lifecycle decisions for the introspection control socket, shared by the server (`aterm-gui`) and the client (`aterm-ctl`).

use std::path::Path;

/// Filename of the `latest` symlink in the socket directory: points at the newest instance's `aterm-<pid>.sock`, so clients with no flags reach it.
pub const LATEST_SOCK_FILE: &str = "aterm.sock";

/// The legacy shared token filename.
pub const SIBLING_TOKEN_FILE: &str = "aterm.token";

/// Prefix that keeps an oddly-named socket's token off the two reserved names,
/// [`SIBLING_TOKEN_FILE`] and `aterm-<pid>.token`: appending `.token` to `aterm` or `aterm-4242`
/// would yield them, so a name not ending in `.sock` is prefixed.
const EXPLICIT_TOKEN_PREFIX: &str = "aterm-sock-";

/// What the host should do about the control socket, decided from its launch flags by [`socket_directive`].
#[derive(Debug, PartialEq, Eq)]
pub enum SocketDirective {
    /// Bind the per-instance default (`aterm-<pid>.sock` in the per-user dir) and maintain the `latest` symlink.
    PerInstance,
    /// Bind exactly this caller-supplied path; no symlink is maintained.
    Explicit(String),
    /// Do not bind a control socket at all.
    Disabled,
}

/// Decide the socket disposition from the `--control-sock` value and the `--no-control-sock` switch (`None` = not given; the switch reads as [`env_flag_engaged`]).
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

/// The one reading of a boolean `ATERM_NO_*` / veto value (`None` = unset): engaged only by a non-empty value other than `"0"`.
#[must_use]
pub fn env_flag_engaged(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// The per-instance socket filename for `pid`: `aterm-<pid>.sock`.
#[must_use]
pub fn instance_sock_name(pid: u32) -> String {
    let mut name = String::from("aterm-");
    name.push_str(&pid.to_string());
    name.push_str(".sock");
    name
}

/// The per-instance token filename for `pid`: `aterm-<pid>.token`.
#[must_use]
pub fn instance_token_name(pid: u32) -> String {
    let mut name = String::from("aterm-");
    name.push_str(&pid.to_string());
    name.push_str(".token");
    name
}

/// Parse the owning pid out of a per-instance filename (`aterm-<pid>.sock` or `aterm-<pid>.token`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn instance_pid(name: &str) -> Option<u32> {
    let stem = name
        .strip_suffix(".sock")
        .or_else(|| name.strip_suffix(".token"))?;
    let pid = stem.strip_prefix("aterm-")?;
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok()
}

/// The pid a socket filename encodes, for the `aterm-<pid>.sock` spelling only. [`instance_pid`]
/// also accepts `.token` for the stale sweep; the token rule must not, or a socket path ending
/// `.token` would name itself and the server would write its token over its own socket.
fn sock_name_pid(sock_name: &str) -> Option<u32> {
    if !sock_name.ends_with(".sock") {
        return None;
    }
    instance_pid(sock_name)
}

/// The token filename that authenticates the socket named `sock_name`: the name the server writes beside it and the first a client reads.
#[must_use]
pub fn token_name_for_sock(sock_name: &str) -> String {
    if let Some(pid) = sock_name_pid(sock_name) {
        return instance_token_name(pid);
    }
    let mut name = String::new();
    if !sock_name.ends_with(".sock") {
        name.push_str(EXPLICIT_TOKEN_PREFIX);
    }
    name.push_str(sock_name);
    name.push_str(".token");
    name
}

/// Every token filename a client may read for the socket named `sock_name`, in order.
#[must_use]
pub fn token_names_for_sock(sock_name: &str) -> Vec<String> {
    let mut names = Vec::with_capacity(2);
    names.push(token_name_for_sock(sock_name));
    if sock_name_pid(sock_name).is_none() {
        names.push(SIBLING_TOKEN_FILE.to_string());
    }
    names
}

/// The `sock <abs-path>` line of a discovery graph entry (`<dir>/graph/<sid>`), which the server writes and both its sibling forward and `aterm-ctl`'s self-location read: one parser, so the ends cannot drift.
#[must_use]
pub fn graph_entry_sock(body: &str) -> Option<String> {
    body.lines()
        .find_map(|l| l.strip_prefix("sock "))
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
}

/// One spelling per socket file, however a path arrived at it: the key both the writer and the reader of the discovery graph use.
#[must_use]
pub fn socket_key(path: &str) -> String {
    let stripped = path.strip_prefix(r"\\?\").unwrap_or(path);
    let drive = stripped.len() >= 2
        && stripped.as_bytes()[1] == b':'
        && stripped.as_bytes()[0].is_ascii_alphabetic();
    let text = if drive { stripped } else { path };
    if cfg!(windows) {
        text.to_ascii_lowercase()
    } else {
        text.to_string()
    }
}

/// The `pid <n>` line of a discovery graph entry: the pid of the instance that wrote it, so `aterm-ctl`'s `instances`/`ls` can report a pid for an explicit-`--control-sock` instance whose socket name carries none.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn graph_entry_pid(body: &str) -> Option<u32> {
    let raw = body.lines().find_map(|l| l.strip_prefix("pid "))?.trim();
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    raw.parse().ok()
}

/// From a socket-directory listing, the per-instance files whose owning pid is dead — stale leftovers of crashed instances, safe to remove.
#[must_use]
pub fn stale_instance_files(names: &[&str], pid_alive: &dyn Fn(u32) -> bool) -> Vec<String> {
    names
        .iter()
        .filter(|n| matches!(instance_pid(n), Some(pid) if !pid_alive(pid)))
        .map(|n| (*n).to_string())
        .collect()
}

/// One entry of a socket-directory listing as the launch sweep sees it: its name, its byte length, and how long ago it was last written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SockFileFacts<'a> {
    /// The bare filename (`aterm-4242.sock`, `private.sock`, `images`, …).
    pub name: &'a str,
    /// Byte length.
    pub len: u64,
    /// Seconds since the file was last written; `0` when the clock cannot say.
    pub age_secs: u64,
}

/// How long a zero-length socket file must sit untouched before the launch sweep probes it: one day, which keeps an instance between its `bind` and first `accept` (milliseconds) out of reach.
pub const STALE_SOCK_MIN_AGE_SECS: u64 = 24 * 60 * 60;

/// From a socket-directory listing, the zero-length `*.sock` files at least `min_age_secs` old that nothing listens on: leftovers of killed instances, safe to remove.
#[must_use]
pub fn stale_socket_files(
    files: &[SockFileFacts<'_>],
    min_age_secs: u64,
    aliased: Option<&str>,
    pid_alive: &dyn Fn(u32) -> bool,
    listening: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    files
        .iter()
        .filter(|f| {
            f.name.ends_with(".sock")
                && f.name != LATEST_SOCK_FILE
                && Some(f.name) != aliased
                && f.len == 0
                && f.age_secs >= min_age_secs
                && match instance_pid(f.name) {
                    Some(pid) if !pid_alive(pid) => true,
                    _ => !listening(f.name),
                }
        })
        .map(|f| f.name.to_string())
        .collect()
}

/// Whether a `latest` symlink target (relative or absolute) designates the instance socket of `pid` — i.e. the link belongs to that instance and may be removed on its exit.
#[must_use]
pub fn symlink_targets_pid(target: &str, pid: u32) -> bool {
    Path::new(target)
        .file_name()
        .is_some_and(|f| f == instance_sock_name(pid).as_str())
}

#[cfg(test)]
#[path = "control_socket_tests.rs"]
mod tests;
