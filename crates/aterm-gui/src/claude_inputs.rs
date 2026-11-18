// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FOOTER'S INPUT WATCHES: one watch for every file a Claude Code footer read looked at
//! before it read it ([`aterm_agent::harness::looks::Look`]: a session's registry row,
//! transcript, subagents, git `HEAD` and usage-tap reading, and the machine's login profiles,
//! roster, login bindings, accounts store and catalog folder), and one on the exec and exit of
//! each process the resolver holds anything for (the footer cause design's step C5).
//!
//! Every watch is by path, with identity ([`InputWatch::reconcile`]): a file is watched where it
//! is (its contents alone where the look says so, [`Heed::Contents`]), a directory for its
//! entries, a symlink at what it names, and a path that names nothing at its nearest existing
//! ancestor, moved inward as the levels appear. A watch whose object was replaced (another
//! device and inode at its path) or that the kernel let go is placed again on what the path names
//! now. The resolver places before it reads what a placement owes: each path placed afresh is
//! handed back ([`Placed::fresh`]), and a look of it that moved since its read is read again.
//!
//! At most [`BUDGET`] paths are watched at once (each a kqueue vnode descriptor on macOS). A
//! session a look of which is left with no watch, past the budget, refused by the kernel, or with
//! no watcher at all, is on the clocks arm ([`Placed::clocks`]).
//!
//! While anything is watched, the forwarder also polls the wall clock's watch
//! ([`aterm_uds::wall_watch::WallWatch::start_zoned`], one for the process): a step of the wall
//! clock or a wake it hears is told as [`Told::Host`] of [`Bell::Wall`], a new local time zone as
//! one of [`Bell::Zone`] (the footer cause design's step C8). Where that watch hears each of the
//! Mac's sleeps ([`InputWatch::hears_wall`]), a deadline a read posted is computed again at the
//! wall's move and waited toward whole. Where it hears none (no watch was had or polled, or a
//! macOS watch whose power half was refused, which hears `clock_set` alone and so no sleep the
//! clock was not set across), the resolver holds each wait toward a deadline to a look
//! (`claude_footer::WALL_LOOK`) and measures the deadline again from the wall at each turn, so a
//! deadline the Mac slept past is read within a look of the wake.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use aterm_agent::harness::looks::Heed;
use aterm_uds::wall_watch::WallWatch;
use atpkg::dirwatch::{WatchId, Watcher};

use crate::host_facts::Bell;

/// The forwarding thread's name.
pub(crate) const THREAD: &str = "aterm-claude-inputs";

/// The most paths watched at once ([`InputWatch::reconcile`]).
pub(crate) const BUDGET: usize = 256;

/// Where the forwarder tells what fired.
pub(crate) type Sink = Arc<dyn Fn(Told) -> bool + Send + Sync>;

/// What one wait of the forwarder tells, per fired watch.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Told {
    /// A watch that serves the looked path fired: the path may stand otherwise than a read
    /// looked at it.
    Input(PathBuf),
    /// The watch on process `pid`, the one the kernel started at `started` (its start time in
    /// seconds), fired: the process exec'd, or (`exited`) it exited and the watch is gone.
    Process {
        pid: u32,
        started: u64,
        exited: bool,
    },
    /// The wall clock's watch said the host moved: the wall clock stepped or the machine woke
    /// ([`Bell::Wall`]), or the local time zone was set ([`Bell::Zone`]).
    Host(Bell),
}

/// The wall clock watch a live watch's forwarder polls beside its watcher ([`Told::Host`]): the
/// process's, started at the first ask and kept for the process's life (its sleep and wake
/// listener is one thread, however often the input watch starts again); `None` where the system
/// announces no move. A move announced while no watch is live stays readable, and the next live
/// watch tells it at once.
#[cfg(all(unix, not(test)))]
fn open_wall() -> Option<Arc<WallWatch>> {
    static WALL: std::sync::OnceLock<Option<Arc<WallWatch>>> = std::sync::OnceLock::new();
    WALL.get_or_init(|| WallWatch::start_zoned().map(Arc::new))
        .clone()
}

