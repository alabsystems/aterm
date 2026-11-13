// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What tells a parked setup run that a fact it reads may have moved, with no timer
//! ([`super::InitialSetupParkMark::watches`]): the secrets folder changing (the subscriber's
//! `state.toml`, a join's identity or source, and materialized `~/.secrets` files), this Mac's
//! network changing (the kernel's network-change events, [`aterm_uds::network::RouteChanges`]),
//! and a `git`
//! arriving (atpkg's, the Command Line Tools' or Xcode's), and the selected repository inputs
//! or pinned helper changing. A watch only
//! says "look again": the park reads its mark again and runs a pass only when a fact changed.
//! Each watch lets a burst settle before it says so, and the network watch says so at most once a
//! minute, so an interface that keeps going up and down moves a park no more often than the
//! minute's timer the watches replaced.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The facts a park watches ([`super::InitialSetupParkMark::watches`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Watches {
    /// The secrets folder: the vault's status, its materialization and the subscriber's state.
    pub secrets: bool,
    /// This Mac's network addresses.
    pub network: bool,
    /// Git activation and local repository inputs: the selected source and pinned helper.
    pub git: bool,
}

impl Watches {
    /// Whether nothing is watched: the park waits on a person, or on its one timer.
    #[must_use]
    pub const fn none(self) -> bool {
        !self.secrets && !self.network && !self.git
    }
}

/// Where Xcode's `git` lands when Xcode is installed where macOS puts it, which
/// `aterm_machines::items::github::installed_git` takes as well as the Command Line Tools'
/// ([`aterm_machines::items::github::CLT_GIT`]): both folders are watched while Repositories wait
/// on a `git`.
const XCODE_BIN: &str = "/Applications/Xcode.app/Contents/Developer/usr/bin";

/// The watches of one park, ended when dropped. Its threads are told to end and are not waited
/// for, so a park never waits on a watch to end.
pub struct ParkWatch {
    stop: Arc<AtomicBool>,
    /// Set by a watch that stopped hearing its fact after it started ([`ParkWatch::whole`]).
    lost: Arc<AtomicBool>,
    files: Option<Arc<atpkg::dirwatch::Watcher>>,
    network: Option<NetworkWatch>,
}

/// A park's network watch: the write end of the pipe its thread waits on beside the route
/// changes. Dropping it hangs the pipe up, which ends that wait; a kernel event socket takes no
/// `shutdown(2)` (it answers `ENOTCONN`, and a poll on it goes on).
struct NetworkWatch {
    _end: std::io::PipeWriter,
    /// The thread's route changes, gone once the thread ended.
    #[cfg(test)]
    routes: std::sync::Weak<aterm_uds::network::RouteChanges>,
}

