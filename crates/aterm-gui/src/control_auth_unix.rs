// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unix implementations behind [`control_auth`](super): private-dir and socket-file modes,
//! the token file, pid liveness, and the `getpeereid`/`SO_PEERCRED` same-uid peer gate.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use aterm_uds::CtlStream;

/// Create `dir` (and parents) if absent, force its mode to `0700`, and verify it is owned by
/// us and not group/other-writable.
///
/// SEC-3: the mode alone is not enough: a directory pre-created by another user keeps its
/// owner, who can plant or swap files. A foreign-owned or group/other-writable directory
/// fails [`aterm_types::fs_restricted::dir_safe_for_private_write`] and is refused.
pub(crate) fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    ensure_private_dir_as(dir, our_uid())
}

/// [`ensure_private_dir`] with the uid that must own `dir` given, so a test can
/// stand as a stranger to a directory it really owns: the one way, unprivileged,
/// to reach the SEC-3 refusal of a foreign-owned `0700` directory.
pub(super) fn ensure_private_dir_as(dir: &Path, owner: u32) -> std::io::Result<()> {
    if already_private(dir, owner) {
        return Ok(());
    }
    // Missing ancestors `0755` whatever the umask, never `create_dir_all`'s `0777 & ~umask`:
    // under the state root this can be the first to make `~/.local` (Linux) or the atpkg
    // prefix's parent (macOS), and `targo trust` refuses every toolchain installed below a
    // group-writable directory.
    aterm_types::fs_restricted::create_dir_all_unshared(dir, 0o700)?;
    tighten_and_verify(dir, owner)
}

/// Whether `dir` already IS what [`ensure_private_dir`] returns success for —
/// a directory, exactly `0700`,
/// passing the SEC-3 predicate on a fresh stat — so nothing needs creating or
/// writing: one `stat(2)`, where the full path is four (`mkdir`, the `EEXIST`
/// stat, two more). Every spawn asks for the control dir and its subdirs about
/// eight times. Nothing is remembered between calls, so a directory removed or
/// loosened since is seen at once and takes the full path. `owner` is the uid
/// the directory must belong to (ours, outside a test).
fn already_private(dir: &Path, owner: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(dir).is_ok_and(|meta| {
        meta.is_dir()
            && meta.mode() & 0o7777 == 0o700
            && aterm_types::fs_restricted::dir_safe_for_private_write(
                owner,
                meta.uid(),
                meta.mode(),
            )
    })
}

/// The tail both doors share, over a directory that is there: `0700`, written
/// only when it is not already, then the ownership-safety gate on a fresh stat,
/// for `owner`.
fn tighten_and_verify(dir: &Path, owner: u32) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    // Only a mode that is not already `0700` is written: a `chmod(2)` to the
    // mode a directory already has still moves its `st_ctime` (APFS), and
    // tippy aborts when an ancestor of its executable changes ctime mid-run
    // — aterm-update-core's copy of this rule learned that on 2026-09-15
    // (privatedir.rs). The stat below still proves the final mode.
    if !std::fs::metadata(dir).is_ok_and(|m| m.mode() & 0o7777 == 0o700) {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    // Stat after tightening: set_permissions cannot fix a foreign owner.
    let meta = std::fs::metadata(dir)?;
    let safe =
        aterm_types::fs_restricted::dir_safe_for_private_write(owner, meta.uid(), meta.mode());
    if safe {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{}: control directory must be owned by uid {} and not group/other-writable",
                dir.display(),
                owner
            ),
        ))
    }
}

/// Pid liveness via `kill(pid, 0)`: only `ESRCH` means dead; 0, `EPERM` and anything else
/// mean alive, because sweeping a live instance's files breaks it while keeping a dead one's
/// costs a file. Pids that cannot be real (0, or wider than `pid_t`) are dead.
pub(crate) fn pid_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Write a fresh capability token to `path` at mode `0600` and return its hex. It rotates
/// every launch, so a token leaked from a prior run is worthless.
///
/// `None` (no entropy, or the file cannot be written) must make the caller skip binding the
/// socket (fail closed).
#[must_use]
pub(crate) fn provision_token(path: &Path) -> Option<String> {
    let token = super::random_token_hex()?;
    // SEC-3: remove any prior file (our stale token, or a planted file or symlink), then
    // `O_CREAT|O_EXCL|O_NOFOLLOW`: we only write a file we just created, never through a
    // symlink even one raced in after the unlink, and it is never briefly world-readable.
    let _ = std::fs::remove_file(path);
    write_fresh_token(path, &token)?;
    Some(token)
}