/// In a test, a wall clock watch of the live watch's own, told by hand ([`InputWatch::wall`]),
/// which hears each sleep it is told.
#[cfg(all(unix, test))]
fn open_wall() -> Option<Arc<WallWatch>> {
    WallWatch::by_hand().ok().map(Arc::new)
}

/// No arm polls a descriptor here ([`Watcher::new`] answers `Unsupported`): no wall clock watch.
#[cfg(not(unix))]
fn open_wall() -> Option<Arc<WallWatch>> {
    None
}

/// Where a process watch stands once [`InputWatch::watch_process`] asked for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Placement {
    /// Placed, by this ask (`fresh`) or before it: its exit is told, and its exec too where
    /// `exec` ([`Watcher::HEARS_EXEC`]).
    Watched { exec: bool, fresh: bool },
    /// The process is gone: its pid names no process, or one the kernel started at another
    /// time.
    Gone,
    /// No watch: no watcher, or the kernel refused this one.
    Unwatched,
}

/// What one [`InputWatch::reconcile`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Placed {
    /// The looked paths whose watch this reconcile placed, or placed again: a look of one may
    /// have moved between its read and the placement.
    pub(crate) fresh: Vec<PathBuf>,
    /// The sessions on the clocks arm: a look of theirs has no watch.
    pub(crate) clocks: BTreeSet<u64>,
}

/// What a watch was put on: the object's device and inode, so an object replaced under its
/// watch is told apart from the one watched.
type Ident = (u64, u64);

#[cfg(unix)]
fn ident(meta: &std::fs::Metadata) -> Option<Ident> {
    use std::os::unix::fs::MetadataExt as _;
    Some((meta.dev(), meta.ino()))
}

/// No arm watches anything here ([`Watcher::new`] answers `Unsupported`), so nothing is ever
/// placed.
#[cfg(not(unix))]
fn ident(_meta: &std::fs::Metadata) -> Option<Ident> {
    None
}

/// What kind of watch stands on a level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Kind {
    /// A directory, for its entries ([`Watcher::add_dir`]).
    Dir,
    /// A file, for every change ([`Watcher::add_file`]).
    File,
    /// A file, for its contents ([`Watcher::add_file_contents`]).
    Contents,
}

/// Where a looked path's watch stands: `kind` on `level`, the object `ident` names.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Spot {
    level: PathBuf,
    kind: Kind,
    ident: Ident,
}

/// Where the watch of `path`, looked at to be heard as `heed` says, belongs now: on the file or
/// directory it names, on what a symlink names, and, where it names nothing (or a link names
/// nothing), on its nearest existing ancestor directory. `None` where nothing along it is there,
/// or it names something no watch hears (a socket, a device).
fn spot_of(path: &Path, heed: Heed) -> Option<Spot> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => match std::fs::canonicalize(path) {
            Ok(real) if real != path => spot_of(&real, heed),
            _ => ancestor_spot(path),
        },
        Ok(meta) if meta.is_dir() => Some(Spot {
            level: path.to_path_buf(),
            kind: Kind::Dir,
            ident: ident(&meta)?,
        }),
        Ok(meta) if meta.is_file() => Some(Spot {
            level: path.to_path_buf(),
            kind: match heed {
                Heed::All => Kind::File,
                Heed::Contents => Kind::Contents,
            },
            ident: ident(&meta)?,
        }),
        Ok(_) => None,
        Err(_) => ancestor_spot(path),
    }
}

/// The nearest existing ancestor directory of `path`, as a [`Spot`].
fn ancestor_spot(path: &Path) -> Option<Spot> {
    path.ancestors().skip(1).find_map(|level| {
        let meta = std::fs::metadata(level)
            .ok()
            .filter(std::fs::Metadata::is_dir)?;
        Some(Spot {
            level: level.to_path_buf(),
            kind: Kind::Dir,
            ident: ident(&meta)?,
        })
    })
}

