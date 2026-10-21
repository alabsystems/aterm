// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Which listening socket a path names: the socket file its `bind` created (device and inode,
//! plus its birth where the filesystem keeps one, so an inode number reissued at once is
//! still told apart). Every bind makes a new one, except two files born in one tick of the
//! filesystem's clock under one inode number: ext4 stamps births from the kernel's coarse
//! clock and reissues a freed inode number at once, so a socket unlinked within a tick of its
//! own bind and bound again reads as the same listener (measured on Linux 6.8 ext4: the inode
//! number in 50 of 50 rebinds, the birth too in most; on tmpfs, where `$XDG_RUNTIME_DIR`
//! puts the socket, neither).
//!
//! A pid alone does not name a server: aterm-gui's cold update lane closes its listener and
//! every stream it accepted (`Endpoint::retire`) right before an `exec` that keeps the pid, and
//! the new image binds the same path (if the `exec` returns, `Endpoint::reopen` renames a fresh
//! socket over it). Both sides of a hang-up judge it by this identity: `aterm ctl`'s redial
//! (whether the server it reached still answers) and aterm-gui's forward relay (whether the
//! host behind a forward still serves where it was reached).
//!
//! The watchdog's rebind of a wedged listener (aterm-gui's `control_listener`) leaves every
//! stream it accepted served; a stream that server then ends without saying its sessions
//! exited is asked once more of that same process, never another aterm.

use std::path::Path;
use std::time::SystemTime;

/// The listening socket a path named when it was read ([`listener_at`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListenerId {
    /// `(st_dev, st_ino)` of the socket file.
    file: (u64, u64),
    /// Its birth, where the filesystem records one.
    born: Option<SystemTime>,
}

/// The [`ListenerId`] of the socket `path` names now, a link followed. Read once a connection
/// through `path` is up, it names the listener that accepted it, unless one was renamed over
/// the path in between (which errs toward "the same listener"). `None` when nothing is there,
/// it is not a socket, or the platform names no socket file (all but Unix).
#[must_use]
pub fn listener_at(path: impl AsRef<Path>) -> Option<ListenerId> {
    let meta = std::fs::metadata(path).ok()?;
    Some(ListenerId {
        file: socket_file(&meta)?,
        born: meta.created().ok(),
    })
}

/// Whether `path` still names the listener `then` was read from, read now (after whatever
/// answered there answered). `true` when either reading is missing: the pid alone decides.
#[must_use]
pub fn still_names(path: impl AsRef<Path>, then: Option<ListenerId>) -> bool {
    match (then, listener_at(path)) {
        (Some(then), Some(now)) => then == now,
        _ => true,
    }
}

#[cfg(unix)]
fn socket_file(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
    meta.file_type()
        .is_socket()
        .then(|| (meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn socket_file(_: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Set the access and modification times of what `path` names to `secs` (and 123 ns).
    /// Darwin also moves the birth back to a time set here that is earlier; nothing moves it
    /// forward.
    fn set_times(path: &Path, secs: i64) {
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).expect("path");
        let t = libc::timespec {
            tv_sec: secs,
            tv_nsec: 123,
        };
        // SAFETY: a NUL-terminated path and two initialized timespecs, live for the call.
        let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), [t, t].as_ptr(), 0) };
        assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
    }

    /// A path names the socket bound there, through a link too, and nothing else; it still
    /// names it, new times on the file included, until another bind, and a reading that is
    /// missing on either side leaves the pid to decide.
    #[test]
    fn a_path_names_its_socket_until_another_bind() {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("uds-l.")
            .tempdir()
            .expect("scratch");
        let dir = scratch.path();
        let sock = dir.join("s.sock");
        let plain = dir.join("plain");
        std::fs::write(&plain, "").expect("file");
        assert_eq!(listener_at(&plain), None, "not a socket");
        assert_eq!(listener_at(dir.join("absent")), None);

        let first = crate::CtlListener::bind(&sock).expect("bind");
        let then = listener_at(&sock).expect("a socket");
        let link = dir.join("link");
        std::os::unix::fs::symlink(&sock, &link).expect("symlink");
        assert_eq!(listener_at(&link), Some(then), "a link is followed");
        assert!(still_names(&sock, Some(then)));
        assert!(still_names(&sock, None), "no earlier reading");
        set_times(&sock, 4_000_000_000);
        assert!(still_names(&sock, Some(then)), "new times are no new bind");

        drop(first);
        // The old file kept by a second name, so the next bind cannot be born in its tick
        // under its inode number (the blind spot the module names, which ext4 meets here).
        std::fs::hard_link(&sock, dir.join("held")).expect("held");
        std::fs::remove_file(&sock).expect("unlink");
        assert!(still_names(&sock, Some(then)), "nothing there now");
        let _second = crate::CtlListener::bind(&sock).expect("rebind");
        assert!(
            !still_names(&sock, Some(then)),
            "another bind, another listener"
        );
    }

    /// Each half of the identity tells two socket files apart where the other cannot: a fresh
    /// socket renamed over the path (`Endpoint::reopen`) born in the same tick of a coarse
    /// clock (HFS+ keeps whole seconds) is another listener by its inode number, and the same
    /// inode number born at another time (ext4 reissues one at once) is another listener by
    /// its birth. Darwin moves a birth back to an earlier time `utimes` sets, which stages both.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_inode_number_or_a_birth_alone_tells_listeners_apart() {
        // A short prefix: the scratch's name ends in this process's count of temporary
        // directories, and a socket path must fit `sun_path`'s 104 bytes however many
        // the binary's other tests made first.
        let scratch = aterm_tempfile::Builder::new()
            .prefix("uds-l.")
            .tempdir()
            .expect("scratch");
        let sock = scratch.path().join("s.sock");
        let fresh = scratch.path().join("fresh.sock");
        let _first = crate::CtlListener::bind(&sock).expect("bind");
        let _second = crate::CtlListener::bind(&fresh).expect("bind");
        set_times(&sock, 1000);
        set_times(&fresh, 1000);
        let then = listener_at(&sock).expect("a socket");
        assert_eq!(listener_at(&fresh).map(|id| id.born), Some(then.born));
        std::fs::rename(&fresh, &sock).expect("renamed over");
        assert!(
            !still_names(&sock, Some(then)),
            "another inode, born in the same tick"
        );

        let now = listener_at(&sock);
        set_times(&sock, 999);
        assert_eq!(listener_at(&sock).map(|id| id.file), now.map(|id| id.file));
        assert!(
            !still_names(&sock, now),
            "the same inode number, born at another time"
        );
    }
}
