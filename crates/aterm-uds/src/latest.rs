// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `latest` alias that points flagless clients at the newest instance's `aterm-<pid>.sock`.

use std::ffi::OsString;
use std::path::Path;

/// The pid a socket filename encodes (`aterm-<pid>.sock`); only the `.sock`
/// spelling, so a file named `aterm-<pid>.token` is never taken for a socket.
/// Mirrors `aterm_types::control_socket::sock_name_pid`.
fn sock_name_pid(sock_name: &str) -> Option<u32> {
    sock_name
        .strip_prefix("aterm-")
        .and_then(|s| s.strip_suffix(".sock"))
        .filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|d| d.parse::<u32>().ok())
}

/// The token filename that authenticates the socket named `sock_name`, the one name the server
/// writes beside it.
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

/// Every token filename a client may read for `sock_name`, most specific first.
#[must_use]
pub fn token_names_for_sock(sock_name: &str) -> Vec<String> {
    let mut names = Vec::with_capacity(2);
    names.push(token_name_for_sock(sock_name));
    if sock_name_pid(sock_name).is_none() {
        names.push("aterm.token".to_string());
    }
    names
}

/// The absolute path of the token file authenticating the socket at `sock`.
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

/// Atomically (re)point the `latest` alias at this instance's socket.
#[cfg(unix)]
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

/// Windows twin of the Unix publish.
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

/// The alias target's file name (`aterm-<pid>.sock`), or `None` when `link` is not a live alias.
#[must_use]
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

/// Client-side alias redirect.
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
    /// `aterm drive --socket` client calls (`aterm-ctl` pins it against
    /// `aterm-types`).
    #[test]
    fn token_names_pair_per_socket_and_never_reach_a_reserved_name() {
        assert_eq!(token_name_for_sock("aterm-7.sock"), "aterm-7.token");
        assert_eq!(token_name_for_sock("aterm-01.sock"), "aterm-1.token");
        assert_eq!(token_name_for_sock("a.sock"), "a.sock.token");
        assert_eq!(token_name_for_sock("b.sock"), "b.sock.token");
        assert_eq!(token_name_for_sock("aterm.sock"), "aterm.sock.token");
        assert_eq!(token_name_for_sock("aterm"), "aterm-sock-aterm.token");
        assert_eq!(token_name_for_sock("aterm-42"), "aterm-sock-aterm-42.token");
        assert_eq!(token_name_for_sock("ctl"), "aterm-sock-ctl.token");
        assert_eq!(token_name_for_sock(""), "aterm-sock-.token");
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

        assert_eq!(
            token_path_for_sock(&sock).expect("resolves"),
            dir.join("a.sock.token")
        );

        std::fs::write(dir.join("aterm.token"), "beef\n").unwrap();
        assert_eq!(
            token_path_for_sock(&sock).expect("resolves"),
            dir.join("aterm.token")
        );

        std::fs::write(dir.join("a.sock.token"), "feed\n").unwrap();
        assert_eq!(
            token_path_for_sock(&sock).expect("resolves"),
            dir.join("a.sock.token")
        );

        #[cfg(unix)]
        {
            let token = dir.join("a.sock.token");
            std::fs::remove_file(&token).unwrap();
            std::os::unix::fs::symlink("missing-token", &token).unwrap();
            assert_eq!(token_path_for_sock(&sock).expect("resolves"), token);

            std::fs::remove_file(&token).unwrap();
            assert_eq!(
                token_path_for_sock(&sock).expect("resolves"),
                dir.join("aterm.token")
            );
        }

        let inst = dir.join("aterm-77.sock");
        assert_eq!(
            token_path_for_sock(&inst.to_string_lossy()).expect("resolves"),
            dir.join("aterm-77.token")
        );

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
