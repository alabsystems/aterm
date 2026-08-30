// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `latest` alias (`aterm.sock`) that points flagless clients at the
//! newest instance's `aterm-<pid>.sock`.
//!
//! * **Unix**: a relative SYMLINK, published atomically (symlink under a temp
//!   name, then `rename`). Clients connect THROUGH it (kernel-resolved) and
//!   `readlink` only to learn the sibling token name. Verbatim the shipping
//!   `aterm-gui` behavior.
//! * **Windows**: NTFS symlinks need privilege or developer mode, so the
//!   alias is a regular POINTER FILE whose contents are the same relative
//!   instance name a Unix `readlink` returns (`aterm-<pid>.sock\n`), written
//!   under a temp name then `rename`d (`MoveFileExW` + replace — atomic on
//!   NTFS). Clients must resolve it themselves ([`resolve`]) before
//!   connecting; contents are validated against the `aterm-<pid>.sock` shape
//!   before use, so a same-user-planted junk file degrades to "no alias",
//!   never to dialing an arbitrary path outside the socket dir. The trust
//!   boundary is identical to the Unix symlink (also same-user-writable).

use std::ffi::OsString;
use std::path::Path;

/// The pid a SOCKET filename encodes (`aterm-<pid>.sock`) — the `.sock`
/// spelling only, so a path pointed at a file named `aterm-<pid>.token` is
/// never told its token is itself. Mirrors
/// `aterm_types::control_socket::sock_name_pid`.
fn sock_name_pid(sock_name: &str) -> Option<u32> {
    sock_name
        .strip_prefix("aterm-")
        .and_then(|s| s.strip_suffix(".sock"))
        // Digits only: keep `u32::parse`'s `+` tolerance from matching odd names.
        .filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|d| d.parse::<u32>().ok())
}

/// The token filename that authenticates the socket named `sock_name`, and the
/// one name the server writes beside it: a per-instance socket
/// (`aterm-<pid>.sock`) pairs with its `aterm-<pid>.token`; **everything else**
/// — an EXPLICIT custom `$ATERM_CONTROL_SOCK` path — pairs with its OWN
/// filename plus `.token` (`a.sock` → `a.sock.token`), prefixed `aterm-sock-`
/// when the name does not end in `.sock` so it can never land on the reserved
/// `aterm.token` / `aterm-<pid>.token` names.
///
/// Mirrors `aterm_types::control_socket::token_name_for_sock`; kept local so
/// this crate stays dependency-free, exactly as `is_instance_sock_name` mirrors
/// the `.sock` shape. `aterm-ctl` resolves the token through the canonical
/// copy, so the two MUST agree — pinned by
/// `uds_token_name_mirror_matches_aterm_types` in `aterm-ctl`, the one crate
/// that depends on both.
///
/// Load-bearing twice over. Hand-rolling `<stem>.token` instead (the bug this
/// replaced) derives `/tmp/c.token` for `/tmp/c.sock`, which no server writes.
/// And collapsing every non-instance name onto the shared `aterm.token` (the
/// bug this REPLACES) gave two explicit sockets in one directory one token
/// file, so the second instance to start silently took the first one's
/// credential and its clients were refused `ERR auth` while it still listened.
/// Note the pid round-trip: the canonical rule parses the digits to a `u32` and
/// re-formats, so `aterm-01.sock` pairs with `aterm-1.token` (NOT
/// `aterm-01.token`) and an out-of-range pid falls through to the explicit
/// form. Mirroring by string slicing alone silently drifts on both — the
/// cross-check test caught exactly that.
#[must_use]
pub fn token_name_for_sock(sock_name: &str) -> String {
    match sock_name_pid(sock_name) {
        Some(pid) => format!("aterm-{pid}.token"),
        None => {
            let mut name = String::new();
            if !sock_name.ends_with(".sock") {
                name.push_str("aterm-sock-");
            }
            name.push_str(sock_name);
            name.push_str(".token");
            name
        }
    }
}

/// Every token filename a client may read for `sock_name`, most specific
/// first: the per-socket file this build's server writes, then — for an
/// explicit socket only — the legacy shared `aterm.token` a server built
/// BEFORE the per-socket token wrote for that same socket.
///
/// Mirrors `aterm_types::control_socket::token_names_for_sock`, including the
/// reason the fallback is safe: a server compares `AUTH` against the token it
/// minted in memory, never against a file, so a shared `aterm.token` that
/// belongs to another instance is refused exactly as no token at all is. A
/// per-instance socket gets no fallback — its token has never been shared.
#[must_use]
pub fn token_names_for_sock(sock_name: &str) -> Vec<String> {
    let mut names = Vec::with_capacity(2);
    names.push(token_name_for_sock(sock_name));
    if sock_name_pid(sock_name).is_none() {
        names.push("aterm.token".to_string());
    }
    names
}