/// The exclusive write behind [`provision_token`], once the unlink is done:
/// `token` lands only in a file this call creates. Whatever stands at `path`
/// by now — a file, a symlink, a hard link to a file of ours, planted in the
/// gap after the unlink — is refused (`None`) and left exactly as it was.
pub(super) fn write_fresh_token(path: &Path, token: &str) -> Option<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true).mode(0o600);
    opts.custom_flags(libc::O_NOFOLLOW);
    let f = opts.open(path).ok()?;
    // `fchmod` through the open fd: a path-based chmod would re-resolve the path.
    f.set_permissions(std::fs::Permissions::from_mode(0o600))
        .ok()?;
    let mut f = f;
    f.write_all(token.as_bytes()).ok()?;
    f.flush().ok()
}

/// Tighten the bound socket file to mode `0600` so only the owner can connect
/// even if it somehow lands in a shared directory. Best-effort: a failure here
/// still leaves the directory perms + peer check + token in force.
pub(crate) fn lock_socket_file(path: &str) {
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

/// The connecting peer's effective uid (`getpeereid(2)` on macOS/BSD, `SO_PEERCRED` on
/// Linux) via `aterm_uds::peer`, or `None` if the kernel will not say.
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
#[must_use]
pub(crate) fn peer_uid(stream: &CtlStream) -> Option<u32> {
    aterm_uds::peer::peer_uid(stream)
}

/// Other Unixes: no portable peer-cred primitive wired here. Return `None`,
/// which the caller treats as "cannot verify" → refuse (fail closed).
#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
#[must_use]
pub(crate) fn peer_uid(_stream: &CtlStream) -> Option<u32> {
    None
}

/// Our own effective uid — the only uid allowed to drive the socket.
#[must_use]
pub(crate) fn our_uid() -> u32 {
    unsafe { libc::geteuid() }
}

/// The accept-time peer gate: refuse any connection not from our own uid, including one
/// whose uid cannot be read (fail closed). The `Err` is the audit-logged denial text.
pub(crate) fn peer_check(stream: &CtlStream) -> Result<(), String> {
    peer_verdict(peer_uid(stream), our_uid())
}

/// [`peer_check`]'s decision over the uid the kernel reported (`None` when it
/// would not say): only `our_uid` is admitted.
pub(super) fn peer_verdict(peer: Option<u32>, our_uid: u32) -> Result<(), String> {
    match peer {
        Some(uid) if uid == our_uid => Ok(()),
        other => Err(format!("connect (peer uid {other:?} != {our_uid})")),
    }
}

#[cfg(test)]
mod ctime_tests {
    use super::ensure_private_dir;

    /// A directory already `0700` and ours is left EXACTLY alone — no `chmod(2)`,
    /// so its `st_ctime` does not move — while a drifted mode is still forced back
    /// to `0700`. A same-mode chmod moves the ctime on APFS, and tippy snapshots the
    /// ctime of every ancestor of the executable it runs and aborts on a change
    /// ("ancestor … changed identity or contents"): an unconditional chmod here
    /// loses a whole tippy run, since `~/Library/Application Support/aterm`, the
    /// control-socket directory, is an ancestor of the atpkg store and has its
    /// ctime moved mid-compile. aterm-update-core's copy of the rule is the same.
    #[cfg(unix)]
    #[test]
    fn an_already_private_dir_keeps_its_ctime_and_a_drifted_one_is_hardened() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = std::env::temp_dir().join(format!("agui-privdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();
        let stamp = || {
            let m = std::fs::metadata(&dir).unwrap();
            (m.ctime(), m.ctime_nsec(), m.mode() & 0o7777)
        };
        let before = stamp();
        assert_eq!(before.2, 0o700);
        ensure_private_dir(&dir).unwrap();
        assert_eq!(stamp(), before, "an unchanged directory was written");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&dir).unwrap();
        assert_eq!(stamp().2, 0o700, "a drifted mode is hardened");
        // Nothing is remembered from the calls above: a directory removed since
        // is made again, private.
        std::fs::remove_dir(&dir).unwrap();
        ensure_private_dir(&dir).unwrap();
        assert_eq!(stamp().2, 0o700, "a removed directory is made again");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
