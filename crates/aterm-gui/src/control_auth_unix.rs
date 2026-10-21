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

/// [`ensure_private_dir`] for a directory that must already be there: the same
/// fresh-stat fast path, `0700` and SEC-3 gate, but nothing is created. A
/// missing `dir` is `NotFound` and something there that is no directory, a
/// symlink to nothing included, is `NotADirectory` (`super::require_dir`). A
/// session identity's admission hardens the folders of an identity it did not
/// make with this (`agent_identity::harden_identity`), so a folder that goes
/// between its look and its hardening is never made again by a create here.
pub(crate) fn harden_private_dir(dir: &Path) -> std::io::Result<()> {
    if already_private(dir, our_uid()) {
        return Ok(());
    }
    super::require_dir(dir)?;
    tighten_and_verify(dir, our_uid())
}

/// Whether `dir` already IS what [`ensure_private_dir`] and
/// [`harden_private_dir`] return success for — a directory, exactly `0700`,
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
    // Ownership-safety gate: stat AFTER tightening, then verify owner == us and
    // no group/other write bits. set_permissions cannot fix a foreign owner.
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

/// Provision the capability token: generate a fresh token, write it to
/// `path` at mode `0600` (truncating any prior token), and return the hex
/// string. The token rotates every launch — a leaked token from a prior run
/// is worthless.
///
/// Returns `None` when entropy is unavailable or the file cannot be written; a
/// `None` here MUST make the caller skip binding the socket (fail closed).
#[must_use]
pub(crate) fn provision_token(path: &Path) -> Option<String> {
    let token = super::random_token_hex()?;
    // SEC-3: create the token EXCLUSIVELY and refuse to follow a symlink.
    // Remove any prior file first (a stale token from our own previous run, or
    // an attacker-planted file/symlink at this path), then `O_CREAT|O_EXCL|
    // O_NOFOLLOW`: O_EXCL means we only ever write a file WE just created (never
    // through a pre-existing symlink or someone else's file), and O_NOFOLLOW
    // refuses a symlink even racing the unlink. The token is thus never written
    // through an attacker-controlled path, and never briefly world-readable.
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
    // Force 0600 via the OPEN fd (`fchmod`), never a path-based set_permissions
    // that would re-resolve (and could follow) the path.
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

/// The connecting peer's effective uid (`getpeereid(2)` on macOS/BSD,
/// `SO_PEERCRED` on Linux), or `None` if the kernel will not say. Moved to
/// `aterm_uds::peer` (2026-09-28), shared with the PTY keeper.
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

/// The accept-time peer gate: refuse any connection NOT from our own uid.
/// `None` (cannot verify) also refuses — fail closed. The `Err` carries the
/// exact denial text the accept loop has always audit-logged.
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
