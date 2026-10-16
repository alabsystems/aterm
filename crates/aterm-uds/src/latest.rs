// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `latest` alias (`aterm.sock`) that points flagless clients at the
//! newest instance's `aterm-<pid>.sock`.
//!
//! * Unix: a relative symlink, published atomically (symlink under a temp name,
//!   then `rename`). A client that presents a token `readlink`s it once and
//!   dials the instance socket it names, reading that socket's token; resolving
//!   the link twice could name two instances if another republished it between.
//! * Windows: NTFS symlinks need privilege or developer mode, so the alias is a
//!   regular pointer file holding the relative instance name
//!   (`aterm-<pid>.sock\n`), written under a temp name then renamed
//!   (`MoveFileExW` + replace, atomic on NTFS). Clients resolve it themselves
//!   ([`resolve`]); contents are validated against the `aterm-<pid>.sock` shape,
//!   so a planted junk file degrades to "no alias", never to dialing a path
//!   outside the socket dir. The trust boundary matches the Unix symlink.

use std::ffi::OsString;
use std::path::Path;

/// The pid a socket filename encodes (`aterm-<pid>.sock`); only the `.sock`
/// spelling, so a file named `aterm-<pid>.token` is never taken for a socket.
/// Mirrors `aterm_types::control_socket::sock_name_pid`.
fn sock_name_pid(sock_name: &str) -> Option<u32> {
    sock_name
        .strip_prefix("aterm-")
        .and_then(|s| s.strip_suffix(".sock"))
        // Digits only: keep `u32::parse`'s `+` tolerance from matching odd names.
        .filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|d| d.parse::<u32>().ok())
}

/// The token filename that authenticates the socket named `sock_name`, the one name the server
/// writes beside it. `aterm-<pid>.sock` pairs with `aterm-<pid>.token` (the pid round-trips
/// through `u32`: `aterm-01.sock` pairs with `aterm-1.token`, and an out-of-range pid takes the
/// explicit form); any other `--control-sock` name pairs with its own filename plus `.token`
/// (`a.sock` → `a.sock.token`), prefixed `aterm-sock-` when it does not end in `.sock`, so it
/// never lands on the reserved `aterm.token` / `aterm-<pid>.token` and two explicit sockets in
/// one directory never share a token file.
///
/// Mirrors `aterm_types::control_socket::token_name_for_sock`, kept local so this crate stays
/// dependency-free; aterm-ctl's `uds_token_name_mirror_matches_aterm_types` pins the two.
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
/// first: the per-socket file, then (for an explicit socket only) the legacy
/// shared `aterm.token` older servers wrote for it.
///
/// Mirrors `aterm_types::control_socket::token_names_for_sock`. The fallback
/// is safe: a server checks `AUTH` against the token it minted in memory, so
/// another instance's shared `aterm.token` is refused like no token at all. A
/// per-instance socket gets no fallback.
#[must_use]
pub fn token_names_for_sock(sock_name: &str) -> Vec<String> {
    let mut names = Vec::with_capacity(2);
    names.push(token_name_for_sock(sock_name));
    if sock_name_pid(sock_name).is_none() {
        names.push("aterm.token".to_string());
    }
    names
}

/// The absolute path of the token file authenticating the socket at `sock`:
/// [`token_names_for_sock`] resolved in the socket's own directory, after
/// following the `latest` alias ([`target_name`]).
///
/// The first candidate not known to be absent wins; with all absent the
/// per-socket name is returned, so an error names the file this build expects.
/// Only `NotFound` moves on: an unreadable or dangling per-socket entry belongs
/// to the instance being dialed, and falling back would reach for another
/// instance's credential.
#[must_use]
pub fn token_path_for_sock(sock: &str) -> Option<std::path::PathBuf> {
    let p = Path::new(sock);
    let dir = p.parent()?;
    let name = target_name(p).unwrap_or(p.file_name()?.to_os_string());
    let mut candidates = token_names_for_sock(&name.to_string_lossy())
        .into_iter()
        .map(|n| dir.join(n));
    let first = candidates.next()?;
    let missing = |path: &Path| matches!(path.symlink_metadata(), Err(e) if e.kind() == std::io::ErrorKind::NotFound);
    if !missing(&first) {
        return Some(first);
    }
    Some(candidates.find(|c| !missing(c)).unwrap_or(first))
}

