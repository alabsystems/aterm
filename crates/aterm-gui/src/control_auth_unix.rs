// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unix implementations behind [`control_auth`](super).

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use aterm_uds::CtlStream;

/// Create `dir` (and parents) if absent, force its mode to `0700`, and verify it is owned by us and
/// not group/other-writable.
pub(crate) fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    ensure_private_dir_as(dir, our_uid())
}

/// [`ensure_private_dir`] with the uid that must own `dir` given, so a test can stand as a stranger
/// to a directory it really owns: the one way, unprivileged, to reach the SEC-3 refusal of a
/// foreign-owned `0700` directory.
pub(super) fn ensure_private_dir_as(dir: &Path, owner: u32) -> std::io::Result<()> {
    if already_private(dir, owner) {
        return Ok(());
    }
    match aterm_types::fs_restricted::create_dir_all_unshared(dir, 0o700) {
        Ok(()) => {}
        Err(error)
            if error.kind() == std::io::ErrorKind::AlreadyExists
                && std::fs::symlink_metadata(dir).is_ok_and(|meta| !meta.file_type().is_dir()) =>
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{}: must be a real directory, not a link", dir.display()),
            ));
        }
        Err(error) => return Err(error),
    }
    tighten_and_verify(dir, owner)
}

fn already_private(dir: &Path, owner: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(dir).is_ok_and(|meta| {
        meta.file_type().is_dir()
            && meta.mode() & 0o7777 == 0o700
            && aterm_types::fs_restricted::dir_safe_for_private_write(
                owner,
                meta.uid(),
                meta.mode(),
            )
    })
}

fn tighten_and_verify(dir: &Path, owner: u32) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let refused = || {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{}: control directory must be owned by uid {} and not group/other-writable",
                dir.display(),
                owner
            ),
        )
    };
    let before = std::fs::symlink_metadata(dir)?;
    if !before.file_type().is_dir() || before.uid() != owner {
        return Err(refused());
    }
    if before.mode() & 0o7777 != 0o700 {
        let dir_fd = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(dir)?;
        if dir_fd.metadata()?.uid() != owner {
            return Err(refused());
        }
        dir_fd.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    }
    let meta = std::fs::symlink_metadata(dir)?;
    let safe = meta.file_type().is_dir()
        && aterm_types::fs_restricted::dir_safe_for_private_write(owner, meta.uid(), meta.mode());
    if safe { Ok(()) } else { Err(refused()) }
}

/// Pid liveness: a pid that cannot be real (0, or wider than `pid_t`) is dead; otherwise
/// `kill(pid, 0)` decides, and only `ESRCH` means dead.
pub(crate) fn pid_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Write a fresh capability token to `path` at mode `0600` and return its hex.
#[must_use]
pub(crate) fn provision_token(path: &Path) -> Option<String> {
    let token = super::random_token_hex()?;
    let _ = std::fs::remove_file(path);
    write_fresh_token(path, &token)?;
    Some(token)
}

/// The exclusive write behind [`provision_token`], once the unlink is done.
pub(super) fn write_fresh_token(path: &Path, token: &str) -> Option<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true).mode(0o600);
    opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let f = opts.open(path).ok()?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))
        .ok()?;
    let mut f = f;
    f.write_all(token.as_bytes()).ok()?;
    f.flush().ok()
}

/// A minute hang detector for another launch's ownership probe to release its temporary lock.
/// A descheduled probe must not discard the serving instance's ownership evidence.
const SOCKET_OWNER_HANG: std::time::Duration = std::time::Duration::from_secs(60);

/// This instance's hold on its control socket while it serves it: the `flock` on the socket's
/// token file, which [`socket_is_live`](super::socket_is_live) probes. A stopped or unscheduled
/// instance refuses a connect once its queue fills, exactly as a dead socket does; the lock tells
/// them apart, and the kernel lets go when the process ends, however it ends.
#[derive(Debug)]
pub(crate) struct SocketOwner {
    _held: crate::native_document_host::HeldAdvisoryLock,
}

impl SocketOwner {
    /// Take the lock on the token file this instance just wrote: `None`, holding nothing, if it
    /// cannot be taken (a filesystem with no `flock`), which leaves the socket judged by a connect
    /// alone.
    pub(crate) fn hold(token_path: &Path) -> Option<Self> {
        Self::hold_at(
            token_path,
            std::time::Instant::now(),
            crate::native_document_host::acquire_advisory_lock_within,
        )
    }

    fn hold_at(
        token_path: &Path,
        started: std::time::Instant,
        acquire: impl FnOnce(
            std::fs::File,
            std::time::Duration,
        ) -> Result<
            crate::native_document_host::HeldAdvisoryLock,
            crate::native_document_host::AdvisoryLockRefusal,
        >,
    ) -> Option<Self> {
        use crate::native_document_host::AdvisoryLockRefusal;
        let taken = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(token_path)
            .map_err(AdvisoryLockRefusal::Failed)
            .and_then(|file| acquire(file, SOCKET_OWNER_HANG.saturating_sub(started.elapsed())));
        let why = match taken {
            Ok(held) => return Some(Self { _held: held }),
            Err(AdvisoryLockRefusal::Busy) => "another holder".to_string(),
            Err(AdvisoryLockRefusal::Failed(error)) => error.to_string(),
        };
        aterm_log::info!(
            "control socket owner lock on {} not held: {why}",
            token_path.display()
        );
        None
    }
}

/// Tighten the bound socket file to mode `0600` so only the owner can connect even if it somehow
/// lands in a shared directory.
pub(crate) fn lock_socket_file(path: &str) {
    let _ = aterm_uds::atomic_file::set_mode_nofollow(Path::new(path), 0o600);
}

/// The connecting peer's effective uid (`getpeereid(2)` on macOS/BSD, `SO_PEERCRED` on Linux) via
/// `aterm_uds::peer`, or `None` if the kernel will not say.
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
#[must_use]
pub(crate) fn peer_uid(stream: &CtlStream) -> Option<u32> {
    aterm_uds::peer::peer_uid(stream)
}

/// Other Unixes: no portable peer-cred primitive wired here.
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

/// The accept-time peer gate: refuse any connection not from our own uid, including one whose uid
/// cannot be read (fail closed).
pub(crate) fn peer_check(stream: &CtlStream) -> Result<(), String> {
    peer_verdict(peer_uid(stream), our_uid())
}

/// [`peer_check`]'s decision over the uid the kernel reported (`None` when it would not say).
pub(super) fn peer_verdict(peer: Option<u32>, our_uid: u32) -> Result<(), String> {
    match peer {
        Some(uid) if uid == our_uid => Ok(()),
        other => Err(format!("connect (peer uid {other:?} != {our_uid})")),
    }
}

#[cfg(test)]
#[path = "control_auth_unix_socket_owner_tests.rs"]
mod socket_owner_tests;

#[cfg(test)]
#[path = "control_auth_unix_ctime_tests.rs"]
mod ctime_tests;
