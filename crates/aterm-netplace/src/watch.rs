// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PLACE'S FILES WATCHED (ATERM-KI-1105-20). Every input of the place that is a file lives in
//! its folder (`<state>/netplace/`, or its quiet `.ubd` home on macOS): the keeper's record
//! (`place.toml`), the evidence every lane notes (`heard`, and the direct scans'
//! `observations/heard`) and the member book (`book`). Each writer replaces its file by an atomic rename
//! (`aterm_uds::private_file::write_private`), which a watch of the file hears
//! ([`aterm_uds::file_watch::FileWatch`] watches each file's folder too, and tells only a file
//! whose stamp changed). A debug build also watches the scripted-facts file its
//! [`crate::facts::SEAM`] names.
//!
//! Each owner watches exactly the files it reads ([`Reads`]): the keeper the evidence and the
//! book, never the record it writes itself, so its own write never wakes it; a reader those and
//! the record. Nothing else in the folder tells: not the home-day ledger (`home-days`, which no
//! step reads), a lock taken, or a writer's temporary file.
//!
//! [`place_files`] arms that watch BEFORE its owner first reads the files: the first look of a
//! newly watched file always tells, so a write between the owner's read and the arm is told too,
//! at the cost of one more read that finds nothing new. After that each settled burst of writes
//! tells `notify`; the owner latches it and reads again, and between writes reads nothing (no
//! timer looks for a change).
//!
//! A reader that caches the place hears the other half of its inputs with [`routes`]: the default
//! routes and the tunnel it reads live, which change with no file written whenever the keeper's
//! record stays the same (a VPN up or down within one stay turns `remote · lan` into
//! `remote · vpn`), and which no keeper writes at all while none keeps.
//!
//! Only kqueue (macOS) and inotify (Linux) arm it. Elsewhere [`place_files`] answers `None`:
//! `FileWatch`'s stat poll looks twice a second, more than the bounded slow look the owner falls
//! back to without a watch ([`crate::keeper::UNWATCHED_EVERY`]).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// Which of the place's files an owner reads, and so watches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reads {
    /// The keeper: the evidence and the book. The record is its own to write.
    Keeper,
    /// A reader: the evidence, the book and the keeper's record.
    Reader,
}

/// The scripted-facts file's key in the watch.
const SEAM_FILE: u64 = 0;

/// What [`place_files`] tells for the scripted-facts file.
pub const SCRIPTED: &str = "scripted";

/// Each watched file's key and its path in the folder.
const FILES: [(u64, &str); 4] = [
    (1, crate::heard::FILE),
    (2, "observations/heard"),
    (3, crate::heard::BOOK_FILE),
    (4, crate::record::FILE),
];

impl Reads {
    /// The files of [`FILES`] this owner reads.
    fn files(self) -> &'static [(u64, &'static str)] {
        match self {
            Self::Keeper => &FILES[..3],
            Self::Reader => &FILES,
        }
    }
}

/// A live watch of the place's files: dropping it ends the watch and joins its thread.
pub struct Watch {
    files: aterm_uds::file_watch::FileWatch,
    reads: Reads,
    /// The folder watched now ([`Watch::point`]).
    folder: Mutex<PathBuf>,
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch")
            .field("reads", &self.reads)
            .field(
                "folder",
                &*self.folder.lock().unwrap_or_else(PoisonError::into_inner),
            )
            .finish_non_exhaustive()
    }
}

impl Watch {
    /// Watch the files in `folder` instead, when it is not the one watched: a reader re-resolves
    /// the place's folder each read (`record::dir`, which the one-time quiet handoff moves), and
    /// points the watch there BEFORE it reads, so the folder it reads is the folder it hears.
    pub fn point(&self, folder: &Path) {
        let mut held = self.folder.lock().unwrap_or_else(PoisonError::into_inner);
        if held.as_path() != folder {
            folder.clone_into(&mut held);
            self.arm(folder);
        }
    }

    /// How many first looks the watch tells as it arms: one per watched file.
    #[must_use]
    pub fn first_looks(&self) -> usize {
        self.reads.files().len() + usize::from(crate::facts::seam_path().is_some())
    }

    fn arm(&self, folder: &Path) {
        for &(key, name) in self.reads.files() {
            self.files.watch(key, folder.join(name));
        }
    }
}

/// Watch the place's files in `folder` that `reads` names (and, in a debug build, the
/// scripted-facts file), telling `notify` which (its name in the folder, or [`SCRIPTED`]) at each
/// settled burst of writes to one and once at each one's first look. `None` where no file events
/// run here (off macOS and Linux) or the event source could not start (logged): the caller then
/// looks again on its bounded slow look.
#[must_use]
pub fn place_files(
    folder: &Path,
    reads: Reads,
    notify: impl Fn(&'static str) + Send + 'static,
) -> Option<Watch> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let files = match aterm_uds::file_watch::FileWatch::platform(
            "aterm-netplace-watch",
            Box::new(move |key| {
                notify(
                    FILES
                        .iter()
                        .find(|&&(file, _)| file == key)
                        .map_or(SCRIPTED, |&(_, name)| name),
                );
            }),
        ) {
            Ok(files) => files,
            Err(error) => {
                aterm_log::warn!(
                    "network place: its files cannot be watched ({error}); it looks again every \
                     {} s instead",
                    crate::keeper::UNWATCHED_EVERY.as_secs()
                );
                return None;
            }
        };
        if let Some(seam) = crate::facts::seam_path() {
            files.watch(SEAM_FILE, seam);
        }
        let watch = Watch {
            files,
            reads,
            folder: Mutex::new(folder.to_path_buf()),
        };
        watch.arm(folder);
        Some(watch)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (folder, reads, notify, SEAM_FILE, FILES);
        None
    }
}