/// Atomically (re)point the `latest` alias at this instance's socket: a symlink to the
/// relative sock filename, made at `<target>.lnk` and renamed over `link`, so a client never
/// sees a missing alias and the newest instance wins. Best-effort: clients can still dial the
/// instance socket directly (`aterm ctl --pid`).
#[cfg(unix)]
// Skip: bottoms out at std fs syscalls.
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

/// Windows twin of the Unix publish: the same temp-name + rename, but the
/// alias is a pointer file carrying the relative instance name.
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

/// The alias target's file name (`aterm-<pid>.sock`), or `None` when `link`
/// is not a live alias. Unix: `readlink` + final component. Windows: the
/// pointer file's trimmed contents, validated against the instance-name shape.
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

/// Validate a pointer file's contents as a bare `aterm-<digits>.sock` name
/// (one path component). Mirrors `aterm_types::control_socket::instance_pid`
/// plus the `.sock` suffix, so a planted alias can only redirect within the
/// socket directory.
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

/// Client-side alias redirect: on Windows, a pointer file whose validated contents name an
/// instance socket resolves to `<parent>/<instance>.sock`; otherwise (always on Unix, where
/// `connect` follows the symlink) `path` is returned unchanged. A dangling pointer behaves
/// like a dangling symlink: the later connect reports `NotFound`.
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

    /// The mirror's own statement of the rule, in the crate the dependency-free
    /// `--dial` client calls (`aterm-ctl` pins it against `aterm-types`).
    #[test]
    fn token_names_pair_per_socket_and_never_reach_a_reserved_name() {
        // Per-instance.
        assert_eq!(token_name_for_sock("aterm-7.sock"), "aterm-7.token");
        assert_eq!(token_name_for_sock("aterm-01.sock"), "aterm-1.token");
        // Explicit sockets carry their own filename, so two in one directory
        // never share a credential file.
        assert_eq!(token_name_for_sock("a.sock"), "a.sock.token");
        assert_eq!(token_name_for_sock("b.sock"), "b.sock.token");
        assert_eq!(token_name_for_sock("aterm.sock"), "aterm.sock.token");
        // Names that would otherwise append onto a reserved name are prefixed.
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

    /// The per-socket file wins whenever it exists; the legacy shared name only in its
    /// absence, and never for a per-instance socket.
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

        // Only the legacy file (an older server): the fallback.
        std::fs::write(dir.join("aterm.token"), "beef\n").unwrap();
        assert_eq!(
            token_path_for_sock(&sock).expect("resolves"),
            dir.join("aterm.token")
        );

        // Both present: the per-socket file wins; the shared one may be anybody's.
        std::fs::write(dir.join("a.sock.token"), "feed\n").unwrap();
        assert_eq!(
            token_path_for_sock(&sock).expect("resolves"),
            dir.join("a.sock.token")
        );

        #[cfg(unix)]
        {
            // An existing per-socket entry whose target is missing still wins;
            // it must not send the legacy credential to this socket instead.
            let token = dir.join("a.sock.token");
            std::fs::remove_file(&token).unwrap();
            std::os::unix::fs::symlink("missing-token", &token).unwrap();
            assert_eq!(token_path_for_sock(&sock).expect("resolves"), token);

            // Removing that entry restores the older server's legacy fallback.
            std::fs::remove_file(&token).unwrap();
            assert_eq!(
                token_path_for_sock(&sock).expect("resolves"),
                dir.join("aterm.token")
            );
        }

        // A per-instance socket never falls back.
        let inst = dir.join("aterm-77.sock");
        assert_eq!(
            token_path_for_sock(&inst.to_string_lossy()).expect("resolves"),
            dir.join("aterm-77.token")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two explicit sockets in one directory resolve to two different token
    /// files, so the second instance cannot overwrite the first's credential.
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

    /// The `latest` alias is resolved before the name rule, so a flagless
    /// client reads the token of the instance the alias points at.
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