/// The identity of the object at `level` now, as a watch of `kind` looks at it.
fn ident_now(level: &Path, kind: Kind) -> Option<Ident> {
    let meta = match kind {
        Kind::Dir => std::fs::metadata(level)
            .ok()
            .filter(std::fs::Metadata::is_dir),
        Kind::File | Kind::Contents => std::fs::symlink_metadata(level)
            .ok()
            .filter(std::fs::Metadata::is_file),
    }?;
    ident(&meta)
}

/// Watch over the files the resolver's reads looked at, and the processes it holds anything for.
pub(crate) struct InputWatch {
    /// Makes the watcher ([`Watcher::new`]; a test's refusal disables it).
    open: fn() -> io::Result<Watcher>,
    /// Opens the wall clock's watch a live watch polls ([`open_wall`]; a test sets its own,
    /// [`Self::hear_wall_by`]).
    wall: fn() -> Option<Arc<WallWatch>>,
    sink: Sink,
    /// The watcher and its forwarder, while anything is watched.
    live: Option<Live>,
    places: Places,
    /// The watcher was refused once (logged once, asked again each turn).
    refused: bool,
    /// The most levels watched at once ([`BUDGET`]; a test sets it low).
    budget: usize,
}

/// Where each watch is.
#[derive(Default)]
struct Places {
    /// Per looked path, where its watch stands.
    looks: HashMap<PathBuf, Spot>,
    /// Per watched level and kind, its watch and the looked paths it serves.
    levels: HashMap<(PathBuf, Kind), Level>,
    /// Per process (pid, start time in seconds), its process watch.
    processes: HashMap<(u32, u64), WatchId>,
}

/// One level's watch, the object it was placed on, and the looked paths it serves.
struct Level {
    id: WatchId,
    /// What the level's path named when the watch was placed: the watch follows that object,
    /// so once the path names another (a rename of the level and a new one made in its place,
    /// which the kernel tells the watch but does not end it for) the watch is stale.
    ident: Ident,
    serves: BTreeSet<PathBuf>,
}

impl Places {
    fn is_empty(&self) -> bool {
        self.looks.is_empty() && self.processes.is_empty()
    }
}

/// Where a watch's events go.
#[derive(Clone)]
enum Route {
    /// The looked paths the watch serves.
    Looks(BTreeSet<PathBuf>),
    /// The process `pid` the kernel started at `started`.
    Process { pid: u32, started: u64 },
    /// One of the wall clock watch's descriptors.
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "no arm polls a descriptor off unix")
    )]
    Wall,
}

/// A test drain is acknowledged only after the forwarder empties the kernel queue and tells
/// the sink every notification it took, including a batch already in flight at the request.
#[cfg(test)]
#[derive(Default)]
struct Settle {
    requested: AtomicU64,
    finished: AtomicU64,
}

/// The watcher, the route of each watch, and the forwarder reading them.
struct Live {
    watcher: Arc<Watcher>,
    /// Map from a fired watch to what it tells, held across registration and routing.
    routes: Arc<Mutex<HashMap<WatchId, Route>>>,
    /// The wall clock's watch whose descriptors the watcher polls ([`Route::Wall`]), which a test
    /// tells ([`InputWatch::wall`]).
    #[cfg(all(test, unix))]
    wall: Option<Arc<WallWatch>>,
    /// The watcher polls a wall clock watch that hears each of the Mac's sleeps
    /// ([`InputWatch::hears_wall`]).
    hears_wall: bool,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Requests for an acknowledged drain of queued notifications.
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    settle: Arc<Settle>,
}

/// The forwarder's share of [`Live`]: the thread's loop ([`Self::run`]).
struct Forwarder {
    watcher: Arc<Watcher>,
    routes: Arc<Mutex<HashMap<WatchId, Route>>>,
    /// What a [`Route::Wall`] that fired is taken from.
    wall: Option<Arc<WallWatch>>,
    stop: Arc<AtomicBool>,
    sink: Sink,
    /// The test thread's drain requests and the last one acknowledged.
    #[cfg(test)]
    settle: Arc<Settle>,
}