/// The absolute path of the token file authenticating the socket at `sock` —
/// [`token_names_for_sock`] resolved in the socket's OWN directory, after
/// following the `latest` alias ([`target_name`]) so a flagless client reads the
/// pointed-at instance's token rather than the alias's.
///
/// The first candidate that EXISTS wins; with none on disk the per-socket name
/// is returned anyway, so a caller's error message names the file this build
/// expects rather than a legacy one nothing writes. Absent is the only reason
/// to move on: a per-socket token that exists but cannot be read belongs to the
/// instance being dialed, and reaching past it for a directory-shared file
/// would be reaching for someone else's credential.
#[must_use]
pub fn token_path_for_sock(sock: &str) -> Option<std::path::PathBuf> {
    let p = Path::new(sock);
    let dir = p.parent()?;
    let name = target_name(p).unwrap_or(p.file_name()?.to_os_string());
    let mut candidates = token_names_for_sock(&name.to_string_lossy())
        .into_iter()
        .map(|n| dir.join(n));
    let first = candidates.next()?;
    if first.exists() {
        return Some(first);
    }
    Some(candidates.find(|c| c.exists()).unwrap_or(first))
}

/// Atomically (re)point the `latest` alias at this instance's socket: write
/// the RELATIVE sock filename under `<target>.lnk`, then rename over `link`,
/// so a client never observes a missing alias and the newest instance always
/// wins. Best-effort: on failure clients can still target the instance socket
/// directly (`aterm-ctl --pid`).
#[cfg(unix)]
// Skip: best-effort alias (re)point bottoms out at std fs syscalls
// (`remove_file`/`rename`, absent bodies) + OsString alloc.
#[cfg_attr(trust_verify, trust::skip)]
pub fn publish(link: &Path, sock_path: &str) {
    let Some(target) = Path::new(sock_path).file_name() else {
        return;
    };
    let mut tmp_name = target.to_os_string();
    tmp_name.push(".lnk");
    let tmp = link.with_file_name(tmp_name);
    let _ = std::fs::remove_file(&tmp);
    if std::os::unix::fs::symlink(target, &tmp).is_err() {
        return;
    }
    if std::fs::rename(&tmp, link).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Windows twin of the Unix publish: same temp-name + rename dance, but the
/// alias is a pointer FILE carrying the relative instance name (see the
/// module docs).
#[cfg(windows)]
pub fn publish(link: &Path, sock_path: &str) {
    let Some(target) = Path::new(sock_path).file_name() else {
        return;
    };
    let mut tmp_name = target.to_os_string();
    tmp_name.push(".lnk");
    let tmp = link.with_file_name(tmp_name);
    let _ = std::fs::remove_file(&tmp);
    let mut contents = target.to_string_lossy().into_owned();
    contents.push('\n');
    if std::fs::write(&tmp, contents).is_err() {
        return;
    }
    if std::fs::rename(&tmp, link).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// The alias target's FILE NAME (`aterm-<pid>.sock`), or `None` when `link`
/// is not a live alias. Unix: `readlink` + final component (the link target
/// is already the relative name). Windows: the pointer file's trimmed
/// contents, validated against the instance-name shape first.
#[must_use]
// Skip: reads + validates the pointer file via std fs (absent syscall bodies).
#[cfg_attr(trust_verify, trust::skip)]
pub fn target_name(link: &Path) -> Option<OsString> {
    #[cfg(unix)]
    {
        std::fs::read_link(link)
            .ok()
            .and_then(|t| t.file_name().map(std::ffi::OsStr::to_os_string))
    }
    #[cfg(windows)]
    {
        // Reading a real socket (an afunix reparse point) or a directory here
        // fails, so a non-alias path safely yields `None`.
        let body = std::fs::read_to_string(link).ok()?;
        let name = body.trim();
        if is_instance_sock_name(name) {
            Some(OsString::from(name))
        } else {
            None
        }
    }
}

/// Validate a pointer file's contents as a bare `aterm-<digits>.sock` name —
/// one path component, nothing else. Mirrors
/// `aterm_types::control_socket::instance_pid` (+ the `.sock` suffix); kept
/// local so this crate stays dependency-free, and load-bearing: a planted
/// alias can only ever redirect WITHIN the socket directory.
#[cfg(windows)]
fn is_instance_sock_name(name: &str) -> bool {
    let Some(stem) = name.strip_prefix("aterm-") else {
        return false;
    };
    let Some(digits) = stem.strip_suffix(".sock") else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) && !name.contains(['/', '\\'])
}

/// Client-side alias redirect: on Windows, if `path` is a pointer file whose
/// validated contents name an instance socket, return
/// `<parent>/<instance>.sock`; otherwise (and always on Unix, where the
/// kernel resolves the symlink during `connect`) return `path` unchanged.
/// A dangling pointer behaves like a dangling symlink — the later connect
/// reports `NotFound`.
#[must_use]
pub fn resolve(path: &str) -> String {
    #[cfg(windows)]
    {
        let p = Path::new(path);
        if let (Some(name), Some(dir)) = (target_name(p), p.parent()) {
            return dir.join(name).to_string_lossy().into_owned();
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mirror's own statement of the rule. `aterm-ctl` pins it against the
    /// canonical `aterm-types` copy; this pins the SHAPE here, in the crate the
    /// dependency-free `--dial` client actually calls.
    #[test]
    fn token_names_pair_per_socket_and_never_reach_a_reserved_name() {
        // Per-instance: unchanged since the first release.
        assert_eq!(token_name_for_sock("aterm-7.sock"), "aterm-7.token");
        assert_eq!(token_name_for_sock("aterm-01.sock"), "aterm-1.token");
        // Explicit sockets carry their own filename, so two in one directory
        // can never share a credential file (F9).
        assert_eq!(token_name_for_sock("a.sock"), "a.sock.token");
        assert_eq!(token_name_for_sock("b.sock"), "b.sock.token");
        assert_eq!(token_name_for_sock("aterm.sock"), "aterm.sock.token");
        // Names that would otherwise APPEND onto a reserved name are prefixed.
        assert_eq!(token_name_for_sock("aterm"), "aterm-sock-aterm.token");
        assert_eq!(token_name_for_sock("aterm-42"), "aterm-sock-aterm-42.token");
        assert_eq!(token_name_for_sock("ctl"), "aterm-sock-ctl.token");
        assert_eq!(token_name_for_sock(""), "aterm-sock-.token");
        // Read order: per-socket first, legacy shared name only as a fallback,
        // and never for a per-instance socket.
        assert_eq!(token_names_for_sock("aterm-7.sock"), vec!["aterm-7.token"]);
        assert_eq!(
            token_names_for_sock("a.sock"),
            vec!["a.sock.token".to_string(), "aterm.token".to_string()]
        );
    }

    /// A temp directory of this test's own (tests share one process).
    fn test_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("aterm-uds-tok-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    /// The per-socket file wins whenever it exists; the legacy shared name is
    /// reached only in its ABSENCE (an instance from a build that wrote it),
    /// and never for a per-instance socket. With nothing on disk the answer is
    /// the per-socket name, so a caller's miss names the file this build wants.
    #[test]
    fn token_path_prefers_the_per_socket_file_over_the_legacy_shared_one() {
        let dir = test_dir("fallback");
        let sock = dir.join("a.sock");
        let sock = sock.to_string_lossy().into_owned();

        // Nothing on disk: the per-socket name, not the legacy one.
        assert_eq!(
            token_path_for_sock(&sock).expect("resolves"),
            dir.join("a.sock.token")
        );

        // Only the legacy file (an older server): the bridge.
        std::fs::write(dir.join("aterm.token"), "beef\n").unwrap();
        assert_eq!(
            token_path_for_sock(&sock).expect("resolves"),
            dir.join("aterm.token")
        );

        // Both present: the per-socket file wins — the instance being dialed
        // wrote that one, and the shared file may be anybody's.
        std::fs::write(dir.join("a.sock.token"), "feed\n").unwrap();
        assert_eq!(
            token_path_for_sock(&sock).expect("resolves"),
            dir.join("a.sock.token")
        );

        // A per-instance socket never falls back, even with the legacy file
        // sitting right there: its token has never been shared.
        let inst = dir.join("aterm-77.sock");
        assert_eq!(
            token_path_for_sock(&inst.to_string_lossy()).expect("resolves"),
            dir.join("aterm-77.token")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// F9 in one assertion: two explicit sockets in ONE directory resolve to
    /// two different token files, so the second instance to start cannot
    /// overwrite the first one's credential.
    #[test]
    fn two_explicit_sockets_in_one_directory_never_share_a_token() {
        let dir = test_dir("pair");
        let a = dir.join("a.sock").to_string_lossy().into_owned();
        let b = dir.join("b.sock").to_string_lossy().into_owned();
        std::fs::write(dir.join("a.sock.token"), "aaaa\n").unwrap();
        std::fs::write(dir.join("b.sock.token"), "bbbb\n").unwrap();
        let ta = token_path_for_sock(&a).expect("resolves");
        let tb = token_path_for_sock(&b).expect("resolves");
        assert_ne!(ta, tb);
        assert_eq!(std::fs::read_to_string(&ta).unwrap().trim(), "aaaa");
        assert_eq!(std::fs::read_to_string(&tb).unwrap().trim(), "bbbb");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The `latest` alias is resolved BEFORE the name rule, so a flagless
    /// client reads the token of the instance the alias points AT — the
    /// property that keeps `aterm.sock` working with no flags.
    #[cfg(unix)]
    #[test]
    fn the_latest_alias_resolves_to_the_pointed_at_instance_token() {
        let dir = test_dir("alias");
        let link = dir.join("aterm.sock");
        publish(&link, &dir.join("aterm-101.sock").to_string_lossy());
        assert_eq!(
            token_path_for_sock(&link.to_string_lossy()).expect("resolves"),
            dir.join("aterm-101.token"),
            "the alias must name the instance's token, never its own"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
