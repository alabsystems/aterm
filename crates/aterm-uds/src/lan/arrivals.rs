// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Broker socket generations as readiness hints, each verified by the ordinary hello.

use super::{LanError, Unavailable, broker};
use crate::nofollow::{self, Kind, Watcher};
use std::io;
use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Version(u64, u64, i64, i64);

/// Hear a broker become usable at startup or after its socket is made again.
/// This is a wake hint, never admission authority: every generation must answer
/// the ordinary uid/protocol-checked hello. No connection or timer is kept once
/// that generation answered. A caller owns the watcher and its bounded waits.
pub struct Arrivals {
    socket: PathBuf,
    protocol: u32,
    watch: Option<Watcher>,
    directory: Option<OwnedFd>,
    dirty: bool,
    seen: Option<Version>,
    answered: Option<Version>,
    retry: Option<Instant>,
    delay: Duration,
}

impl Default for Arrivals {
    fn default() -> Self {
        Self::new()
    }
}

impl Arrivals {
    /// Capture this thread's broker endpoint before a worker takes the watch.
    #[must_use]
    pub fn new() -> Self {
        Self {
            socket: super::dialled_socket(),
            protocol: super::protocol(),
            watch: None,
            directory: None,
            dirty: true,
            seen: None,
            answered: None,
            retry: None,
            delay: Duration::from_secs(1),
        }
    }

    /// The native event descriptor, after the first successful [`Self::poll`].
    /// It can be waited on beside route events; this reads no notification.
    #[must_use]
    pub fn watch_fd(&self) -> Option<BorrowedFd<'_>> {
        self.watch.as_ref().map(Watcher::as_fd)
    }

    /// Bound a consumer's wait while a socket was made before its listener was
    /// ready, or its broker did not judge this process. Unchanged refused or
    /// outdated generations are not repeatedly dialled.
    #[must_use]
    pub fn wait_for(&self, most: Duration) -> Duration {
        self.retry.map_or(most, |at| {
            most.min(at.saturating_duration_since(Instant::now()))
        })
    }

    fn arm(&mut self) -> io::Result<()> {
        if self.watch.is_none() {
            self.watch = Some(Watcher::new()?);
        }
        // These descriptors only report events. Resolving an existing ancestor
        // permits /var and /tmp's system links without changing hello authority.
        for parent in self.socket.ancestors().skip(1) {
            let path = match std::fs::canonicalize(parent) {
                Ok(path) => path,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            let fd = match nofollow::open_evtonly(&path, Kind::Dir) {
                Ok(fd) => fd,
                Err(error) if nofollow::is_missing(&error) || nofollow::is_link(&error) => continue,
                Err(error) => return Err(error),
            };
            self.watch.as_ref().unwrap().add(fd.as_fd(), 1)?;
            self.directory = Some(fd);
            self.dirty = true;
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no broker socket ancestor could be watched",
        ))
    }

    /// Whether a new socket generation answered its checked hello. The watch
    /// arms before its level read, including when the broker's folder is absent.
    /// Parent churn leaves an unchanged generation alone. A failed transient hello
    /// retries with bounded backoff while the consumer keeps its ordinary pace.
    /// # Errors
    /// The native event source or metadata cannot be read. The next call retries
    /// an unarmed directory, so descriptor pressure does not permanently lose it.
    pub fn poll(&mut self) -> io::Result<bool> {
        if self.directory.is_none() {
            self.arm()?;
        }
        let events = self.watch.as_ref().unwrap().wait(Duration::ZERO)?;
        if !events.is_empty() {
            self.dirty = true;
            if events.iter().any(|event| event.gone) {
                self.directory.take();
                self.arm()?;
            } else {
                // A previously absent closer folder may now be there.
                self.arm()?;
            }
        }
        let due = self.retry.is_some_and(|at| at <= Instant::now());
        if !self.dirty && !due {
            return Ok(false);
        }
        let metadata = match std::fs::symlink_metadata(&self.socket) {
            Ok(metadata) if metadata.file_type().is_socket() => metadata,
            Ok(_) => {
                self.forget();
                return Ok(false);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                self.forget();
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        self.dirty = false;
        let version = Version(
            metadata.dev(),
            metadata.ino(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        );
        let changed = self.seen != Some(version);
        if changed {
            self.seen = Some(version);
            self.delay = Duration::from_secs(1);
        }
        if self.answered == Some(version) {
            self.retry = None;
            return Ok(false);
        }
        if !changed && !due {
            return Ok(false);
        }
        match broker::hello_at(&self.socket, self.protocol) {
            Ok(_) => {
                self.answered = Some(version);
                self.retry = None;
                Ok(true)
            }
            // A refusal of this code and another protocol stand for this generation.
            Err(LanError::Unavailable(Unavailable::Outdated { .. } | Unavailable::Refused(_))) => {
                self.retry = None;
                Ok(false)
            }
            // No listener yet, a broker that did not judge this process (its code check did not
            // finish, or it was serving its most connections), or a failed exchange.
            Err(LanError::Unavailable(Unavailable::NotInstalled | Unavailable::Unchecked(_)))
            | Err(LanError::Io(_)) => {
                self.retry = Some(Instant::now() + self.delay);
                self.delay = (self.delay * 2).min(Duration::from_secs(30));
                Ok(false)
            }
        }
    }

    fn forget(&mut self) {
        self.dirty = false;
        self.seen = None;
        self.answered = None;
        self.retry = None;
    }
}