impl ParkWatch {
    /// Start what `watches` names, each calling `changed` when its fact may have moved.
    ///
    /// # Errors
    /// A watch that could not start, with its cause (the kernel's errno for a network-change
    /// socket it refused): the park then reads its mark on its old timer.
    pub fn start(watches: Watches, changed: fn()) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let lost = Arc::new(AtomicBool::new(false));
        let mut watch = Self {
            stop: Arc::clone(&stop),
            lost: Arc::clone(&lost),
            files: None,
            network: None,
        };
        let mut folders = Vec::new();
        let mut input_files = Vec::new();
        let mut checkout_files = Vec::new();
        if watches.secrets {
            let home = aterm_types::dirs::home_dir()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
            folders.extend(secrets_folders(&home));
            input_files.extend(secrets_input_files(&home));
        }
        if watches.git {
            let tools = Path::new(aterm_machines::items::github::CLT_GIT)
                .parent()
                .map_or_else(PathBuf::new, Path::to_path_buf);
            folders.push((tools, PathBuf::from("/Library")).into());
            folders.push((PathBuf::from(XCODE_BIN), PathBuf::from("/Applications")).into());
            if let Some(layout) = atpkg::store::resolve_configured() {
                folders.extend(packaged_git_folders(&layout.prefix));
            }
            input_files.extend(
                aterm_machines::init_github::repository_input_control_watch_paths(
                    &aterm_machines::port::Live::new(),
                )
                .map_err(io::Error::other)?,
            );
            folders.extend(repository_input_folders(&input_files));
            let sys = aterm_machines::port::Live::new();
            let user = aterm_machines::port::System::identity(&sys)
                .map_err(io::Error::other)?
                .user;
            for path in aterm_machines::init_github::repository_checkout_watch_paths(&sys)
                .map_err(io::Error::other)?
            {
                folders.push(FolderTarget {
                    target: path.clone(),
                    floor: PathBuf::from(&user.home),
                    follow_links: false,
                    owner: Some(user.uid),
                });
                checkout_files.push((path.clone(), PathBuf::from(&user.home), user.uid));
                input_files.push(path);
            }
        }
        if !folders.is_empty() {
            watch.files = Some(if input_files.is_empty() {
                watch_folders(folders, Arc::clone(&stop), Arc::clone(&lost), changed)?
            } else {
                watch_paths_scoped(
                    folders,
                    input_files,
                    checkout_files,
                    Arc::clone(&stop),
                    Arc::clone(&lost),
                    changed,
                )?
            });
        }
        if watches.network {
            watch.network = Some(watch_network(stop, lost, changed)?);
        }
        Ok(watch)
    }

    /// Whether every watch still hears its fact: `false` once one failed after it started (its
    /// kernel queue or socket failed, or a folder that appeared could not be watched), so the
    /// park reads its facts on its old timer again.
    #[must_use]
    pub fn whole(&self) -> bool {
        !self.lost.load(Ordering::Acquire)
    }
}

impl Drop for ParkWatch {
    /// Tell each thread to end. The network thread's pipe hangs up when `network` drops, after
    /// this, so it finds `stop` set.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(files) = &self.files {
            let _ = files.wake();
        }
    }
}

/// A watch that stopped hearing its fact, unless the park ended it: marked `lost`, and the park
/// told to look again, so it takes its timer back.
fn lose(stop: &AtomicBool, lost: &AtomicBool, changed: fn()) {
    if !stop.load(Ordering::Acquire) {
        lost.store(true, Ordering::Release);
        changed();
    }
}

/// The deepest folder that exists on the way to `target`, no higher than `floor`: watching it
/// sees the next folder on the way appear, and the target's own entries once it is there.
fn nearest(target: &Path, floor: &Path, follow_links: bool) -> Option<PathBuf> {
    target
        .ancestors()
        .take_while(|folder| folder.starts_with(floor))
        .find(|folder| {
            if follow_links {
                folder.is_dir()
            } else {
                std::fs::symlink_metadata(folder).is_ok_and(|meta| meta.is_dir())
            }
        })
        .map(Path::to_path_buf)
}

struct FolderTarget {
    target: PathBuf,
    floor: PathBuf,
    follow_links: bool,
    owner: Option<u32>,
}

impl FolderTarget {
    fn nearest(&self) -> Option<PathBuf> {
        if let Some(uid) = self.owner {
            self.target
                .ancestors()
                .take_while(|path| path.starts_with(&self.floor))
                .find(|path| owned_directory(path, &self.floor, uid))
                .map(Path::to_path_buf)
        } else {
            nearest(&self.target, &self.floor, self.follow_links)
        }
    }
}

/// A selected checkout may be replaced with a link while a park is alive. Inspect every
/// component from its trusted home before opening a deeper directory or direct leaf.
fn owned_directory(path: &Path, floor: &Path, uid: u32) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let Ok(tail) = path.strip_prefix(floor) else {
        return false;
    };
    let mut at = floor.to_path_buf();
    let safe = |path: &Path| {
        std::fs::symlink_metadata(path)
            .is_ok_and(|s| s.is_dir() && s.uid() == uid && s.mode() & 0o022 == 0)
    };
    if !safe(&at) {
        return false;
    }
    for part in tail.components() {
        let std::path::Component::Normal(name) = part else {
            return false;
        };
        at.push(name);
        if !safe(&at) {
            return false;
        }
    }
    true
}