impl Live {
    fn start(
        open: fn() -> io::Result<Watcher>,
        open_wall: fn() -> Option<Arc<WallWatch>>,
        sink: &Sink,
    ) -> io::Result<Live> {
        let watcher = Arc::new(open()?);
        let routes: Arc<Mutex<HashMap<WatchId, Route>>> = Arc::default();
        let wall = Self::poll_wall(&watcher, &routes, open_wall);
        let hears_wall = wall.as_deref().is_some_and(WallWatch::hears_sleep);
        #[cfg(all(test, unix))]
        let held = wall.clone();
        let stop = Arc::new(AtomicBool::new(false));
        #[cfg(test)]
        let settle = Arc::new(Settle::default());
        let forwarder = Forwarder {
            watcher: Arc::clone(&watcher),
            routes: Arc::clone(&routes),
            wall,
            stop: Arc::clone(&stop),
            sink: Arc::clone(sink),
            #[cfg(test)]
            settle: Arc::clone(&settle),
        };
        let thread =
            crate::qos::spawn_lane_thread(THREAD, crate::qos::Role::Responsive, move || {
                forwarder.run();
            })?;
        Ok(Live {
            watcher,
            routes,
            #[cfg(all(test, unix))]
            wall: held,
            hears_wall,
            stop,
            thread: Some(thread),
            #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
            settle,
        })
    }

    /// Open the wall clock's watch with `open_wall` ([`InputWatch`]'s opener: the module's
    /// [`open_wall`] unless a test set its own) and have `watcher` poll each of its descriptors
    /// ([`Route::Wall`]): the watch where every descriptor was taken, else `None` (logged), and
    /// then no sleep is heard ([`InputWatch::hears_wall`]).
    #[cfg(unix)]
    fn poll_wall(
        watcher: &Watcher,
        routes: &Mutex<HashMap<WatchId, Route>>,
        open_wall: fn() -> Option<Arc<WallWatch>>,
    ) -> Option<Arc<WallWatch>> {
        let wall = open_wall()?;
        let mut routes = routes.lock().unwrap_or_else(|p| p.into_inner());
        let mut ids = Vec::new();
        for fd in wall.fds() {
            match watcher.add_readable(fd) {
                Ok(id) => {
                    routes.insert(id, Route::Wall);
                    ids.push(id);
                }
                Err(e) => {
                    aterm_log::info!("claude inputs: the wall clock's watch is not polled ({e})");
                    for id in ids {
                        routes.remove(&id);
                        watcher.remove(id);
                    }
                    return None;
                }
            }
        }
        Some(wall)
    }

    /// No arm polls a descriptor here ([`Watcher::new`] answers `Unsupported`).
    #[cfg(not(unix))]
    fn poll_wall(
        _watcher: &Watcher,
        _routes: &Mutex<HashMap<WatchId, Route>>,
        _open_wall: fn() -> Option<Arc<WallWatch>>,
    ) -> Option<Arc<WallWatch>> {
        None
    }

    /// Register a watch of `kind` on `level`, serving `serves`: a directory that refuses its
    /// watch at its path is watched at its canonical path.
    fn add(&self, level: &Path, kind: Kind, serves: &BTreeSet<PathBuf>) -> io::Result<WatchId> {
        let mut routes = self.routes.lock().unwrap_or_else(|p| p.into_inner());
        let id = match kind {
            Kind::Dir => self.watcher.add_dir(level).or_else(|e| {
                let real = std::fs::canonicalize(level)?;
                if real == level {
                    return Err(e);
                }
                self.watcher.add_dir(&real)
            }),
            Kind::File => self.watcher.add_file(level),
            Kind::Contents => self.watcher.add_file_contents(level),
        }?;
        routes.insert(id, Route::Looks(serves.clone()));
        Ok(id)
    }

