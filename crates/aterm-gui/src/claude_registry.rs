// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE REGISTRY BY PUSH (`docs/DESIGN-card-engine-2026-10-19.md` §3.10).

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use atpkg::dirwatch::{WatchId, Watcher};

/// The forwarding thread's name.
pub(crate) const THREAD: &str = "aterm-claude-registry";

/// The registry directory under a Claude dir.
const SESSIONS: &str = "sessions";

/// Where the forwarder tells what fired.
pub(crate) type Sink = Arc<dyn Fn(Told) -> bool + Send + Sync>;

/// What one wait of the forwarder tells, per fired watch.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Told {
    /// A watch on this Claude dir's `sessions/`, or on a row in it, fired.
    Rows(PathBuf),
    /// A watch on a level outside a `sessions/` fired.
    Level,
}

/// One awaited process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Awaited {
    pub(crate) dir: PathBuf,
    pub(crate) pid: u32,
}

/// `pid`'s registry row under the Claude dir `dir`.
pub(crate) fn row_of(dir: &Path, pid: u32) -> PathBuf {
    dir.join(SESSIONS).join(format!("{pid}.json"))
}

/// What a watch was put on: the object's device and inode, so a level or a
/// file replaced under its watch is told apart from the one watched.
type Ident = (u64, u64);

/// The identity of the directory at `path` (a symlinked one: the directory
/// it names).
fn dir_ident(path: &Path) -> Option<Ident> {
    std::fs::metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_dir)
        .and_then(|meta| ident(&meta))
}

/// The identity of the regular file at `path`, never through a link.
fn file_ident(path: &Path) -> Option<Ident> {
    std::fs::symlink_metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_file)
        .and_then(|meta| ident(&meta))
}

#[cfg(unix)]
fn ident(meta: &std::fs::Metadata) -> Option<Ident> {
    use std::os::unix::fs::MetadataExt as _;
    Some((meta.dev(), meta.ino()))
}

/// No arm watches anything here ([`Watcher::new`] answers `Unsupported`), so
/// nothing is ever placed.
#[cfg(not(unix))]
fn ident(_meta: &std::fs::Metadata) -> Option<Ident> {
    None
}

/// The watch over the registry rows the resolver awaits (module docs).
pub(crate) struct RegistryWatch {
    /// Makes the watcher ([`Watcher::new`]; a test's refusal disables it).
    open: fn() -> io::Result<Watcher>,
    sink: Sink,
    /// The watcher and its forwarder, while anything is awaited.
    live: Option<Live>,
    places: Places,
    /// The watcher was refused once (logged once, asked again each turn).
    refused: bool,
}

/// Where each watch is.
#[derive(Default)]
struct Places {
    /// Per Claude dir, its one directory watch.
    dirs: HashMap<PathBuf, Placed>,
    /// Per awaited row, its file watch.
    files: HashMap<PathBuf, Filed>,
}

/// A Claude dir's directory watch: on `level`, which is its `sessions`
/// directory or the nearest ancestor of it that is there.
struct Placed {
    level: PathBuf,
    id: WatchId,
    ident: Ident,
}

/// An awaited row's file watch.
struct Filed {
    id: WatchId,
    ident: Ident,
}

/// Where a watch's events go: the Claude dir it serves, and whether it
/// watches that dir's rows (`sessions/` itself, or a row) or a level outside
/// them.
struct Route {
    claude: PathBuf,
    rows: bool,
}

impl Route {
    fn told(&self) -> Told {
        if self.rows {
            Told::Rows(self.claude.clone())
        } else {
            Told::Level
        }
    }
}

/// The watcher, the route of each watch, and the forwarder reading them.
struct Live {
    watcher: Arc<Watcher>,
    /// The forwarder's map from a fired watch to what it tells. Held across
    /// each registration and its route, so an event that fires at once is
    /// not missed for want of its route.
    routes: Arc<Mutex<HashMap<WatchId, Route>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// The forwarder's finished waits ([`Forwarder::waits`]), read by
    /// registry_tests' platforms alone, as the helpers below are.
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    waits: Arc<AtomicU64>,
}

