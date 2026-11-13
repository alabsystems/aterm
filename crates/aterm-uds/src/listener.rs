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

impl ListenerId {
    /// Identity from one already-read metadata record. In particular, metadata
    /// read without following links never gives a symlink a socket identity.
    #[must_use]
    pub fn from_metadata(metadata: &std::fs::Metadata) -> Option<Self> {
        socket_file(metadata).map(|file| Self {
            file,
            born: metadata.created().ok(),
        })
    }
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
        Ok(meta) => Ok(ListenerId::from_metadata(&meta)),
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
#[path = "listener_tests.rs"]
mod tests;