    /// Route the watch `id` to the looked paths `serves`.
    fn route(&self, id: WatchId, serves: &BTreeSet<PathBuf>) {
        self.routes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id, Route::Looks(serves.clone()));
    }

    /// Register a watch of process `pid`, started at `started`.
    fn add_process(&self, pid: u32, started: u64) -> io::Result<WatchId> {
        let mut routes = self.routes.lock().unwrap_or_else(|p| p.into_inner());
        let id = self.watcher.add_process(pid)?;
        routes.insert(id, Route::Process { pid, started });
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
    /// Stop the forwarder and wait for it: the watcher closes with the last handle, which the
    /// forwarder holds until it returns.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Err(e) = self.watcher.wake() {
            aterm_log::warn!("claude inputs: could not wake the watch thread: {e}");
        }
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            aterm_log::warn!("claude inputs: the watch thread panicked");
        }
    }
}

impl Forwarder {
    /// The forwarder's loop: wait with no timeout, then tell the sink what the fired watches
    /// route to, each once per wait.
    fn run(&self) {
        loop {
            let fired = match self.watcher.wait(None) {
                Ok(fired) => fired,
                Err(e) => {
                    aterm_log::warn!("claude inputs: the watch failed: {e}");
                    return;
                }
            };
            if self.stop.load(Ordering::Acquire) {
                return;
            }
            if !self.tell(&fired) {
                return;
            }
            #[cfg(test)]
            {
                let requested = self.settle.requested.load(Ordering::Acquire);
                if requested > self.settle.finished.load(Ordering::Acquire) {
                    loop {
                        let fired = match self.watcher.wait(Some(std::time::Duration::ZERO)) {
                            Ok(fired) => fired,
                            Err(_) => return,
                        };
                        if fired.is_empty() {
                            break;
                        }
                        if !self.tell(&fired) {
                            return;
                        }
                    }
                    self.settle.finished.store(requested, Ordering::Release);
                }
            }
        }
    }

    /// Route and deliver one batch before any drain can acknowledge it.
    fn tell(&self, fired: &[WatchId]) -> bool {
        let told: BTreeSet<Told> = {
            let routes = self.routes.lock().unwrap_or_else(|p| p.into_inner());
            fired
                .iter()
                .filter_map(|id| Some((routes.get(id)?, *id)))
                .flat_map(|(route, id)| match route {
                    Route::Looks(paths) => paths.iter().cloned().map(Told::Input).collect(),
                    Route::Process { pid, started } => vec![Told::Process {
                        pid: *pid,
                        started: *started,
                        exited: self.watcher.is_gone(id),
                    }],
                    Route::Wall => self.wall_moved(),
                })
                .collect()
        };
        for told in told {
            if !(self.sink)(told) {
                return false;
            }
        }
        true
    }
}

impl Forwarder {
    /// What the wall clock's watch says moved, every descriptor of it emptied
    /// ([`WallWatch::take`]); a second fired descriptor in the same wait finds it taken.
    fn wall_moved(&self) -> Vec<Told> {
        let Some(moved) = self.wall.as_ref().map(|wall| wall.take()) else {
            return Vec::new();
        };
        let mut told = Vec::new();
        if moved.stepped || moved.woke {
            told.push(Told::Host(Bell::Wall));
        }
        if moved.zoned {
            told.push(Told::Host(Bell::Zone));
        }
        told
    }
}

impl InputWatch {
    /// A watch that opens its watcher with `open` and tells `sink`.
    pub(crate) fn new(open: fn() -> io::Result<Watcher>, sink: Sink) -> InputWatch {
        InputWatch {
            open,
            wall: open_wall,
            sink,
            live: None,
            places: Places::default(),
            refused: false,
            budget: BUDGET,
        }
    }