/// The forwarder's share of [`Live`]: the thread's loop ([`Self::run`]).
struct Forwarder {
    watcher: Arc<Watcher>,
    routes: Arc<Mutex<HashMap<WatchId, Route>>>,
    stop: Arc<AtomicBool>,
    sink: Sink,
    /// Each wait counted once what it fired is told: how a test sees a wait
    /// that told nothing, which no sink shows.
    #[cfg(test)]
    waits: Arc<AtomicU64>,
}

/// Which kind of watch to register.
#[derive(Clone, Copy)]
enum Kind {
    Dir,
    File,
}

impl Live {
    fn start(open: fn() -> io::Result<Watcher>, sink: &Sink) -> io::Result<Live> {
        let watcher = Arc::new(open()?);
        let routes: Arc<Mutex<HashMap<WatchId, Route>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        #[cfg(test)]
        let waits = Arc::new(AtomicU64::new(0));
        let forwarder = Forwarder {
            watcher: Arc::clone(&watcher),
            routes: Arc::clone(&routes),
            stop: Arc::clone(&stop),
            sink: Arc::clone(sink),
            #[cfg(test)]
            waits: Arc::clone(&waits),
        };
        let thread =
            crate::qos::spawn_lane_thread(THREAD, crate::qos::Role::Responsive, move || {
                forwarder.run();
            })?;
        Ok(Live {
            watcher,
            routes,
            stop,
            thread: Some(thread),
            #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
            waits,
        })
    }

    /// Register `path` and route it to the Claude dir `claude`: to its rows
    /// when `rows` (its `sessions/`, or a row), else as a level outside them.
    fn add(&self, claude: &Path, path: &Path, kind: Kind, rows: bool) -> io::Result<WatchId> {
        let mut routes = self.routes.lock().unwrap_or_else(|p| p.into_inner());
        let id = match kind {
            Kind::Dir => self.watcher.add_dir(path),
            Kind::File => self.watcher.add_file(path),
        }?;
        routes.insert(
            id,
            Route {
                claude: claude.to_path_buf(),
                rows,
            },
        );
        Ok(id)
    }

    fn remove(&self, id: WatchId) {
        self.routes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id);
        self.watcher.remove(id);
    }

    /// The forwarder ended on its own.
    fn ended(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for Live {
    /// Stop the forwarder and wait for it: the watcher closes with the last
    /// handle, which the forwarder holds until it returns.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Err(e) = self.watcher.wake() {
            aterm_log::warn!("claude registry: could not wake the watch thread: {e}");
        }
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            aterm_log::warn!("claude registry: the watch thread panicked");
        }
    }
}

impl Forwarder {
    /// The forwarder's loop: wait with no timeout, then tell the sink what
    /// the fired watches route to, each once per wait.
    fn run(&self) {
        loop {
            let fired = match self.watcher.wait(None) {
                Ok(fired) => fired,
                Err(e) => {
                    aterm_log::warn!("claude registry: the watch failed: {e}");
                    return;
                }
            };
            if self.stop.load(Ordering::Acquire) {
                return;
            }
            let told: BTreeSet<Told> = {
                let routes = self.routes.lock().unwrap_or_else(|p| p.into_inner());
                fired
                    .iter()
                    .filter_map(|id| routes.get(id).map(Route::told))
                    .collect()
            };
            for told in told {
                if !(self.sink)(told) {
                    return;
                }
            }
            #[cfg(test)]
            self.waits.fetch_add(1, Ordering::Release);
        }
    }
}

impl RegistryWatch {
    /// A watch that opens its watcher with `open` and tells `sink`.
    pub(crate) fn new(open: fn() -> io::Result<Watcher>, sink: Sink) -> RegistryWatch {
        RegistryWatch {
            open,
            sink,
            live: None,
            places: Places::default(),
            refused: false,
        }
    }

    /// Watch exactly what `awaited` needs (per Claude dir, one watch on `sessions/` or else its
    /// nearest existing ancestor, and one on each awaited row in it), and return the Claude dirs
    /// where a watch was newly placed.
    pub(crate) fn reconcile(&mut self, awaited: &[Awaited]) -> Vec<PathBuf> {
        if awaited.is_empty() {
            self.release();
            return Vec::new();
        }
        if self.live.as_ref().is_some_and(Live::ended) {
            aterm_log::warn!("claude registry: the watch thread ended; starting it again");
            self.release();
        }
        if self.live.is_none() {
            match Live::start(self.open, &self.sink) {
                Ok(live) => {
                    self.live = Some(live);
                    self.refused = false;
                }
                Err(e) => {
                    if !self.refused {
                        aterm_log::info!(
                            "claude registry: no watch ({e}); reads keep their clocks"
                        );
                        self.refused = true;
                    }
                    return Vec::new();
                }
            }
        }
        let Some(live) = self.live.as_ref() else {
            return Vec::new();
        };
        self.places.keep_only(live, awaited);
        self.places.place(live, awaited)
    }

