// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What tells a parked setup run that a fact it reads may have moved, with no timer
//! ([`super::InitialSetupParkMark::watches`]): the secrets folder changing (the subscriber's
//! `state.toml`, a join's identity or source, and materialized `~/.secrets` files), this Mac's
//! network addresses changing (a route socket's address and interface messages), and a `git`
//! arriving (atpkg's, the Command Line Tools' or Xcode's), and the selected repository inputs
//! or pinned helper changing. A watch only
//! says "look again": the park reads its mark again and runs a pass only when a fact changed.
//! Each watch lets a burst settle before it says so, and the network watch says so at most once a
//! minute, so an interface that keeps going up and down moves a park no more often than the
//! minute's timer the watches replaced.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
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
    network: Option<Arc<OwnedFd>>,
}

impl ParkWatch {
    /// Start what `watches` names, each calling `changed` when its fact may have moved.
    ///
    /// # Errors
    /// A watch that could not start: the park then reads its mark on its old timer.
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
        if watches.secrets {
            let home = aterm_types::dirs::home_dir()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
            folders.extend(secrets_folders(&home));
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
            input_files = aterm_machines::init_github::repository_input_watch_paths(
                &aterm_machines::port::Live::new(),
            )
            .map_err(io::Error::other)?;
            folders.extend(repository_input_folders(&input_files));
        }
        if !folders.is_empty() {
            watch.files = Some(if input_files.is_empty() {
                watch_folders(folders, Arc::clone(&stop), Arc::clone(&lost), changed)?
            } else {
                watch_paths(
                    folders,
                    input_files,
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
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(files) = &self.files {
            let _ = files.wake();
        }
        if let Some(route) = &self.network {
            // SAFETY: `route` is an open socket this watch owns for as long as the Arc lives;
            // shutting it down ends the thread's blocked poll, and it closes when the last Arc
            // goes.
            unsafe { libc::shutdown(route.as_raw_fd(), libc::SHUT_RDWR) };
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
}

impl From<(PathBuf, PathBuf)> for FolderTarget {
    fn from((target, floor): (PathBuf, PathBuf)) -> Self {
        Self {
            target,
            floor,
            follow_links: true,
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
        },
    ]
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
            let coverage: Vec<_> = self
                .targets
                .iter()
                .map(|target| nearest(&target.target, &target.floor, target.follow_links))
                .collect();
            let mut wanted = BTreeMap::<PathBuf, bool>::new();
            for (target, closest) in self.targets.iter().zip(&coverage) {
                if let Some(closest) = closest {
                    for folder in closest
                        .ancestors()
                        .take_while(|folder| folder.starts_with(&target.floor))
                    {
                        // The materialized root itself refuses links. Its parents, including
                        // the login's home path, retain their ordinary path resolution.
                        let follow = target.follow_links || folder != target.target.as_path();
                        wanted
                            .entry(folder.to_path_buf())
                            .and_modify(|links| *links &= follow)
                            .or_insert(follow);
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
                for (path, follow) in wanted {
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
                        let id = watcher.add_dir(&resolved)?;
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
            if self.targets.iter().zip(coverage).all(|(target, before)| {
                nearest(&target.target, &target.floor, target.follow_links) == before
            }) {
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
                match std::fs::symlink_metadata(path) {
                    Ok(metadata) if metadata.is_file() => {}
                    Ok(_) => continue, // A link's parent hears its replacement; its target is separate.
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
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
                    let id = watcher.add_file(path)?;
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
    mut files: Vec<PathBuf>,
    stop: Arc<AtomicBool>,
    lost: Arc<AtomicBool>,
    changed: fn(),
) -> io::Result<Arc<atpkg::dirwatch::Watcher>> {
    let watcher = Arc::new(atpkg::dirwatch::Watcher::new()?);
    let mut folders = Folders::new(targets);
    files.sort();
    files.dedup();
    folders.files = files;
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

/// Darwin's routing message types for an address added or removed and an interface's state
/// (`<net/route.h>`'s `RTM_NEWADDR`, `RTM_DELADDR`, `RTM_IFINFO`); the routes a LAN's neighbours
/// come and go by are not watched.
const ADDRESS_MESSAGES: [u8; 3] = [0xc, 0xd, 0xe];

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

/// A route socket's thread, calling `changed` once each burst of address or interface messages
/// settles ([`network_tell_at`]), until `stop`; a socket that fails is `lost` ([`lose`]).
fn watch_network(
    stop: Arc<AtomicBool>,
    lost: Arc<AtomicBool>,
    changed: fn(),
) -> io::Result<Arc<OwnedFd>> {
    // Kept out of children from its creation (`aterm_uds::kept_out`); close-on-exec is set
    // before the mark, as `F_SETFD` replaces every descriptor flag.
    let route = Arc::new(aterm_uds::kept_out(|| {
        // SAFETY: socket(2) with constant arguments; a non-negative result is a new descriptor
        // this function owns.
        let raw = unsafe { libc::socket(libc::PF_ROUTE, libc::SOCK_RAW, libc::AF_UNSPEC) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is the open descriptor socket(2) just returned, owned by nothing else.
        let route = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: F_SETFD changes only this descriptor's close-on-exec flag.
        if unsafe { libc::fcntl(route.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(route)
    })?);
    let thread_route = Arc::clone(&route);
    std::thread::Builder::new()
        .name("aterm-setup-park-network".into())
        .spawn(move || {
            let mut message = [0u8; 2048];
            let mut burst: Option<(Instant, Instant)> = None;
            while !stop.load(Ordering::Acquire) {
                let timeout = match burst {
                    None => -1,
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
                        libc::c_int::try_from(at.duration_since(now).as_millis())
                            .unwrap_or(libc::c_int::MAX)
                            .max(1)
                    }
                };
                let mut ready = libc::pollfd {
                    fd: thread_route.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one pollfd for a descriptor the Arc keeps open.
                let polled = unsafe { libc::poll(&mut ready, 1, timeout) };
                if polled < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                if polled < 0 {
                    lose(&stop, &lost, changed);
                    return;
                }
                if polled == 0 {
                    continue;
                }
                // SAFETY: reads into `message`, a buffer of its stated length, from a descriptor
                // the Arc keeps open.
                let read = unsafe {
                    libc::read(
                        thread_route.as_raw_fd(),
                        message.as_mut_ptr().cast(),
                        message.len(),
                    )
                };
                if read < 0
                    && matches!(
                        io::Error::last_os_error().kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    )
                {
                    continue;
                }
                // The watch shut the socket down, or it failed: nothing more is heard.
                if read <= 0 {
                    lose(&stop, &lost, changed);
                    return;
                }
                // `rt_msghdr`: a u16 length, a u8 version, then the u8 type.
                if read >= 4 && ADDRESS_MESSAGES.contains(&message[3]) {
                    let now = Instant::now();
                    burst = Some((burst.map_or(now, |(first, _)| first), now));
                }
            }
        })?;
    Ok(route)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Wait, as a one-minute hang detector, until `done`.
    fn until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A target's watch sits on the deepest folder there, never above its floor, and moves
    /// deeper once the next folder on the way appears.
    #[test]
    fn a_watch_moves_deeper_as_folders_appear() {
        let home = aterm_tempfile::TempDir::new().unwrap();
        let floor = home.path().join(".config");
        let target = floor.join("aterm/secrets");
        let watcher = atpkg::dirwatch::Watcher::new().unwrap();
        let mut folders = Folders::new(vec![(target.clone(), floor.clone()).into()]);
        let on = |folders: &Folders| {
            folders.watched[0]
                .as_ref()
                .map(|(folder, _)| folder.clone())
        };
        folders.arm(&watcher).unwrap();
        assert_eq!(
            on(&folders),
            None,
            "nothing at or below the floor to watch yet"
        );
        std::fs::create_dir_all(&floor).unwrap();
        folders.arm(&watcher).unwrap();
        assert_eq!(on(&folders), Some(floor));
        std::fs::create_dir_all(&target).unwrap();
        folders.arm(&watcher).unwrap();
        assert_eq!(on(&folders), Some(target));
    }

    static FIRED: AtomicUsize = AtomicUsize::new(0);

    fn fired() {
        FIRED.fetch_add(1, Ordering::SeqCst);
    }

    /// The subscriber writing its state where a park watches tells the park to look again.
    #[test]
    fn a_write_in_a_watched_folder_tells_the_park() {
        let home = aterm_tempfile::TempDir::new().unwrap();
        let floor = home.path().join(".config");
        let target = floor.join("aterm/secrets");
        std::fs::create_dir_all(&target).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let lost = Arc::new(AtomicBool::new(false));
        let watcher = watch_folders(
            vec![(target.clone(), floor).into()],
            Arc::clone(&stop),
            Arc::clone(&lost),
            fired,
        )
        .unwrap();
        std::fs::write(target.join("state.toml"), "seq = 1\n").unwrap();
        until("the watch never fired", || FIRED.load(Ordering::SeqCst) > 0);
        assert!(!lost.load(Ordering::Acquire));
        stop.store(true, Ordering::Release);
        watcher.wake().unwrap();
    }

    static MATERIALIZED_FIRED: AtomicUsize = AtomicUsize::new(0);

    static GIT_FIRED: AtomicUsize = AtomicUsize::new(0);

    static REPOSITORY_INPUT_FIRED: AtomicUsize = AtomicUsize::new(0);

    fn repository_input_fired() {
        REPOSITORY_INPUT_FIRED.fetch_add(1, Ordering::SeqCst);
    }

    static DANGLING_HELPER_FIRED: AtomicUsize = AtomicUsize::new(0);

    fn dangling_helper_fired() {
        DANGLING_HELPER_FIRED.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn a_dangling_helper_target_arriving_rebinds_its_file_watch() {
        use std::io::Write as _;

        for parent_link in [false, true] {
            let root = aterm_tempfile::TempDir::new().unwrap();
            let base = root.path().canonicalize().unwrap();
            let home = base.join("home");
            let launcher = home.join(".local/bin/aterm");
            let target = base.join("package/bin/aterm");
            let declaration = if parent_link {
                std::fs::create_dir_all(home.join(".local")).unwrap();
                let link = home.join(".local/bin");
                std::os::unix::fs::symlink(target.parent().unwrap(), &link).unwrap();
                link
            } else {
                std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::os::unix::fs::symlink(&target, &launcher).unwrap();
                launcher.clone()
            };
            assert!(launcher.canonicalize().is_err(), "the helper is absent");
            let original = std::fs::read_link(&declaration).unwrap();
            let files = vec![launcher.clone(), target.clone()];
            let mut folders = repository_input_folders(&files);
            for folder in &mut folders {
                folder.floor = base.clone();
            }
            let stop = Arc::new(AtomicBool::new(false));
            let lost = Arc::new(AtomicBool::new(false));
            let watcher = watch_paths(
                folders,
                files,
                Arc::clone(&stop),
                Arc::clone(&lost),
                dangling_helper_fired,
            )
            .unwrap();
            if parent_link {
                let before = DANGLING_HELPER_FIRED.load(Ordering::SeqCst);
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                until("the missing helper parent was not heard", || {
                    DANGLING_HELPER_FIRED.load(Ordering::SeqCst) > before
                });
            }
            let before = DANGLING_HELPER_FIRED.load(Ordering::SeqCst);
            std::fs::write(&target, "helper01").unwrap();
            until("the dangling helper target's arrival was not heard", || {
                DANGLING_HELPER_FIRED.load(Ordering::SeqCst) > before
            });
            assert_eq!(launcher.canonicalize().unwrap(), target);
            assert_eq!(std::fs::read_link(&declaration).unwrap(), original);

            let before = DANGLING_HELPER_FIRED.load(Ordering::SeqCst);
            let mut helper = std::fs::OpenOptions::new()
                .write(true)
                .open(&target)
                .unwrap();
            helper.write_all(b"helper02").unwrap();
            drop(helper);
            until("the arriving helper's file watch was not rebound", || {
                DANGLING_HELPER_FIRED.load(Ordering::SeqCst) > before
            });
            assert!(!lost.load(Ordering::Acquire));
            stop.store(true, Ordering::Release);
            watcher.wake().unwrap();
            let ended = Arc::downgrade(&watcher);
            drop(watcher);
            until("the helper watch outlived its park", || {
                ended.upgrade().is_none()
            });
        }
    }

    #[test]
    fn selected_inputs_and_helper_wake_for_in_place_writes_and_replacement() {
        use std::io::Write as _;
        use std::os::unix::fs::MetadataExt as _;

        let root = aterm_tempfile::TempDir::new().unwrap();
        let home = root.path().join("home");
        let profile = home.join(".config/aterm/init.toml");
        let token = root.path().join("team/private-token");
        let helper = root.path().join("application/Contents/MacOS/aterm");
        let launcher = home.join(".local/bin/aterm");
        for file in [&profile, &token, &helper, &launcher] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        }
        for file in [&profile, &token, &helper] {
            std::fs::write(file, "before01").unwrap();
        }
        std::os::unix::fs::symlink(&helper, &launcher).unwrap();
        let files = vec![profile.clone(), token.clone(), launcher, helper.clone()];
        let mut folders = repository_input_folders(&files);
        // The production watch follows parent chains to root; this isolated tree is the
        // test's namespace, so unrelated tests cannot supply its notification.
        for target in &mut folders {
            target.floor = root.path().to_path_buf();
        }
        let stop = Arc::new(AtomicBool::new(false));
        let lost = Arc::new(AtomicBool::new(false));
        let watcher = watch_paths(
            folders,
            files,
            Arc::clone(&stop),
            Arc::clone(&lost),
            repository_input_fired,
        )
        .unwrap();
        let rewrite = |path: &Path, bytes: &[u8]| {
            let before = REPOSITORY_INPUT_FIRED.load(Ordering::SeqCst);
            let inode = std::fs::metadata(path).unwrap().ino();
            let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
            file.write_all(bytes).unwrap();
            drop(file);
            assert_eq!(std::fs::metadata(path).unwrap().ino(), inode);
            until("the in-place repository input change was not heard", || {
                REPOSITORY_INPUT_FIRED.load(Ordering::SeqCst) > before
            });
        };
        rewrite(&profile, b"profile2");
        rewrite(&token, b"token002");
        rewrite(&helper, b"helper02");

        let before = REPOSITORY_INPUT_FIRED.load(Ordering::SeqCst);
        let replacement = token.with_extension("replacement");
        std::fs::write(&replacement, "replaced").unwrap();
        std::fs::rename(replacement, &token).unwrap();
        until("the token replacement was not heard", || {
            REPOSITORY_INPUT_FIRED.load(Ordering::SeqCst) > before
        });
        rewrite(&token, b"token003");
        assert!(!lost.load(Ordering::Acquire));
        stop.store(true, Ordering::Release);
        watcher.wake().unwrap();
        let ended = Arc::downgrade(&watcher);
        drop(watcher);
        until("the input watch outlived its park", || {
            ended.upgrade().is_none()
        });
    }

    fn git_fired() {
        GIT_FIRED.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn packaged_git_activation_wakes_a_park_waiting_for_git() {
        let home = aterm_tempfile::TempDir::new().unwrap();
        let prefix = home.path().join("pkg");
        std::fs::create_dir_all(prefix.join("store/git")).unwrap();
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        let current = prefix.join("store/git/current");
        let next = prefix.join("store/git/current.next");
        std::os::unix::fs::symlink("41", &current).unwrap();
        std::os::unix::fs::symlink("42", &next).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let lost = Arc::new(AtomicBool::new(false));
        let watcher = watch_folders(
            packaged_git_folders(&prefix),
            Arc::clone(&stop),
            Arc::clone(&lost),
            git_fired,
        )
        .unwrap();
        let before = GIT_FIRED.load(Ordering::SeqCst);
        std::fs::rename(next, current).unwrap();
        until("the managed Git activation link never woke setup", || {
            GIT_FIRED.load(Ordering::SeqCst) > before
        });
        let before = GIT_FIRED.load(Ordering::SeqCst);
        std::fs::write(prefix.join("store/git/42.ready"), "ready").unwrap();
        until("the managed Git activation never woke setup", || {
            GIT_FIRED.load(Ordering::SeqCst) > before
        });
        let before = GIT_FIRED.load(Ordering::SeqCst);
        std::fs::write(prefix.join("bin/git"), "shim").unwrap();
        until("the managed Git shim never woke setup", || {
            GIT_FIRED.load(Ordering::SeqCst) > before
        });
        assert!(!lost.load(Ordering::Acquire));
        stop.store(true, Ordering::Release);
        watcher.wake().unwrap();
    }

    fn materialized_fired() {
        MATERIALIZED_FIRED.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn a_materialized_file_repair_wakes_without_a_vault_state_write() {
        let home = aterm_tempfile::TempDir::new().unwrap();
        let state_dir = home.path().join(".config/aterm/secrets");
        let output = home.path().join(".secrets");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::create_dir(&output).unwrap();
        let state = state_dir.join("state.toml");
        std::fs::write(&state, "seq = 7\n").unwrap();
        let credential = output.join("test_credential");
        std::fs::write(&credential, "before").unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let lost = Arc::new(AtomicBool::new(false));
        let watcher = watch_folders(
            secrets_folders(home.path()),
            Arc::clone(&stop),
            Arc::clone(&lost),
            materialized_fired,
        )
        .unwrap();
        let replacement = output.join("replacement");
        std::fs::write(&replacement, "after").unwrap();
        std::fs::rename(replacement, credential).unwrap();
        until("the materialized repair was never heard", || {
            MATERIALIZED_FIRED.load(Ordering::SeqCst) > 0
        });
        assert_eq!(std::fs::read_to_string(state).unwrap(), "seq = 7\n");
        assert!(!lost.load(Ordering::Acquire));
        stop.store(true, Ordering::Release);
        watcher.wake().unwrap();
    }

    #[test]
    fn a_materialized_root_symlink_is_watched_at_its_parent() {
        let home = aterm_tempfile::TempDir::new().unwrap();
        let elsewhere = aterm_tempfile::TempDir::new().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), home.path().join(".secrets")).unwrap();
        let watcher = atpkg::dirwatch::Watcher::new().unwrap();
        let mut folders = Folders::new(secrets_folders(home.path()));
        folders.arm(&watcher).unwrap();
        assert_eq!(
            folders.watched[1].as_ref().map(|(path, _)| path.as_path()),
            Some(home.path()),
            "materialization refuses the symlink; only its replacement can make progress"
        );
    }

    static REPLACED_FIRED: AtomicUsize = AtomicUsize::new(0);

    fn replaced_fired() {
        REPLACED_FIRED.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn a_replaced_directory_keeps_hearing_its_new_contents() {
        let home = aterm_tempfile::TempDir::new().unwrap();
        let target = home.path().join("watched");
        std::fs::create_dir(&target).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let lost = Arc::new(AtomicBool::new(false));
        let watcher = watch_folders(
            vec![(target.clone(), home.path().to_path_buf()).into()],
            Arc::clone(&stop),
            Arc::clone(&lost),
            replaced_fired,
        )
        .unwrap();
        std::fs::rename(&target, home.path().join("previous")).unwrap();
        std::fs::create_dir(&target).unwrap();
        until("the directory replacement was never heard", || {
            REPLACED_FIRED.load(Ordering::SeqCst) > 0
        });
        assert!(!lost.load(Ordering::Acquire));
        let before = REPLACED_FIRED.load(Ordering::SeqCst);
        std::fs::write(target.join("state.toml"), "seq = 2\n").unwrap();
        until("the watch stayed on the renamed old directory", || {
            REPLACED_FIRED.load(Ordering::SeqCst) > before
        });
        assert!(!lost.load(Ordering::Acquire));
        stop.store(true, Ordering::Release);
        watcher.wake().unwrap();
    }

    static RETARGET_FIRED: AtomicUsize = AtomicUsize::new(0);

    fn retarget_fired() {
        RETARGET_FIRED.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn a_retargeted_config_link_keeps_hearing_the_new_vault() {
        let home = aterm_tempfile::TempDir::new().unwrap();
        let first = aterm_tempfile::TempDir::new().unwrap();
        let second = aterm_tempfile::TempDir::new().unwrap();
        for root in [first.path(), second.path()] {
            std::fs::create_dir_all(root.join("aterm/secrets")).unwrap();
        }
        std::fs::create_dir(home.path().join(".secrets")).unwrap();
        let config = home.path().join(".config");
        std::os::unix::fs::symlink(first.path(), &config).unwrap();
        let replacement = home.path().join("next-config");
        std::os::unix::fs::symlink(second.path(), &replacement).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let lost = Arc::new(AtomicBool::new(false));
        let watcher = watch_folders(
            secrets_folders(home.path()),
            Arc::clone(&stop),
            Arc::clone(&lost),
            retarget_fired,
        )
        .unwrap();
        std::fs::rename(replacement, config).unwrap();
        until("the config link retarget was never heard", || {
            RETARGET_FIRED.load(Ordering::SeqCst) > 0
        });
        let before = RETARGET_FIRED.load(Ordering::SeqCst);
        std::fs::write(second.path().join("aterm/secrets/state.toml"), "seq = 2\n").unwrap();
        until(
            "the watch stayed on the previous config destination",
            || RETARGET_FIRED.load(Ordering::SeqCst) > before,
        );
        assert!(!lost.load(Ordering::Acquire));
        stop.store(true, Ordering::Release);
        watcher.wake().unwrap();
    }

    #[test]
    fn a_parent_replacement_rebinds_the_descendant_watch() {
        let home = aterm_tempfile::TempDir::new().unwrap();
        let parent = home.path().join("config");
        let target = parent.join("aterm/secrets");
        std::fs::create_dir_all(&target).unwrap();
        let watcher = atpkg::dirwatch::Watcher::new().unwrap();
        let mut folders = Folders::new(vec![(target.clone(), home.path().to_path_buf()).into()]);
        folders.arm(&watcher).unwrap();
        std::fs::rename(parent, home.path().join("previous-config")).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let fired: BTreeSet<_> = watcher
            .wait(Some(Duration::from_secs(60)))
            .unwrap()
            .into_iter()
            .collect();
        assert!(!fired.is_empty(), "a parent must hear the namespace change");
        folders.arm_after(&watcher, &fired).unwrap();
        let current = folders.watched[0].as_ref().unwrap().1;
        std::fs::write(target.join("state.toml"), "seq = 3\n").unwrap();
        until("the descendant watch stayed on the old parent", || {
            watcher
                .wait(Some(Duration::ZERO))
                .unwrap()
                .contains(&current)
        });
    }

    static FLOOR_LOST_TOLD: AtomicUsize = AtomicUsize::new(0);

    fn floor_lost_told() {
        FLOOR_LOST_TOLD.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn a_missing_watch_floor_returns_the_fallback_timer() {
        let home = aterm_tempfile::TempDir::new().unwrap();
        let floor = home.path().join("floor");
        std::fs::create_dir(&floor).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let lost = Arc::new(AtomicBool::new(false));
        let watcher = watch_folders(
            vec![(floor.clone(), floor.clone()).into()],
            Arc::clone(&stop),
            Arc::clone(&lost),
            floor_lost_told,
        )
        .unwrap();
        std::fs::remove_dir(floor).unwrap();
        until(
            "a park with no remaining watch must regain its timer",
            || lost.load(Ordering::Acquire) && FLOOR_LOST_TOLD.load(Ordering::SeqCst) > 0,
        );
        stop.store(true, Ordering::Release);
        watcher.wake().unwrap();
    }

    static LOST_TOLD: AtomicUsize = AtomicUsize::new(0);

    fn lost_told() {
        LOST_TOLD.fetch_add(1, Ordering::SeqCst);
    }

    /// A folder that appears where no watch can follow (this login cannot read it) leaves the
    /// watch lost and tells the park, which takes its timer back rather than wait unheard.
    #[test]
    fn a_folder_the_watch_cannot_follow_gives_the_park_its_timer_back() {
        use std::os::unix::fs::PermissionsExt;
        let home = aterm_tempfile::TempDir::new().unwrap();
        let floor = home.path().join(".config");
        std::fs::create_dir_all(&floor).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let lost = Arc::new(AtomicBool::new(false));
        let watcher = watch_folders(
            vec![(floor.join("aterm/secrets"), floor.clone()).into()],
            Arc::clone(&stop),
            Arc::clone(&lost),
            lost_told,
        )
        .unwrap();
        let unreadable = floor.join("aterm");
        std::fs::create_dir(&unreadable).unwrap();
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
        until("the watch never said it was lost", || {
            lost.load(Ordering::Acquire) && LOST_TOLD.load(Ordering::SeqCst) > 0
        });
        stop.store(true, Ordering::Release);
        watcher.wake().unwrap();
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// The network watch tells the park once a burst settles, by ten seconds into one that never
    /// does, and never within a minute of its last tell.
    #[test]
    fn the_network_watch_tells_once_a_burst_settles_and_at_most_once_a_minute() {
        let start = Instant::now();
        let at = |seconds| start + Duration::from_secs(seconds);
        assert_eq!(
            network_tell_at(start, at(3), None),
            at(5),
            "a join: the link, then its addresses"
        );
        assert_eq!(network_tell_at(start, at(30), None), at(10), "never quiet");
        assert_eq!(
            network_tell_at(at(5), at(5), Some(start)),
            at(60),
            "a burst soon after a tell waits out the minute"
        );
        assert_eq!(network_tell_at(at(90), at(90), Some(start)), at(92));
    }

    static NETWORK_TOLD: AtomicUsize = AtomicUsize::new(0);

    fn network_told() {
        NETWORK_TOLD.fetch_add(1, Ordering::SeqCst);
    }

    /// A park's end ends its network watch's thread (its socket closes with it), and is no loss.
    #[test]
    fn a_dropped_park_watch_ends_its_network_thread() {
        let watch = ParkWatch::start(
            Watches {
                network: true,
                ..Watches::default()
            },
            network_told,
        )
        .unwrap();
        let route_fd = watch.network.as_ref().unwrap().as_raw_fd();
        // SAFETY: F_GETFD reads the descriptor's flags without mutating state.
        let flags = unsafe { libc::fcntl(route_fd, libc::F_GETFD) };
        assert!(flags >= 0, "F_GETFD failed");
        assert_ne!(
            flags & libc::FD_CLOEXEC,
            0,
            "route socket must be close-on-exec"
        );
        let socket = Arc::downgrade(watch.network.as_ref().unwrap());
        let lost = Arc::clone(&watch.lost);
        drop(watch);
        until("the network thread outlived its park", || {
            socket.upgrade().is_none()
        });
        assert!(!lost.load(Ordering::Acquire));
        assert_eq!(NETWORK_TOLD.load(Ordering::SeqCst), 0);
    }
}