    /// Watch exactly the paths `wants` names, each session's looked paths with how each is to
    /// be heard (a path looked at both ways is heard for every change), and let go of every
    /// other path's watch. A watch stands while its spot does ([`spot_of`]: the same level, kind
    /// and object, the level's path still names the object its watch was placed on, and the
    /// watcher has not let it go); else it is placed again where the path belongs now. Sessions
    /// are placed in their order, so a budget the earlier ones fill puts the later ones on the
    /// clocks arm. The process watches stand as they are.
    pub(crate) fn reconcile(&mut self, wants: &BTreeMap<u64, Vec<(PathBuf, Heed)>>) -> Placed {
        let mut heeds: HashMap<&Path, Heed> = HashMap::new();
        for (path, heed) in wants.values().flatten() {
            heeds
                .entry(path.as_path())
                .and_modify(|held| {
                    if *heed == Heed::All {
                        *held = Heed::All;
                    }
                })
                .or_insert(*heed);
        }
        if heeds.is_empty() {
            self.let_go_looks(&HashSet::new());
            return Placed::default();
        }
        if !self.start() {
            return Placed {
                fresh: Vec::new(),
                clocks: wants
                    .iter()
                    .filter(|(_, looks)| !looks.is_empty())
                    .map(|(session, _)| *session)
                    .collect(),
            };
        }
        let wanted: HashSet<&Path> = heeds.keys().copied().collect();
        let Some(live) = self.live.as_ref() else {
            return Placed::default();
        };
        self.places.keep_only(live, &wanted);
        self.places.drop_stale(live);
        let mut placed = Placed::default();
        for (session, looks) in wants {
            for (path, _) in looks {
                let Some(heed) = heeds.remove(path.as_path()) else {
                    // Placed for an earlier session, or found unplaceable there.
                    if !self.places.looks.contains_key(path) {
                        placed.clocks.insert(*session);
                    }
                    continue;
                };
                match self.places.place(live, path, heed, self.budget) {
                    Some(true) => placed.fresh.push(path.clone()),
                    Some(false) => {}
                    None => {
                        placed.clocks.insert(*session);
                    }
                }
            }
        }
        if self.places.is_empty() {
            self.release();
        }
        placed
    }

    /// The watcher and its forwarder up, started where none runs and started again where the
    /// forwarder ended (every watch going with the old one); `false` where the watcher is refused.
    fn start(&mut self) -> bool {
        if self.live.as_ref().is_some_and(Live::ended) {
            aterm_log::warn!("claude inputs: the watch thread ended; starting it again");
            self.release();
        }
        if self.live.is_none() {
            match Live::start(self.open, self.wall, &self.sink) {
                Ok(live) => {
                    self.live = Some(live);
                    self.refused = false;
                }
                Err(e) => {
                    if !self.refused {
                        aterm_log::info!("claude inputs: no watch ({e}); reads keep their clocks");
                        self.refused = true;
                    }
                    return false;
                }
            }
        }
        true
    }

    /// Let go of the watch of every looked path `keep` does not name, placing none.
    pub(crate) fn let_go_looks(&mut self, keep: &HashSet<&Path>) {
        if let Some(live) = self.live.as_ref() {
            self.places.keep_only(live, keep);
        }
        self.release_if_idle();
    }