    /// Let go of the watches `awaited` no longer needs, placing none.
    pub(crate) fn let_go(&mut self, awaited: &[Awaited]) {
        if awaited.is_empty() {
            self.release();
        } else if let Some(live) = self.live.as_ref() {
            self.places.keep_only(live, awaited);
        }
    }

    /// Every watch, the watcher and the forwarder go.
    fn release(&mut self) {
        self.places = Places::default();
        self.live = None;
    }

    /// Whether the watcher and its forwarder are up.
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    pub(crate) fn is_live(&self) -> bool {
        self.live.is_some()
    }

    /// The level the Claude dir `claude`'s directory watch is on.
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    pub(crate) fn level_of(&self, claude: &Path) -> Option<PathBuf> {
        self.places
            .dirs
            .get(claude)
            .map(|placed| placed.level.clone())
    }

    /// Whether the row `row` has a file watch.
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    pub(crate) fn file_watched(&self, row: &Path) -> bool {
        self.places.files.contains_key(row)
    }

    /// Remove `claude`'s directory watch and `row`'s file watch from the watcher behind this watch's back.
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    pub(crate) fn lose_watches(&self, claude: &Path, row: &Path) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        if let Some(placed) = self.places.dirs.get(claude) {
            live.watcher.remove(placed.id);
        }
        if let Some(filed) = self.places.files.get(row) {
            live.watcher.remove(filed.id);
        }
    }

    /// Wake the forwarder with nothing changed, and wait (a hang detector) until it has finished that wait.
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    pub(crate) fn wake_and_settle(&self) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let before = live.waits.load(Ordering::Acquire);
        live.watcher.wake().expect("wake the forwarder");
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the forwarder finishes the wait it was woken from",
            || (live.waits.load(Ordering::Acquire) > before).then_some(()),
        );
    }

    /// Whether the watcher still holds the Claude dir `claude`'s directory watch and the row `row`'s file watch.
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    pub(crate) fn holds(&self, claude: &Path, row: &Path) -> (bool, bool) {
        let Some(live) = self.live.as_ref() else {
            return (false, false);
        };
        (
            self.places
                .dirs
                .get(claude)
                .is_some_and(|placed| !live.watcher.is_gone(placed.id)),
            self.places
                .files
                .get(row)
                .is_some_and(|filed| !live.watcher.is_gone(filed.id)),
        )
    }
}

impl Places {
    /// Remove the watches of the Claude dirs and rows nothing in `awaited`
    /// names.
    fn keep_only(&mut self, live: &Live, awaited: &[Awaited]) {
        self.dirs.retain(|claude, placed| {
            let keep = awaited.iter().any(|a| a.dir == *claude);
            if !keep {
                live.remove(placed.id);
            }
            keep
        });
        self.files.retain(|row, filed| {
            let keep = awaited.iter().any(|a| row_of(&a.dir, a.pid) == *row);
            if !keep {
                live.remove(filed.id);
            }
            keep
        });
    }

    /// Place every watch `awaited` needs; the Claude dirs where one was newly
    /// placed.
    fn place(&mut self, live: &Live, awaited: &[Awaited]) -> Vec<PathBuf> {
        let mut fresh = BTreeSet::new();
        let dirs: BTreeSet<&Path> = awaited.iter().map(|a| a.dir.as_path()).collect();
        for claude in dirs {
            if self.place_dir(live, claude) {
                fresh.insert(claude.to_path_buf());
            }
        }
        for a in awaited {
            let row = row_of(&a.dir, a.pid);
            let in_place = self
                .dirs
                .get(&a.dir)
                .is_some_and(|placed| placed.level == a.dir.join(SESSIONS));
            if !in_place {
                // No registry directory, so no row: its watch, if any, is on
                // a file that is gone.
                if let Some(filed) = self.files.remove(&row) {
                    live.remove(filed.id);
                }
                continue;
            }
            if self.place_file(live, &a.dir, &row) {
                fresh.insert(a.dir.clone());
            }
        }
        fresh.into_iter().collect()
    }