impl From<(PathBuf, PathBuf)> for FolderTarget {
    fn from((target, floor): (PathBuf, PathBuf)) -> Self {
        Self {
            target,
            floor,
            follow_links: true,
            owner: None,
        }
    }
}

/// State writes and output repairs can happen independently. Watch both, following missing
/// parents from home. Materialization refuses a symlink at `~/.secrets`, so watch its parent
/// until a real directory replaces it rather than opening the link's destination.
fn secrets_folders(home: &Path) -> Vec<FolderTarget> {
    vec![
        (home.join(".config/aterm/secrets"), home.to_path_buf()).into(),
        FolderTarget {
            target: home.join(".secrets"),
            floor: home.to_path_buf(),
            follow_links: false,
            owner: None,
        },
    ]
}

/// The canonical standing inputs also need leaf watches: an in-place write or permission
/// repair need not change any parent directory entry.
fn secrets_input_files(home: &Path) -> Vec<PathBuf> {
    aterm_secrets::client::Paths::locate_for_home(home).standing_files()
}

/// Package activation writes `current` and its ready record under `store/git`, and
/// lays the executable shim in `bin`. Watch both, including before the prefix exists.
fn packaged_git_folders(prefix: &Path) -> Vec<FolderTarget> {
    let floor = PathBuf::from("/");
    vec![
        (prefix.join("store/git"), floor.clone()).into(),
        (prefix.join("bin"), floor).into(),
    ]
}

/// Observe declared input files through their parents, including a missing parent or a
/// symlink's replacement. A new park resolves the source and helper again after they change.
fn repository_input_folders(paths: &[PathBuf]) -> Vec<FolderTarget> {
    paths
        .iter()
        .filter_map(|path| path.parent().map(Path::to_path_buf))
        .map(|parent| (parent, PathBuf::from("/")).into())
        .collect()
}

struct ArmedFolder {
    resolved: PathBuf,
    id: atpkg::dirwatch::WatchId,
}

/// Each target and its existing parent chain. Parents keep hearing namespace changes after
/// the destination appears, including a config link retargeted while its old destination lives.
struct Folders {
    targets: Vec<FolderTarget>,
    files: Vec<PathBuf>,
    scoped_files: BTreeMap<PathBuf, (PathBuf, u32)>,
    /// The nearest directory each target currently reaches.
    watched: Vec<Option<(PathBuf, atpkg::dirwatch::WatchId)>>,
    armed: BTreeMap<PathBuf, ArmedFolder>,
    armed_files: BTreeMap<PathBuf, atpkg::dirwatch::WatchId>,
}

impl Folders {
    fn new(targets: Vec<FolderTarget>) -> Self {
        let watched = targets.iter().map(|_| None).collect();
        Self {
            targets,
            files: Vec::new(),
            scoped_files: BTreeMap::new(),
            watched,
            armed: BTreeMap::new(),
            armed_files: BTreeMap::new(),
        }
    }

    /// Put each target's watch on its nearest existing folder now. A watch moves only once its
    /// new folder is watched, so one that cannot follow keeps the folder it had; and a folder
    /// that appeared on the way while the watch moved moves it again, since no watch saw it come.
    fn arm(&mut self, watcher: &atpkg::dirwatch::Watcher) -> io::Result<()> {
        self.arm_after(watcher, &BTreeSet::new())
    }

