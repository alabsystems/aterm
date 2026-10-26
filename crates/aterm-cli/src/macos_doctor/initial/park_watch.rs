// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What tells a parked setup run that a fact it reads may have moved, with no timer
//! ([`super::InitialSetupParkMark::watches`]): the secrets folder changing (the subscriber's
//! `state.toml`, a join's identity or source), this Mac's network addresses changing (a route
//! socket's address and interface messages), and a `git` arriving (the Command Line Tools' or
//! Xcode's). A watch only
//! says "look again": the park reads its mark again and runs a pass only when a fact changed.
//! Each watch lets a burst settle before it says so, and the network watch says so at most once a
//! minute, so an interface that keeps going up and down moves a park no more often than the
//! minute's timer the watches replaced.

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
    /// A `git` arriving: the Command Line Tools', or Xcode's where macOS installs it.
    pub developer_tools: bool,
}

impl Watches {
    /// Whether nothing is watched: the park waits on a person, or on its one timer.
    #[must_use]
    pub const fn none(self) -> bool {
        !self.secrets && !self.network && !self.developer_tools
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
        if watches.secrets {
            let home = aterm_types::dirs::home_dir()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
            folders.push((home.join(".config/aterm/secrets"), home.join(".config")));
        }
        if watches.developer_tools {
            let tools = Path::new(aterm_machines::items::github::CLT_GIT)
                .parent()
                .map_or_else(PathBuf::new, Path::to_path_buf);
            folders.push((tools, PathBuf::from("/Library")));
            folders.push((PathBuf::from(XCODE_BIN), PathBuf::from("/Applications")));
        }
        if !folders.is_empty() {
            watch.files = Some(watch_folders(
                folders,
                Arc::clone(&stop),
                Arc::clone(&lost),
                changed,
            )?);
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
fn nearest(target: &Path, floor: &Path) -> Option<PathBuf> {
    target
        .ancestors()
        .take_while(|folder| folder.starts_with(floor))
        .find(|folder| folder.is_dir())
        .map(Path::to_path_buf)
}

/// The folders a park watches: each target's nearest existing folder ([`nearest`]), moved deeper
/// as folders on the way appear.
struct Folders {
    /// Each `(target, floor)`.
    targets: Vec<(PathBuf, PathBuf)>,
    /// What each target's watch is on now.
    watched: Vec<Option<(PathBuf, atpkg::dirwatch::WatchId)>>,
}

impl Folders {
    fn new(targets: Vec<(PathBuf, PathBuf)>) -> Self {
        let watched = targets.iter().map(|_| None).collect();
        Self { targets, watched }
    }

    /// Put each target's watch on its nearest existing folder now. A watch moves only once its
    /// new folder is watched, so one that cannot follow keeps the folder it had; and a folder
    /// that appeared on the way while the watch moved moves it again, since no watch saw it come.
    fn arm(&mut self, watcher: &atpkg::dirwatch::Watcher) -> io::Result<()> {
        for ((target, floor), slot) in self.targets.iter().zip(self.watched.iter_mut()) {
            loop {
                let want = nearest(target, floor);
                if slot.as_ref().map(|(folder, _)| folder) == want.as_ref() {
                    break;
                }
                // A folder reached through a symbolic link (a `~/.config` kept elsewhere) is
                // watched where it is: the kernel watch opens no link.
                let added = match want {
                    Some(folder) => {
                        let id = watcher.add_dir(&folder.canonicalize()?)?;
                        Some((folder, id))
                    }
                    None => None,
                };
                if let Some((_, id)) = std::mem::replace(slot, added) {
                    watcher.remove(id);
                }
            }
        }
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
    targets: Vec<(PathBuf, PathBuf)>,
    stop: Arc<AtomicBool>,
    lost: Arc<AtomicBool>,
    changed: fn(),
) -> io::Result<Arc<atpkg::dirwatch::Watcher>> {
    let watcher = Arc::new(atpkg::dirwatch::Watcher::new()?);
    let mut folders = Folders::new(targets);
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
                if thread_watcher.wait(None).is_err() {
                    lose(&stop, &lost, changed);
                    return;
                }
                let settle = Instant::now() + FILES_SETTLE_MOST;
                while !stop.load(Ordering::Acquire)
                    && Instant::now() < settle
                    && thread_watcher
                        .wait(Some(FILES_SETTLE))
                        .is_ok_and(|fired| !fired.is_empty())
                {}
                if stop.load(Ordering::Acquire) {
                    return;
                }
                // A folder that appeared moves its watch deeper before the park looks again.
                if folders.arm(&thread_watcher).is_err() {
                    lose(&stop, &lost, changed);
                } else {
                    changed();
                }
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
    // SAFETY: socket(2) with constant arguments; a non-negative result is a new descriptor this
    // function owns.
    let raw = unsafe { libc::socket(libc::PF_ROUTE, libc::SOCK_RAW, libc::AF_UNSPEC) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is the open descriptor socket(2) just returned, owned by nothing else.
    let route = Arc::new(unsafe { OwnedFd::from_raw_fd(raw) });
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
        let mut folders = Folders::new(vec![(target.clone(), floor.clone())]);
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
            vec![(target.clone(), floor)],
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
            vec![(floor.join("aterm/secrets"), floor.clone())],
            Arc::clone(&stop),
            Arc::clone(&lost),
            lost_told,
        )
        .unwrap();
        let unreadable = floor.join("aterm");
        std::fs::create_dir(&unreadable).unwrap();
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
        until("the watch never said it was lost", || {
            lost.load(Ordering::Acquire)
        });
        assert!(LOST_TOLD.load(Ordering::SeqCst) > 0, "the park was told");
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
