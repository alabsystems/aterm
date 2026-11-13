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
    let open = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| std::io::Write::write_all(&mut f, contents.as_bytes()));
    if open.is_err() {
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
        use std::io::Read as _;
        const MAX_ALIAS_BYTES: u64 = 256;
        let (file, meta) = crate::regular_file::open(link).ok()?;
        if meta.len() > MAX_ALIAS_BYTES {
            return None;
        }
        let cap = usize::try_from(meta.len().min(MAX_ALIAS_BYTES))
            .unwrap_or(0)
            .saturating_add(1);
        let mut body = String::with_capacity(cap);
        file.take(MAX_ALIAS_BYTES + 1)
            .read_to_string(&mut body)
            .ok()?;
        if body.len() as u64 > MAX_ALIAS_BYTES {
            return None;
        }
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
#[path = "latest_tests.rs"]
mod tests;