    /// A signaled directory can have been replaced at the same path. Open its current object
    /// before releasing the old watch; a rename does not make the old object cease to exist.
    fn arm_after(
        &mut self,
        watcher: &atpkg::dirwatch::Watcher,
        fired: &BTreeSet<atpkg::dirwatch::WatchId>,
    ) -> io::Result<()> {
        loop {
            let coverage: Vec<_> = self.targets.iter().map(FolderTarget::nearest).collect();
            let mut wanted = BTreeMap::<PathBuf, (bool, bool)>::new();
            for (target, closest) in self.targets.iter().zip(&coverage) {
                if let Some(closest) = closest {
                    for folder in closest
                        .ancestors()
                        .take_while(|folder| folder.starts_with(&target.floor))
                    {
                        // The materialized root itself refuses links. Its parents, including
                        // the login's home path, retain their ordinary path resolution.
                        let follow = target.owner.is_none()
                            && (target.follow_links || folder != target.target.as_path());
                        wanted
                            .entry(folder.to_path_buf())
                            .and_modify(|(links, scoped)| {
                                *links &= follow;
                                *scoped |= target.owner.is_some();
                            })
                            .or_insert((follow, target.owner.is_some()));
                    }
                }
            }
            // A parent can have a new object at the same spelling. Rebind its descendants
            // even when their canonical strings are unchanged after a directory replacement.
            let changed: Vec<_> = self
                .armed
                .iter()
                .filter(|(_, on)| fired.contains(&on.id) || watcher.is_gone(on.id))
                .map(|(path, _)| path.clone())
                .collect();
            let mut next = BTreeMap::new();
            let mut added = Vec::new();
            let result = (|| -> io::Result<()> {
                // Prefix paths sort before their children, so parents are armed first.
                for (path, (follow, scoped)) in wanted {
                    let resolved = if follow {
                        path.canonicalize()?
                    } else {
                        path.clone()
                    };
                    let keep = self.armed.get(&path).filter(|on| {
                        on.resolved == resolved
                            && !changed.iter().any(|parent| path.starts_with(parent))
                    });
                    let id = if let Some(on) = keep {
                        on.id
                    } else {
                        let id = if scoped {
                            watcher.add_dir_scoped(&resolved)?
                        } else {
                            watcher.add_dir(&resolved)?
                        };
                        added.push(id);
                        id
                    };
                    next.insert(path, ArmedFolder { resolved, id });
                }
                Ok(())
            })();
            if let Err(error) = result {
                for id in added {
                    watcher.remove(id);
                }
                return Err(error);
            }
            let kept: BTreeSet<_> = next.values().map(|on| on.id).collect();
            for on in self.armed.values() {
                if !kept.contains(&on.id) {
                    watcher.remove(on.id);
                }
            }
            self.armed = next;
            self.watched = coverage
                .iter()
                .map(|path| {
                    path.as_ref()
                        .and_then(|path| self.armed.get(path).map(|on| (path.clone(), on.id)))
                })
                .collect();
            // Creation before a parent was first armed must not leave the new directory
            // unwatched. Later creations are heard by the retained parent watches.
            if self
                .targets
                .iter()
                .zip(coverage)
                .all(|(target, before)| target.nearest() == before)
            {
                return self.arm_files(watcher, fired, &changed);
            }
        }
    }

    /// Parent watches hear replacement and arrival; file watches also hear in-place writes.
    /// Reopen a signaled or replaced file before releasing its old watch.
    fn arm_files(
        &mut self,
        watcher: &atpkg::dirwatch::Watcher,
        fired: &BTreeSet<atpkg::dirwatch::WatchId>,
        changed: &[PathBuf],
    ) -> io::Result<()> {
        let mut next = BTreeMap::new();
        let mut added = Vec::new();
        let armed = (|| {
            for path in &self.files {
                if let Some((floor, uid)) = self.scoped_files.get(path)
                    && !path
                        .parent()
                        .is_some_and(|parent| owned_directory(parent, floor, *uid))
                {
                    continue;
                }
                match std::fs::symlink_metadata(path) {
                    Ok(metadata) if metadata.is_file() => {}
                    Ok(_) => continue, // A link's parent hears its replacement; its target is separate.
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    // Named under a regular file (a records layout's files while the records
                    // entry is a plain file): not there, and the folder that holds that file
                    // hears it replaced by a directory.
                    Err(error) if error.kind() == io::ErrorKind::NotADirectory => continue,
                    Err(error) => return Err(error),
                }
                let kept = self.armed_files.get(path).filter(|id| {
                    !fired.contains(*id)
                        && !watcher.is_gone(**id)
                        && !changed.iter().any(|parent| path.starts_with(parent))
                });
                let id = if let Some(id) = kept {
                    *id
                } else {
                    let id = if self.scoped_files.contains_key(path) {
                        watcher.add_file_scoped(path)?
                    } else {
                        watcher.add_file(path)?
                    };
                    added.push(id);
                    id
                };
                next.insert(path.clone(), id);
            }
            Ok(())
        })();
        if let Err(error) = armed {
            for id in added {
                watcher.remove(id);
            }
            return Err(error);
        }
        let kept: BTreeSet<_> = next.values().copied().collect();
        for id in self.armed_files.values() {
            if !kept.contains(id) {
                watcher.remove(*id);
            }
        }
        self.armed_files = next;
        Ok(())
    }
}