/// A live watch of this Mac's route table for a reader ([`routes`]): dropping it stops its thread
/// (the stop's bell) and joins it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub struct Routes {
    stop: std::sync::Arc<aterm_uds::stop::Stop>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl std::fmt::Debug for Routes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Routes").finish_non_exhaustive()
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for Routes {
    fn drop(&mut self) {
        self.stop.set();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Watch the route table through `changes` (`aterm_uds::network::RouteChanges::open()`, or a
/// test's stand-in), telling `changed` at each change of network
/// ([`aterm_uds::network::RouteWake::Change`]) on a background thread that otherwise sleeps in
/// one `poll(2)` beside its stop. The socket hears every change from when it opened, so a caller
/// that opens it before its first read misses none. `lost` is told once if the socket fails
/// later, so the caller falls back to its bounded slow look. `None` where the socket did not
/// open, or its stop's bell or its thread could not be made (logged). Only macOS and Linux
/// have a route socket.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[must_use]
pub fn routes(
    changes: std::io::Result<aterm_uds::network::RouteChanges>,
    changed: impl Fn() + Send + 'static,
    lost: impl FnOnce() + Send + 'static,
) -> Option<Routes> {
    use aterm_uds::network::RouteWake;
    let changes = match changes {
        Ok(changes) => changes,
        Err(error) => {
            aterm_log::warn!(
                "network place: route changes cannot be heard ({error}); it looks again every {} \
                 s instead",
                crate::keeper::UNWATCHED_EVERY.as_secs()
            );
            return None;
        }
    };
    let stop = std::sync::Arc::new(aterm_uds::stop::Stop::new());
    if let Err(error) = stop.bell() {
        aterm_log::warn!("network place: the route watch cannot be stopped ({error})");
        return None;
    }
    let held = std::sync::Arc::clone(&stop);
    let spawned = std::thread::Builder::new()
        .name("aterm-netplace-routes".to_owned())
        .spawn(move || {
            crate::keeper::background();
            let Ok(bell) = held.bell() else {
                lost();
                return;
            };
            loop {
                match changes.wait_beside(crate::keeper::REST, Some(bell)) {
                    RouteWake::Change => changed(),
                    RouteWake::Timeout => {}
                    RouteWake::Also => return,
                    RouteWake::Failed => {
                        aterm_log::warn!(
                            "network place: the route watch failed; it looks again every {} s \
                             instead",
                            crate::keeper::UNWATCHED_EVERY.as_secs()
                        );
                        lost();
                        return;
                    }
                }
            }
        });
    match spawned {
        Ok(thread) => Some(Routes {
            stop,
            thread: Some(thread),
        }),
        Err(error) => {
            aterm_log::warn!("network place: the route watch could not start ({error})");
            None
        }
    }
}

/// A live watch of the wall clock's moves for an owner ([`wall`]): dropping it stops its thread
/// (the stop's bell) and joins it.
#[cfg(unix)]
pub struct WallMoves {
    stop: std::sync::Arc<aterm_uds::stop::Stop>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl std::fmt::Debug for WallMoves {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WallMoves").finish_non_exhaustive()
    }
}

#[cfg(unix)]
impl Drop for WallMoves {
    fn drop(&mut self) {
        self.stop.set();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Hear the wall clock's moves through `source` (`aterm_uds::wall_watch::WallWatch::start`, or
/// a test's told by hand), telling `moved` at each announced sleep's wake or wall-clock step on a
/// background thread that otherwise sleeps in one `poll(2)` beside its stop. The place's
/// deadlines are wall-clock times waited for on the monotonic clock (which stops in sleep), so a
/// move is an input change: the owner steps again at the wall's new now (ATERM-KI-1105-20).
/// `None` (the move then waits for the next change, as before) where `source` is `None` or the
/// stop's bell or the thread could not be made (logged).
#[cfg(unix)]
#[must_use]
pub fn wall(
    source: Option<std::sync::Arc<aterm_uds::wall_watch::WallWatch>>,
    moved: impl Fn() + Send + 'static,
) -> Option<WallMoves> {
    let source = source?;
    let stop = std::sync::Arc::new(aterm_uds::stop::Stop::new());
    if let Err(error) = stop.bell() {
        aterm_log::warn!("network place: the wall-clock watch cannot be stopped ({error})");
        return None;
    }
    let held = std::sync::Arc::clone(&stop);
    let spawned = std::thread::Builder::new()
        .name("aterm-netplace-wall".to_owned())
        .spawn(move || {
            crate::keeper::background();
            let Ok(bell) = held.bell() else {
                return;
            };
            loop {
                let mut fds = source.fds();
                fds.push(bell);
                if aterm_uds::ready::any(&fds, None).is_err() || held.is_set() {
                    return;
                }
                if source.take().any() {
                    moved();
                }
            }
        });
    match spawned {
        Ok(thread) => Some(WallMoves {
            stop,
            thread: Some(thread),
        }),
        Err(error) => {
            aterm_log::warn!("network place: the wall-clock watch could not start ({error})");
            None
        }
    }
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
