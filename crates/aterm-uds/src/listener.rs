// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHICH LISTENING SOCKET a path names: the socket file its `bind` created —
//! device and inode, and its birth where the filesystem keeps one, so an inode
//! number handed out again at once is still told apart. Every bind makes a new
//! one.
//!
//! A PID ALONE DOES NOT NAME A SERVER (2026-09-30): aterm-gui's cold update lane
//! closes its listening socket and shuts down every stream it accepted
//! (`Endpoint::retire`) right before an `exec` that KEEPS the pid; the exec'd
//! image binds the same path (`aterm-<pid>.sock`, or the `--control-sock` its
//! argv carries), and an `exec` that returned reopens it in the same process
//! (`Endpoint::reopen`: a fresh socket renamed over the path). Judged by pid,
//! that process answering was the server that ended the stream on purpose when
//! a client looked once it was up, and never a successor when it looked
//! before. Both sides of a hang-up judge it with this identity: `aterm ctl`'s
//! redial (whether the server it reached still answers) and aterm-gui's forward
//! relay (whether the host behind a forward still serves where it was reached).
//!
//! One fresh listener cuts nothing: the watchdog's rebind of a wedged one
//! (aterm-gui's `control_listener`) leaves every stream it accepted served. A
//! stream that server then ends on purpose, without saying its sessions
//! exited, is asked once more of that same process, which takes it or refuses
//! it — never another aterm.

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

/// The [`ListenerId`] of the socket `path` names now, a link followed. Read
/// once a connection through `path` is up, it names the listener that accepted
/// it — unless one was renamed over the path in between, which errs toward the
/// same listener, the verdict before this identity was read. `None` when
/// nothing is there, it is not a socket, or the platform names no socket file
/// (every one but Unix): the pid alone then decides.
#[must_use]
pub fn listener_at(path: impl AsRef<Path>) -> Option<ListenerId> {
    let meta = std::fs::metadata(path).ok()?;
    Some(ListenerId {
        file: socket_file(&meta)?,
        born: meta.created().ok(),
    })
}

/// Whether `path` still names the listener `then` was read from — read now,
/// after whatever answered there answered. `true` when either reading is
/// missing: the pid alone then decides, as it did before this identity.
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
