// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Which listening socket a path names: the socket file its `bind` created (device and inode,
//! plus its birth where the filesystem keeps one, so an inode number reissued at once is
//! still told apart).

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

/// The [`ListenerId`] of the socket `path` names now, a link followed.
#[must_use]
pub fn listener_at(path: impl AsRef<Path>) -> Option<ListenerId> {
    listener_read(path).ok().flatten()
}

/// [`listener_at`] with a failed read told from an absent socket: `Ok(None)` only when the
/// read proves no socket is there (nothing at the path, a component that is no directory, or
/// a file that is no socket), and `Err` when it failed otherwise (`EIO`, `ESTALE`, `EACCES`),
/// which proves nothing either way.
///
/// # Errors
/// The read's own, when it is not the path's absence.
pub fn listener_read(path: impl AsRef<Path>) -> std::io::Result<Option<ListenerId>> {
    use std::io::ErrorKind::{NotADirectory, NotFound};
    match std::fs::metadata(path) {
        Ok(meta) => Ok(socket_file(&meta).map(|file| ListenerId {
            file,
            born: meta.created().ok(),
        })),
        Err(e) if matches!(e.kind(), NotFound | NotADirectory) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Whether `path` still names the listener `then` was read from, read now (after whatever
/// answered there answered).
#[must_use]
pub fn still_names(path: impl AsRef<Path>, then: Option<ListenerId>) -> bool {
    then.is_none_or(|then| names_now(path, then) != Named::Another)
}

/// What a path names now against a listener read there earlier ([`names_now`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Named {
    /// That listener still.
    Same,
    /// No socket: the file was unlinked, or something that is no socket is there.
    Nothing,
    /// Another socket file: a later bind at the path, or one renamed over it.
    Another,
    /// No answer: the read failed otherwise than by the path's absence ([`listener_read`]).
    Unread,
}

/// What `path` names now against `bound`, the listener a server read there right after its
/// own bind.
#[must_use]
pub fn names_now(path: impl AsRef<Path>, bound: ListenerId) -> Named {
    match listener_read(path) {
        Ok(Some(now)) if now == bound => Named::Same,
        Ok(Some(_)) => Named::Another,
        Ok(None) => Named::Nothing,
        Err(_) => Named::Unread,
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
        std::fs::hard_link(&sock, dir.join("held")).expect("held");
        std::fs::remove_file(&sock).expect("unlink");
        assert!(still_names(&sock, Some(then)), "nothing there now");
        let _second = crate::CtlListener::bind(&sock).expect("rebind");
        assert!(
            !still_names(&sock, Some(then)),
            "another bind, another listener"
        );
    }

    /// A server's own path read strictly ([`names_now`]): its listener, then nothing once the
    /// file is unlinked or a plain file stands there, then another once a second bind takes
    /// the path.
    #[test]
    fn a_server_reads_its_path_gone_or_taken() {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("uds-l.")
            .tempdir()
            .expect("scratch");
        let dir = scratch.path();
        let sock = dir.join("s.sock");
        let first = crate::CtlListener::bind(&sock).expect("bind");
        let bound = listener_at(&sock).expect("a socket");
        assert_eq!(names_now(&sock, bound), Named::Same);

        std::fs::hard_link(&sock, dir.join("held")).expect("held");
        std::fs::remove_file(&sock).expect("unlink");
        assert_eq!(names_now(&sock, bound), Named::Nothing, "unlinked");
        assert!(
            still_names(&sock, Some(bound)),
            "the lenient reading calls it the same"
        );
        std::fs::write(&sock, "").expect("a plain file in its place");
        assert_eq!(names_now(&sock, bound), Named::Nothing, "no socket");

        std::fs::remove_file(&sock).expect("unlink the plain file");
        let _second = crate::CtlListener::bind(&sock).expect("rebind");
        assert_eq!(names_now(&sock, bound), Named::Another, "another bind");
        drop(first);
    }

    /// A read that fails otherwise than by the path's absence proves nothing: a socket whose
    /// directory has lost its search bit reads [`Named::Unread`], never gone, and its own
    /// listener again once the bit is back.
    #[test]
    fn a_read_that_fails_is_no_answer() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = aterm_tempfile::Builder::new()
            .prefix("uds-l.")
            .tempdir()
            .expect("scratch");
        let dir = scratch.path().join("d");
        std::fs::create_dir(&dir).expect("dir");
        let sock = dir.join("s.sock");
        let _server = crate::CtlListener::bind(&sock).expect("bind");
        let bound = listener_at(&sock).expect("a socket");
        let mode = |bits| {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(bits)).expect("chmod");
        };
        mode(0o000);
        let denied = listener_read(&sock);
        let named = names_now(&sock, bound);
        mode(0o700);
        assert!(
            denied.is_err(),
            "a denied search fails the read: {denied:?}"
        );
        assert_eq!(named, Named::Unread, "a denied read");
        assert_eq!(names_now(&sock, bound), Named::Same, "the search bit back");
        assert_eq!(
            names_now(scratch.path().join("absent"), bound),
            Named::Nothing
        );
    }

    /// Each half of the identity tells two socket files apart where the other cannot: a fresh
    /// socket renamed over the path (`Endpoint::reopen`) born in the same tick of a coarse
    /// clock (HFS+ keeps whole seconds) is another listener by its inode number, and the same
    /// inode number born at another time (ext4 reissues one at once) is another listener by
    /// its birth.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_inode_number_or_a_birth_alone_tells_listeners_apart() {
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