/// How long a watched folder stays quiet before the park is told: a join writes several files,
/// and a tools install thousands, each within moments of the last.
const FILES_SETTLE: Duration = Duration::from_millis(200);

/// The longest a watched folder's burst is let settle before the park is told anyway.
const FILES_SETTLE_MOST: Duration = Duration::from_secs(2);

/// Watch `targets` ([`Folders`]) and call `changed` once each burst of changes settles, until
/// `stop`; a watch that fails is `lost` ([`lose`]).
fn watch_folders(
    targets: Vec<FolderTarget>,
    stop: Arc<AtomicBool>,
    lost: Arc<AtomicBool>,
    changed: fn(),
) -> io::Result<Arc<atpkg::dirwatch::Watcher>> {
    watch_paths(targets, Vec::new(), stop, lost, changed)
}

fn watch_paths(
    targets: Vec<FolderTarget>,
    files: Vec<PathBuf>,
    stop: Arc<AtomicBool>,
    lost: Arc<AtomicBool>,
    changed: fn(),
) -> io::Result<Arc<atpkg::dirwatch::Watcher>> {
    watch_paths_scoped(targets, files, Vec::new(), stop, lost, changed)
}

fn watch_paths_scoped(
    targets: Vec<FolderTarget>,
    mut files: Vec<PathBuf>,
    scoped: Vec<(PathBuf, PathBuf, u32)>,
    stop: Arc<AtomicBool>,
    lost: Arc<AtomicBool>,
    changed: fn(),
) -> io::Result<Arc<atpkg::dirwatch::Watcher>> {
    let watcher = Arc::new(atpkg::dirwatch::Watcher::new()?);
    let mut folders = Folders::new(targets);
    files.sort();
    files.dedup();
    folders.files = files;
    folders.scoped_files = scoped
        .into_iter()
        .map(|(path, floor, uid)| (path, (floor, uid)))
        .collect();
    folders.arm(&watcher)?;
    if folders.watched.iter().any(Option::is_none) {
        // Nothing at or below a floor to watch: the park keeps its timer.
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no folder to watch",
        ));
    }
    let thread_watcher = Arc::clone(&watcher);
    std::thread::Builder::new()
        .name("aterm-setup-park-files".into())
        .spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let mut fired: BTreeSet<_> = match thread_watcher.wait(None) {
                    Ok(fired) => fired.into_iter().collect(),
                    Err(_) => {
                        lose(&stop, &lost, changed);
                        return;
                    }
                };
                let settle = Instant::now() + FILES_SETTLE_MOST;
                while !stop.load(Ordering::Acquire) && Instant::now() < settle {
                    match thread_watcher.wait(Some(FILES_SETTLE)) {
                        Ok(batch) if !batch.is_empty() => fired.extend(batch),
                        Ok(_) => break,
                        Err(_) => {
                            lose(&stop, &lost, changed);
                            return;
                        }
                    }
                }
                if stop.load(Ordering::Acquire) {
                    return;
                }
                // A folder that appeared moves its watch deeper before the park looks again.
                if folders.arm_after(&thread_watcher, &fired).is_err()
                    || folders.watched.iter().any(Option::is_none)
                {
                    lose(&stop, &lost, changed);
                    return;
                }
                changed();
            }
        })?;
    Ok(watcher)
}