    /// Watch process `pid`, which the kernel started at `started`, for its exit and, where this
    /// platform hears one ([`Watcher::HEARS_EXEC`]), its exec: told as [`Told::Process`]. The
    /// watch is placed before `birth` is asked again, and stands only where `birth` still says
    /// `started`, so a pid the kernel gave another process between the caller's read of its start
    /// and the placement is [`Placement::Gone`].
    pub(crate) fn watch_process(
        &mut self,
        pid: u32,
        started: u64,
        birth: impl FnOnce() -> Option<u64>,
    ) -> Placement {
        let watched = |fresh| Placement::Watched {
            exec: Watcher::HEARS_EXEC,
            fresh,
        };
        if let (Some(live), Some(id)) = (
            self.live.as_ref(),
            self.places.processes.get(&(pid, started)),
        ) {
            return if live.watcher.is_gone(*id) {
                Placement::Gone
            } else {
                watched(false)
            };
        }
        if !self.start() {
            return Placement::Unwatched;
        }
        let Some(live) = self.live.as_ref() else {
            return Placement::Unwatched;
        };
        let placement = match live.add_process(pid, started) {
            Ok(id) if birth() == Some(started) => {
                self.places.processes.insert((pid, started), id);
                watched(true)
            }
            Ok(id) => {
                live.remove(id);
                Placement::Gone
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Placement::Gone,
            Err(_) => Placement::Unwatched,
        };
        self.release_if_idle();
        placement
    }

    /// Whether the watch of process `pid` started at `started` stands: placed, and not told gone.
    pub(crate) fn holds_process(&self, pid: u32, started: u64) -> bool {
        self.live
            .as_ref()
            .zip(self.places.processes.get(&(pid, started)))
            .is_some_and(|(live, id)| !live.watcher.is_gone(*id))
    }

    /// Whether the live watch polls a wall clock watch that hears each of the Mac's sleeps
    /// ([`WallWatch::hears_sleep`]), so a wake is told ([`Told::Host`] of [`Bell::Wall`]) and a
    /// wait toward a wall second may run whole. `false` while nothing is watched, where no wall
    /// clock watch was had or its descriptors could not all be polled, and where the one polled
    /// hears steps alone (macOS with its power half refused); a step it hears is told all the same.
    pub(crate) fn hears_wall(&self) -> bool {
        self.live.as_ref().is_some_and(|live| live.hears_wall)
    }

    /// Let go of the process watches for any process `held` does not name.
    pub(crate) fn keep_processes(&mut self, held: &HashSet<(u32, u64)>) {
        if let Some(live) = self.live.as_ref() {
            self.places.processes.retain(|process, id| {
                let keep = held.contains(process);
                if !keep {
                    live.remove(*id);
                }
                keep
            });
        }
        self.release_if_idle();
    }

    /// With no watch left, the watcher and the forwarder go.
    fn release_if_idle(&mut self) {
        if self.places.is_empty() {
            self.release();
        }
    }

    /// Every watch, the watcher and the forwarder go.
    fn release(&mut self) {
        self.places = Places::default();
        self.live = None;
    }

    /// How many watches are placed: level and process watches.
    #[cfg(all(test, unix))]
    pub(crate) fn placed(&self) -> usize {
        self.places.levels.len() + self.places.processes.len()
    }

    /// Watch at most `budget` levels at once.
    #[cfg(all(test, unix))]
    pub(crate) fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
    }

    /// Open the wall clock's watch with `wall` from the next start of the watcher on.
    #[cfg(all(test, unix))]
    pub(crate) fn hear_wall_by(&mut self, wall: fn() -> Option<Arc<WallWatch>>) {
        self.wall = wall;
    }

    /// Whether the watcher and its forwarder are up.
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    pub(crate) fn is_live(&self) -> bool {
        self.live.is_some()
    }

    /// The level the looked path `path`'s watch is on.
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    pub(crate) fn level_of(&self, path: &Path) -> Option<PathBuf> {
        self.places.looks.get(path).map(|spot| spot.level.clone())
    }

    /// Whether the watcher still holds the watch the looked path `path` is placed on.
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    pub(crate) fn holds(&self, path: &Path) -> bool {
        let Some(live) = self.live.as_ref() else {
            return false;
        };
        self.places
            .looks
            .get(path)
            .and_then(|spot| self.places.levels.get(&(spot.level.clone(), spot.kind)))
            .is_some_and(|level| !live.watcher.is_gone(level.id))
    }

    /// Remove the watch the looked path `path` is placed on from the underlying watcher.
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    pub(crate) fn lose_watch(&self, path: &Path) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        if let Some(level) = self
            .places
            .looks
            .get(path)
            .and_then(|spot| self.places.levels.get(&(spot.level.clone(), spot.kind)))
        {
            live.watcher.remove(level.id);
        }
    }

    /// The wall clock's watch the live watch polls, which a test tells by hand
    /// ([`WallWatch::tell`]); `None` while nothing is watched.
    #[cfg(all(test, unix))]
    pub(crate) fn wall(&self) -> Option<&WallWatch> {
        self.live.as_ref()?.wall.as_deref()
    }

    /// Request a drain and wait for its acknowledgement: a completion of an older, already
    /// in-flight wait cannot acknowledge this request. Every queued batch is told first.
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    pub(crate) fn wake_and_settle(&self) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let requested = live.settle.requested.fetch_add(1, Ordering::AcqRel) + 1;
        live.watcher.wake().expect("wake the forwarder");
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the forwarder acknowledges the requested queue drain",
            || (live.settle.finished.load(Ordering::Acquire) >= requested).then_some(()),
        );
    }
}