    /// Put the Claude dir `claude`'s directory watch as far in toward its
    /// `sessions` as is there; whether a watch was newly placed.
    fn place_dir(&mut self, live: &Live, claude: &Path) -> bool {
        let target = claude.join(SESSIONS);
        if let Some(placed) = self.dirs.get(claude)
            && (dir_ident(&placed.level) != Some(placed.ident) || live.watcher.is_gone(placed.id))
        {
            live.remove(placed.id);
            self.dirs.remove(claude);
        }
        let mut fresh = false;
        if !self.dirs.contains_key(claude) {
            // The nearest level that is there, from the target outward.
            if let Some(placed) = target
                .ancestors()
                .find_map(|level| register_dir(live, claude, level).ok())
            {
                self.dirs.insert(claude.to_path_buf(), placed);
                fresh = true;
            }
        }
        while let Some(placed) = self.dirs.get(claude)
            && placed.level != target
            && let Some(next) = target
                .ancestors()
                .find(|level| level.parent() == Some(placed.level.as_path()))
            && let Ok(inner) = register_dir(live, claude, next)
        {
            if let Some(outer) = self.dirs.insert(claude.to_path_buf(), inner) {
                live.remove(outer.id);
            }
            fresh = true;
        }
        fresh
    }

    /// Watch the row `row` of the Claude dir `claude` while it is there;
    /// whether a watch was newly placed, or the file was replaced while it
    /// was (read it now, watch it on the next reconcile).
    fn place_file(&mut self, live: &Live, claude: &Path, row: &Path) -> bool {
        if let Some(filed) = self.files.get(row) {
            if file_ident(row) == Some(filed.ident) && !live.watcher.is_gone(filed.id) {
                return false;
            }
            live.remove(filed.id);
            self.files.remove(row);
        }
        let Some(before) = file_ident(row) else {
            return false;
        };
        let Ok(id) = live.add(claude, row, Kind::File, true) else {
            return false;
        };
        if file_ident(row) != Some(before) {
            live.remove(id);
            return true;
        }
        self.files
            .insert(row.to_path_buf(), Filed { id, ident: before });
        true
    }
}

