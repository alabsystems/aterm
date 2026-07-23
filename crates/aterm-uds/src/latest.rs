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