impl Places {
    /// Let go of every level whose watch is stale, with the looks it serves, so each of them is
    /// placed again: a level the watcher let go, or whose path names another object than the one
    /// its watch was placed on (the level renamed or replaced, and another made in its place).
    fn drop_stale(&mut self, live: &Live) {
        let stale: Vec<(PathBuf, Kind)> = self
            .levels
            .iter()
            .filter(|((level, kind), held)| {
                live.watcher.is_gone(held.id) || ident_now(level, *kind) != Some(held.ident)
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale {
            if let Some(level) = self.levels.remove(&key) {
                live.remove(level.id);
                for served in &level.serves {
                    self.looks.remove(served);
                }
            }
        }
    }

    /// Keep `path`'s watch where it stands, or place it where it belongs now ([`spot_of`]):
    /// `Some(true)` where this placed it, `Some(false)` where it stood, `None` where it has none
    /// (nothing along it is there, the kernel refused the watch, the object was replaced while
    /// it was registered, the level it shares went stale since this reconcile's
    /// [`Self::drop_stale`], or a new level would pass `budget`).
    fn place(&mut self, live: &Live, path: &Path, heed: Heed, budget: usize) -> Option<bool> {
        let spot = spot_of(path, heed);
        if let Some(held) = self.looks.get(path)
            && spot.as_ref() == Some(held)
            && self
                .levels
                .get(&(held.level.clone(), held.kind))
                .is_some_and(|level| !live.watcher.is_gone(level.id))
        {
            return Some(false);
        }
        self.unserve(live, path);
        let spot = spot?;
        let key = (spot.level.clone(), spot.kind);
        if let Some(level) = self.levels.get_mut(&key) {
            if live.watcher.is_gone(level.id) || level.ident != spot.ident {
                // The level went stale after this reconcile dropped the stale ones, and the
                // paths it serves were placed on it already: this path waits on its clocks
                // until the next reconcile drops the level and places them all again.
                return None;
            }
            level.serves.insert(path.to_path_buf());
            live.route(level.id, &level.serves);
            self.looks.insert(path.to_path_buf(), spot);
            return Some(true);
        }
        if self.levels.len() >= budget {
            return None;
        }
        let serves = BTreeSet::from([path.to_path_buf()]);
        let id = live.add(&spot.level, spot.kind, &serves).ok()?;
        if ident_now(&spot.level, spot.kind) != Some(spot.ident) {
            live.remove(id);
            return None;
        }
        self.levels.insert(
            key,
            Level {
                id,
                ident: spot.ident,
                serves,
            },
        );
        self.looks.insert(path.to_path_buf(), spot);
        Some(true)
    }

    /// Let go of the watch of every looked path `keep` does not name.
    fn keep_only(&mut self, live: &Live, keep: &HashSet<&Path>) {
        let gone: Vec<PathBuf> = self
            .looks
            .keys()
            .filter(|path| !keep.contains(path.as_path()))
            .cloned()
            .collect();
        for path in gone {
            self.unserve(live, &path);
        }
    }

    /// Take `path` off the watch it is placed on, which goes once it serves no path.
    fn unserve(&mut self, live: &Live, path: &Path) {
        let Some(spot) = self.looks.remove(path) else {
            return;
        };
        let key = (spot.level, spot.kind);
        if let Some(level) = self.levels.get_mut(&key) {
            level.serves.remove(path);
            if level.serves.is_empty() {
                live.remove(level.id);
                self.levels.remove(&key);
            } else {
                live.route(level.id, &level.serves);
            }
        }
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "claude_inputs_tests.rs"]
mod tests;