/// Watch the directory `level` for the Claude dir `claude`: the directory it
/// is when it is a link.
fn register_dir(live: &Live, claude: &Path, level: &Path) -> io::Result<Placed> {
    let before = dir_ident(level).ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
    let rows = level == claude.join(SESSIONS);
    let id = match live.add(claude, level, Kind::Dir, rows) {
        Ok(id) => id,
        Err(e) => {
            let real = std::fs::canonicalize(level)?;
            if real == level {
                return Err(e);
            }
            live.add(claude, &real, Kind::Dir, rows)?
        }
    };
    if dir_ident(level) != Some(before) {
        live.remove(id);
        return Err(io::Error::other("replaced while it was registered"));
    }
    Ok(Placed {
        level: level.to_path_buf(),
        id,
        ident: before,
    })
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    //! The watch's own laws, on real directories and a real kernel queue.
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{Receiver, channel};
    use std::sync::{Arc, Mutex};

    use aterm_spec::wait::MUST_HAPPEN;

    use super::{Awaited, RegistryWatch, Sink, Told, row_of};

    /// A watch whose sink is a channel the test reads.
    fn watch() -> (RegistryWatch, Receiver<Told>) {
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let sink: Sink = Arc::new(move |told| {
            tx.lock()
                .unwrap_or_else(|p| p.into_inner())
                .send(told)
                .is_ok()
        });
        (RegistryWatch::new(atpkg::dirwatch::Watcher::new, sink), rx)
    }

    /// Wait for `want`, past what an earlier step's events still tell (a
    /// hang detector: what must be told and never is fails the test).
    fn until(rx: &Receiver<Told>, want: &Told) {
        loop {
            let told = rx
                .recv_timeout(MUST_HAPPEN)
                .unwrap_or_else(|_| panic!("never told {want:?}"));
            if told == *want {
                return;
            }
        }
    }

    /// A fresh scratch root for test `name`.
    fn root(name: &str) -> (aterm_tempfile::TempDir, PathBuf) {
        let owned = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-gui-registry-{name}-"))
            .tempdir()
            .expect("exclusive registry scratch");
        let path = std::fs::canonicalize(owned.path()).unwrap();
        (owned, path)
    }

    fn awaited(dir: &Path, pid: u32) -> Vec<Awaited> {
        vec![Awaited {
            dir: dir.to_path_buf(),
            pid,
        }]
    }

    /// Nothing awaited, nothing up; something awaited, the watch is placed
    /// (and said fresh once); nothing awaited again, it all goes.
    #[test]
    fn the_watch_lives_exactly_while_something_is_awaited() {
        let (_owned_root, root) = root("lives");
        let claude = root.join("claude");
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        let (mut watch, _rx) = watch();
        assert!(watch.reconcile(&[]).is_empty());
        assert!(!watch.is_live());
        assert_eq!(
            watch.reconcile(&awaited(&claude, 7)),
            std::slice::from_ref(&claude)
        );
        assert!(watch.is_live());
        assert_eq!(watch.level_of(&claude), Some(claude.join("sessions")));
        assert!(
            watch.reconcile(&awaited(&claude, 7)).is_empty(),
            "a kept watch is not fresh"
        );
        assert!(watch.reconcile(&[]).is_empty());
        assert!(!watch.is_live(), "nothing awaited: no watch, no thread");
        assert_eq!(watch.level_of(&claude), None);
        let (tx, _) = channel::<Told>();
        let tx = Mutex::new(tx);
        let mut refused = RegistryWatch::new(
            || Err(std::io::Error::from(std::io::ErrorKind::Unsupported)),
            Arc::new(move |told| tx.lock().unwrap().send(told).is_ok()),
        );
        assert!(refused.reconcile(&awaited(&claude, 7)).is_empty());
        assert!(!refused.is_live());
    }

    /// A row created in `sessions/` is told as its Claude dir; a write in
    /// place to a row that is there is told through its file watch.
    #[test]
    fn a_created_row_and_a_row_written_in_place_are_told() {
        let (_owned_root, root) = root("told");
        let claude = root.join("claude");
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        let (mut watch, rx) = watch();
        let _ = watch.reconcile(&awaited(&claude, 7));
        let row = row_of(&claude, 7);
        std::fs::write(&row, b"").unwrap();
        assert_eq!(
            rx.recv_timeout(MUST_HAPPEN).unwrap(),
            Told::Rows(claude.clone())
        );
        assert_eq!(
            watch.reconcile(&awaited(&claude, 7)),
            std::slice::from_ref(&claude)
        );
        assert!(watch.file_watched(&row));
        while rx.try_recv().is_ok() {}
        std::fs::write(&row, b"{}").unwrap();
        assert_eq!(
            rx.recv_timeout(MUST_HAPPEN).unwrap(),
            Told::Rows(claude.clone())
        );
    }

    /// With `sessions/` and its parent missing, the watch is on the nearest
    /// ancestor there and moves inward as each level appears: registered
    /// first, then the next level looked for, so a parent and child made in
    /// one burst both count.
    #[test]
    fn the_watch_moves_inward_as_the_levels_appear() {
        let (_owned_root, root) = root("inward");
        let claude = root.join("claude");
        let (mut watch, rx) = watch();
        assert_eq!(
            watch.reconcile(&awaited(&claude, 7)),
            std::slice::from_ref(&claude)
        );
        assert_eq!(watch.level_of(&claude), Some(root.clone()));
        std::fs::write(root.join("settings.json"), b"{}").unwrap();
        assert_eq!(rx.recv_timeout(MUST_HAPPEN).unwrap(), Told::Level);
        assert!(
            watch.reconcile(&awaited(&claude, 7)).is_empty(),
            "nothing new to watch"
        );
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        until(&rx, &Told::Level);
        assert_eq!(
            watch.reconcile(&awaited(&claude, 7)),
            std::slice::from_ref(&claude)
        );
        assert_eq!(
            watch.level_of(&claude),
            Some(claude.join("sessions")),
            "straight to the target"
        );
        std::fs::remove_dir(claude.join("sessions")).unwrap();
        until(&rx, &Told::Rows(claude.clone()));
        assert_eq!(
            watch.reconcile(&awaited(&claude, 7)),
            std::slice::from_ref(&claude)
        );
        assert_eq!(watch.level_of(&claude), Some(claude.clone()));
        std::fs::create_dir(claude.join("sessions")).unwrap();
        until(&rx, &Told::Level);
        assert_eq!(
            watch.reconcile(&awaited(&claude, 7)),
            std::slice::from_ref(&claude)
        );
        assert_eq!(watch.level_of(&claude), Some(claude.join("sessions")));
        while rx.try_recv().is_ok() {}
        std::fs::write(row_of(&claude, 7), b"").unwrap();
        until(&rx, &Told::Rows(claude.clone()));
    }

    /// A watch the kernel let go of is placed again although its object is
    /// the one it was put on: on Linux a `sessions/` removed and made again
    /// between two reconciles can take the old inode number, and a watch
    /// kept by `(dev, ino)` alone would never fire again.
    #[test]
    fn a_watch_the_kernel_let_go_is_placed_again() {
        let (_owned_root, root) = root("gone");
        let claude = root.join("claude");
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        let row = row_of(&claude, 7);
        std::fs::write(&row, b"").unwrap();
        let (mut watch, rx) = watch();
        assert_eq!(
            watch.reconcile(&awaited(&claude, 7)),
            std::slice::from_ref(&claude)
        );
        assert_eq!(watch.holds(&claude, &row), (true, true));
        watch.lose_watches(&claude, &row);
        assert_eq!(watch.holds(&claude, &row), (false, false), "let go");
        assert_eq!(
            watch.reconcile(&awaited(&claude, 7)),
            std::slice::from_ref(&claude),
            "placed again, so the row is read again"
        );
        assert_eq!(watch.level_of(&claude), Some(claude.join("sessions")));
        assert_eq!(watch.holds(&claude, &row), (true, true), "both held again");
        std::fs::write(row_of(&claude, 8), b"").unwrap();
        until(&rx, &Told::Rows(claude.clone()));
        std::fs::write(&row, b"{}").unwrap();
        until(&rx, &Told::Rows(claude.clone()));
    }

    /// The forwarder tells only what the kernel fired, never on a clock or a
    /// bare wake: woken with nothing changed, it finishes that wait and tells
    /// nothing, then tells the next real change.
    #[test]
    fn the_forwarder_tells_only_what_the_kernel_fired() {
        let (_owned_root, root) = root("kernel-only");
        let claude = root.join("claude");
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        let row = row_of(&claude, 7);
        std::fs::write(&row, b"").unwrap();
        let (mut watch, rx) = watch();
        assert_eq!(
            watch.reconcile(&awaited(&claude, 7)),
            std::slice::from_ref(&claude)
        );
        assert_eq!(watch.holds(&claude, &row), (true, true));
        for _ in 0..3 {
            watch.wake_and_settle();
            assert_eq!(rx.try_recv().ok(), None, "a wake with nothing fired");
        }
        std::fs::write(&row, b"{}").unwrap();
        assert_eq!(
            rx.recv_timeout(MUST_HAPPEN).unwrap(),
            Told::Rows(claude.clone())
        );
    }

    /// A row no longer awaited loses its file watch, and a Claude dir no
    /// process awaits loses its directory watch, while another stays.
    #[test]
    fn what_is_no_longer_awaited_is_let_go() {
        let (_owned_root, root) = root("let-go");
        let (a, b) = (root.join("a"), root.join("b"));
        for dir in [&a, &b] {
            std::fs::create_dir_all(dir.join("sessions")).unwrap();
        }
        std::fs::write(row_of(&a, 7), b"").unwrap();
        let (mut watch, _rx) = watch();
        let both = [
            Awaited {
                dir: a.clone(),
                pid: 7,
            },
            Awaited {
                dir: b.clone(),
                pid: 8,
            },
        ];
        assert_eq!(watch.reconcile(&both), [a.clone(), b.clone()]);
        assert!(watch.file_watched(&row_of(&a, 7)));
        watch.let_go(&both[1..]);
        assert!(!watch.file_watched(&row_of(&a, 7)));
        assert_eq!(watch.level_of(&a), None);
        assert_eq!(watch.level_of(&b), Some(b.join("sessions")));
        assert!(watch.is_live());
        watch.let_go(&[]);
        assert!(!watch.is_live());
    }
}