/// How long the network stays quiet after an address or interface message before the park is
/// told: joining a network comes as a burst (the link, then each address), and one look after it
/// sees where it settled.
const NETWORK_SETTLE: Duration = Duration::from_secs(2);

/// The longest a network burst is let settle: a network that never goes quiet is looked at anyway.
const NETWORK_SETTLE_MOST: Duration = Duration::from_secs(10);

/// The least time between two tells of the network watch: an interface that keeps going up and
/// down (a peer-to-peer link, a VPN) moves the park at most once a minute, as the timer the watch
/// replaced did.
const NETWORK_SPACING: Duration = Duration::from_secs(60);

/// When the network watch last told a park: shared by every park's watch, since each pass's park
/// starts a new one, so the spacing holds across parks.
static NETWORK_TOLD: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

/// When the network watch tells the park of the burst it heard from `first` to `last`, having
/// last told one at `told` ([`NETWORK_TOLD`]): once the burst settles ([`NETWORK_SETTLE`], at most
/// [`NETWORK_SETTLE_MOST`] after it began), and never within [`NETWORK_SPACING`] of the last tell.
fn network_tell_at(first: Instant, last: Instant, told: Option<Instant>) -> Instant {
    let settled = (last + NETWORK_SETTLE).min(first + NETWORK_SETTLE_MOST);
    told.map_or(settled, |told| settled.max(told + NETWORK_SPACING))
}

/// A thread waiting on the kernel's network-change events ([`aterm_uds::network::RouteChanges`]),
/// calling `changed` once each burst of them settles ([`network_tell_at`]), until the park drops
/// its [`NetworkWatch`]. Events that name no change (a router advertisement, a link-quality
/// report) are read and passed over; a socket that fails is `lost` ([`lose`]). One the kernel
/// refuses fails the watch with its reason ([`aterm_uds::network::RouteChanges::open`]).
fn watch_network(
    stop: Arc<AtomicBool>,
    lost: Arc<AtomicBool>,
    changed: fn(),
) -> io::Result<NetworkWatch> {
    watch_network_over(
        aterm_uds::network::RouteChanges::open()?,
        stop,
        lost,
        changed,
    )
}

/// [`watch_network`] over `routes`, which a test makes a stand-in.
fn watch_network_over(
    routes: aterm_uds::network::RouteChanges,
    stop: Arc<AtomicBool>,
    lost: Arc<AtomicBool>,
    changed: fn(),
) -> io::Result<NetworkWatch> {
    use aterm_uds::network::RouteWake;
    use std::os::fd::AsFd as _;
    let (ended, end) = aterm_uds::pipe()?;
    let routes = Arc::new(routes);
    #[cfg(test)]
    let watched = Arc::downgrade(&routes);
    std::thread::Builder::new()
        .name("aterm-setup-park-network".into())
        .spawn(move || {
            let mut burst: Option<(Instant, Instant)> = None;
            while !stop.load(Ordering::Acquire) {
                let most = match burst {
                    None => Duration::MAX,
                    Some((first, last)) => {
                        let now = Instant::now();
                        let mut told = NETWORK_TOLD
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let at = network_tell_at(first, last, *told);
                        if at <= now {
                            *told = Some(now);
                            drop(told);
                            changed();
                            burst = None;
                            continue;
                        }
                        drop(told);
                        at.duration_since(now)
                    }
                };
                match routes.wait_beside(most, Some(ended.as_fd())) {
                    RouteWake::Change => {
                        let now = Instant::now();
                        burst = Some((burst.map_or(now, |(first, _)| first), now));
                    }
                    RouteWake::Timeout => {}
                    // The park dropped its watch, which hung the pipe up.
                    RouteWake::Also => return,
                    RouteWake::Failed => {
                        lose(&stop, &lost, changed);
                        return;
                    }
                }
            }
        })?;
    Ok(NetworkWatch {
        _end: end,
        #[cfg(test)]
        routes: watched,
    })
}

#[cfg(test)]
#[path = "park_watch_tests.rs"]
mod tests;
