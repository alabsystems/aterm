// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unix implementations behind [`control_auth`](super): POSIX
//! private-dir/mode provisioning, `getentropy` token minting, the `latest`
//! symlink, and the `getpeereid`/`SO_PEERCRED` same-uid peer gate. Moved
//! VERBATIM from `control_auth.rs` in the Windows-port module split — zero
//! semantic change on Unix.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use aterm_uds::CtlStream;

/// Create `dir` (and parents) if absent, force its mode to `0700` when it is not
/// already that, and VERIFY it is owned by us and not group/other-writable before
/// returning success.
///
/// SEC-3: forcing the mode to 0700 is not enough on its own — if `dir` already
/// existed and is owned by ANOTHER user (an attacker who pre-created
/// `$XDG_RUNTIME_DIR/aterm`), our `set_permissions` does not change its owner,
/// and that user could still have planted contents or could swap files in. After
/// ensuring the directory exists and tightening the mode, we `stat` it and apply
/// the same owned-and-unshared predicate the snapshot path uses
/// ([`aterm_types::fs_restricted::dir_safe_for_private_write`]); a foreign-owned
/// or group/other-writable directory is REFUSED (fail closed) rather than
/// provisioned into.
pub(crate) fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    std::fs::create_dir_all(dir)?;
    // Only a mode that is not already `0700` is written: a `chmod(2)` to the
    // mode a directory already has still moves its `st_ctime` (APFS), and
    // tippy aborts when an ancestor of its executable changes ctime mid-run
    // — aterm-update-core's copy of this rule learned that on 2026-09-15
    // (privatedir.rs). The stat below still proves the final mode.
    if !std::fs::metadata(dir).is_ok_and(|m| m.mode() & 0o7777 == 0o700) {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    // Ownership-safety gate: stat AFTER tightening, then verify owner == us and
    // no group/other write bits. set_permissions cannot fix a foreign owner.
    let meta = std::fs::metadata(dir)?;
    let safe =
        aterm_types::fs_restricted::dir_safe_for_private_write(our_uid(), meta.uid(), meta.mode());
    if safe {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{}: control directory must be owned by uid {} and not group/other-writable",
                dir.display(),
                our_uid()
            ),
        ))
    }
}

/// Pid liveness via `kill(pid, 0)`: delivery permission is checked without
/// sending anything, and only `ESRCH` — no such process — means "dead"; 0,
/// `EPERM` and any other answer mean "alive", because a sweep that removes a
/// live instance's files breaks it while one that keeps a dead one's costs a
/// file. Pids that cannot be real (0, or wider than `pid_t`) are dead — files
/// naming them are garbage.
pub(crate) fn pid_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Atomically (re)point the `latest` symlink at this instance's socket:
/// symlink to the RELATIVE sock filename under a temp name, then rename over
/// the link, so a client never observes a missing link and the newest
/// instance always wins. Best-effort: on failure clients can still target the
/// instance socket directly (`aterm-ctl --pid`). The one implementation is
/// [`aterm_uds::latest::publish`], shared with the Windows pointer-file twin.
pub(crate) fn publish_latest_link(link: &Path, sock_path: &str) {
    aterm_uds::latest::publish(link, sock_path);
}

/// Generate 32 random bytes and return them as a 64-char lowercase hex string,
/// via the workspace's ONE audited entropy surface ([`aterm_uds::rand`]:
/// `getentropy(2)` with a BOUNDED `/dev/urandom` `read_exact` fallback — never
/// a hand-rolled device read, which is exactly the pattern that caused the
/// 2026-07 kernel panics). Returns `None` when no entropy source is available;
/// the caller MUST then refuse to start the socket rather than serve a
/// guessable token (fail closed).
#[must_use]
pub(crate) fn random_token_hex() -> Option<String> {
    aterm_uds::rand::hex_token::<32>().ok()
}

/// Provision the capability token: generate a fresh token, write it to
/// `path` at mode `0600` (truncating any prior token), and return the hex
/// string. The token rotates every launch — a leaked token from a prior run
/// is worthless.
///
/// Returns `None` when entropy is unavailable or the file cannot be written; a
/// `None` here MUST make the caller skip binding the socket (fail closed).
#[must_use]
pub(crate) fn provision_token(path: &Path) -> Option<String> {
    let token = random_token_hex()?;
    // SEC-3: create the token EXCLUSIVELY and refuse to follow a symlink.
    // Remove any prior file first (a stale token from our own previous run, or
    // an attacker-planted file/symlink at this path), then `O_CREAT|O_EXCL|
    // O_NOFOLLOW`: O_EXCL means we only ever write a file WE just created (never
    // through a pre-existing symlink or someone else's file), and O_NOFOLLOW
    // refuses a symlink even racing the unlink. The token is thus never written
    // through an attacker-controlled path, and never briefly world-readable.
    let _ = std::fs::remove_file(path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true).mode(0o600);
    opts.custom_flags(libc::O_NOFOLLOW);
    let f = opts.open(path).ok()?;
    // Force 0600 via the OPEN fd (`fchmod`), never a path-based set_permissions
    // that would re-resolve (and could follow) the path.
    f.set_permissions(std::fs::Permissions::from_mode(0o600))
        .ok()?;
    let mut f = f;
    f.write_all(token.as_bytes()).ok()?;
    f.flush().ok()?;
    Some(token)
}

/// Tighten the bound socket file to mode `0600` so only the owner can connect
/// even if it somehow lands in a shared directory. Best-effort: a failure here
/// still leaves the directory perms + peer check + token in force.
pub(crate) fn lock_socket_file(path: &str) {
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

/// The connecting peer's effective uid via `getpeereid(2)`, or `None` if the
/// call fails (e.g. the peer already vanished). macOS/BSD path; Linux can use
/// `SO_PEERCRED`, added below for portability of the test/CI matrix.
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[must_use]
pub(crate) fn peer_uid(stream: &CtlStream) -> Option<u32> {
    use std::os::unix::io::AsRawFd;
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if rc == 0 { Some(uid) } else { None }
}

/// Linux peer-uid via `SO_PEERCRED` (`struct ucred`). Present so the same auth
/// path compiles and is exercised off macOS; macOS remains the target.
#[cfg(target_os = "linux")]
#[must_use]
pub(crate) fn peer_uid(stream: &CtlStream) -> Option<u32> {
    use std::os::unix::io::AsRawFd;
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast::<libc::c_void>(),
            &mut len,
        )
    };
    if rc == 0 { Some(cred.uid) } else { None }
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

/// The accept-time peer gate: refuse any connection NOT from our own uid.
/// `None` (cannot verify) also refuses — fail closed. The `Err` carries the
/// exact denial text the accept loop has always audit-logged.
pub(crate) fn peer_check(stream: &CtlStream) -> Result<(), String> {
    let our_uid = our_uid();
    match peer_uid(stream) {
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
    /// ("ancestor … changed identity or contents"). This copy of the rule kept the
    /// unconditional chmod after aterm-update-core's copy dropped it on 2026-09-15,
    /// and on 2026-09-24 a merge-contract run lost its whole tippy stage to it:
    /// `~/Library/Application Support/aterm`, the control-socket directory and an
    /// ancestor of the atpkg store, had its ctime moved mid-compile.
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
        std::thread::sleep(std::time::Duration::from_millis(20));
        ensure_private_dir(&dir).unwrap();
        assert_eq!(stamp(), before, "an unchanged directory was written");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&dir).unwrap();
        assert_eq!(stamp().2, 0o700, "a drifted mode is hardened");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
