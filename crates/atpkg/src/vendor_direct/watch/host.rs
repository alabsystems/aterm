// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Lock-file rendezvous selecting which aterm host runs the vendor head watch.

use std::fs::File;
use std::io::{Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{BUSY_RETRY, CADENCE};
use crate::index_probe::open_private;
use crate::lock::{Flock, RecordLock};
use crate::store::Layout;

const HOSTS_FILE: &str = "vendor-head.hosts";
const SEAT_FILE: &str = "vendor-head.session";
const MAX_SEAT_BYTES: u64 = 64;

/// Which kind of aterm host runs a head watch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    /// The window's package thread: it watches whenever it is up, sharing the store's vendor files
    /// with every other window.
    Window,
    /// A terminal session: it watches only from the seat, and only while no window does.
    Session,
}

/// Whether a host may start a round of head checks now.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Admit {
    /// Yes; a session keeps the rendezvous until [`HostClaim::end_round`].
    Yes,
    /// Not now; ask again after this long.
    Later(Duration),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Standing {
    Watching,
    SeatTaken,
    WindowWatches,
    Unavailable,
}

/// One host's claim on the store's rendezvous ([`Host`]).
pub(super) struct HostClaim {
    host: Host,
    prefix: PathBuf,
    hosts: Option<Flock>,
    seat: Option<RecordLock>,
    standing: Option<Standing>,
    #[cfg(test)]
    admit_calls: u64,
}

impl std::fmt::Debug for HostClaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostClaim")
            .field("host", &self.host)
            .field("prefix", &self.prefix)
            .field("hosts", &self.hosts.is_some())
            .field("seat", &self.seat.is_some())
            .field("standing", &self.standing)
            .finish()
    }
}

impl HostClaim {
    pub(super) fn new(host: Host, layout: &Layout) -> Self {
        Self {
            host,
            prefix: layout.prefix.clone(),
            hosts: None,
            seat: None,
            standing: None,
            #[cfg(test)]
            admit_calls: 0,
        }
    }

    /// Returns whether this host may start a round now and any standing-change note.
    pub(super) fn admit(&mut self) -> (Admit, Option<String>) {
        #[cfg(test)]
        {
            self.admit_calls += 1;
        }
        match self.host {
            Host::Window => (self.admit_window(), None),
            Host::Session => {
                let (admit, standing) = self.admit_session();
                let note = (self.standing != Some(standing)).then(|| standing_note(standing));
                self.standing = Some(standing);
                (admit, note)
            }
        }
    }

    fn admit_window(&mut self) -> Admit {
        if self.hosts.is_some() {
            return Admit::Yes;
        }
        let Some(file) = open_private(&self.prefix.join(HOSTS_FILE), true) else {
            return Admit::Yes;
        };
        match Flock::try_lock_shared(file) {
            Ok(held) => {
                self.hosts = Some(held);
                Admit::Yes
            }
            Err(std::fs::TryLockError::WouldBlock) => Admit::Later(BUSY_RETRY),
            Err(std::fs::TryLockError::Error(_)) => Admit::Yes,
        }
    }

    fn admit_session(&mut self) -> (Admit, Standing) {
        if self.hosts.is_some() {
            return (Admit::Yes, Standing::Watching);
        }
        if self.seat.is_none() {
            let Some(file) = open_private(&self.prefix.join(SEAT_FILE), true) else {
                return (Admit::Later(CADENCE), Standing::Unavailable);
            };
            match RecordLock::try_lock(file, MAX_SEAT_BYTES) {
                Ok(mut held) => {
                    let _ = write_pid(&mut held);
                    self.seat = Some(held);
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return (Admit::Later(CADENCE), Standing::SeatTaken);
                }
                Err(std::fs::TryLockError::Error(_)) => {
                    return (Admit::Later(CADENCE), Standing::Unavailable);
                }
            }
        }
        let Some(file) = open_private(&self.prefix.join(HOSTS_FILE), true) else {
            return (Admit::Later(CADENCE), Standing::Unavailable);
        };
        match Flock::try_lock(file) {
            Ok(held) => {
                self.hosts = Some(held);
                (Admit::Yes, Standing::Watching)
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                (Admit::Later(CADENCE), Standing::WindowWatches)
            }
            Err(std::fs::TryLockError::Error(_)) => (Admit::Later(CADENCE), Standing::Unavailable),
        }
    }

    /// Whether this session holds the rendezvous exclusive right now.
    pub(super) fn seated_session(&self) -> bool {
        self.host == Host::Session && self.seat.is_some() && self.hosts.is_some()
    }

    /// Whether this session is seated and waiting only because a window holds the rendezvous.
    pub(super) fn window_watches(&self) -> bool {
        self.host == Host::Session && self.standing == Some(Standing::WindowWatches)
    }

    #[cfg(test)]
    pub(super) fn admit_calls(&self) -> u64 {
        self.admit_calls
    }

    /// A round is over: a session lets the rendezvous go, so a window can take it.
    pub(super) fn end_round(&mut self) {
        if self.host == Host::Session {
            self.hosts = None;
        }
    }
}

fn standing_note(standing: Standing) -> String {
    match standing {
        Standing::Watching => String::from(
            "vendor head watch: this terminal session watches the vendor heads (no aterm \
             window does)",
        ),
        Standing::SeatTaken => String::from(
            "vendor head watch: another terminal session watches the vendor heads \u{2014} \
             this one stands by",
        ),
        Standing::WindowWatches => String::from(
            "vendor head watch: an aterm window watches the vendor heads \u{2014} this \
             terminal session stands by",
        ),
        Standing::Unavailable => String::from(
            "vendor head watch: this terminal session could not open the store's watch \
             rendezvous \u{2014} standing by; the next update pass still finds new releases",
        ),
    }
}

fn write_pid(file: &mut File) -> std::io::Result<()> {
    file.set_len(0)?;
    file.seek(std::io::SeekFrom::Start(0))?;
    writeln!(file, "{}", std::process::id())?;
    file.flush()
}

fn read_pid(file: &mut File) -> Option<u32> {
    let mut text =
        String::with_capacity(usize::try_from(MAX_SEAT_BYTES.saturating_add(1)).unwrap_or(0));
    file.take(MAX_SEAT_BYTES).read_to_string(&mut text).ok()?;
    let s = text.trim();
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// Which host watches this store's vendor heads right now ([`watcher`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Watcher {
    /// An aterm window holds the watch.
    Window,
    /// A terminal session holds the watch — its pid, as it recorded it.
    Session(Option<u32>),
    /// No host watches: no window, and no terminal session in the seat.
    Nobody,
    /// The rendezvous could not be read; why.
    Unknown(String),
}

/// Who watches `layout`'s vendor heads, read off the rendezvous's locks without taking the watch.
///
/// The hosts file is read first, then the seat. On macOS a read holds no lock; elsewhere it takes
/// each lock for an instant, which can turn away a host that admits in that instant.
#[must_use]
pub fn watcher(layout: &Layout) -> Watcher {
    let hosts = match probe(&layout.prefix.join(HOSTS_FILE)) {
        Ok(state) => state,
        Err(why) => return Watcher::Unknown(why),
    };
    if hosts == Probe::Shared {
        return Watcher::Window;
    }
    let seat_path = layout.prefix.join(SEAT_FILE);
    // Only a session takes the seat, and only exclusive, so any hold of it is a seated session.
    match probe(&seat_path) {
        Ok(Probe::Free) if hosts == Probe::Exclusive => Watcher::Session(None),
        Ok(Probe::Free) => Watcher::Nobody,
        Ok(Probe::Shared | Probe::Exclusive) => Watcher::Session(
            open_private(&seat_path, false).and_then(|mut file| read_pid(&mut file)),
        ),
        Err(why) => Watcher::Unknown(why),
    }
}

/// How a rendezvous file's lock is held, as one read of it found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Probe {
    Free,
    Shared,
    Exclusive,
}

fn probe(path: &Path) -> Result<Probe, String> {
    if std::fs::symlink_metadata(path).is_err() {
        return Ok(Probe::Free);
    }
    #[cfg(target_os = "macos")]
    {
        look(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        sample(path)
    }
}

fn unopened(path: &Path) -> String {
    format!("{} could not be opened", path.display())
}

/// macOS keeps `flock` locks on the list `fcntl` reads, so `F_GETLK` names the kind of the lock
/// that would refuse a writer. That is one read at one instant, holding nothing: a holder that lets
/// go and takes again cannot split it, and no host admitting meanwhile is turned away.
#[cfg(target_os = "macos")]
fn look(path: &Path) -> Result<Probe, String> {
    use std::os::fd::AsRawFd as _;
    let Some(file) = open_private(path, false) else {
        return Err(unopened(path));
    };
    #[cfg(test)]
    probing(path, Step::Reads);
    let mut lock = libc::flock {
        l_start: 0,
        l_len: 0,
        l_pid: 0,
        l_type: libc::F_WRLCK,
        l_whence: libc::SEEK_SET as libc::c_short,
    };
    // SAFETY: `file` stays open for the call, and `lock` is a live `flock` that `F_GETLK` reads
    // the asked range from and writes the blocking lock, if any, into.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &raw mut lock) } == -1 {
        let e = std::io::Error::last_os_error();
        return Err(format!("{} could not be read ({e})", path.display()));
    }
    match lock.l_type {
        libc::F_UNLCK => Ok(Probe::Free),
        libc::F_RDLCK => Ok(Probe::Shared),
        libc::F_WRLCK => Ok(Probe::Exclusive),
        kind => Err(format!(
            "{} has a lock of unknown kind {kind}",
            path.display()
        )),
    }
}

/// Elsewhere no read names a `flock` holder's kind, so it is sampled by taking the lock for an
/// instant, each take on a fresh open. A refused exclusive take shows somebody holds the file, and a
/// refused shared take that it is held exclusive. A shared take that succeeds shows no exclusive
/// holder at that moment and nothing about a shared one, so a last exclusive take tells a round
/// that has ended (Free) from a window's hold (Shared). The takes are separate instants: a session
/// that lets its round go before the shared take and takes the next before the last reads as a
/// window, and a host admitting while a take holds the file is turned away and asks again later.
#[cfg(any(test, not(target_os = "macos")))]
fn sample(path: &Path) -> Result<Probe, String> {
    let takes = |shared: bool| {
        let Some(file) = open_private(path, false) else {
            return Err(unopened(path));
        };
        #[cfg(test)]
        probing(path, Step::Reads);
        let taken = if shared {
            Flock::try_lock_shared(file)
        } else {
            Flock::try_lock(file)
        };
        match taken {
            Ok(_held) => {
                #[cfg(test)]
                probing(path, Step::Holds);
                Ok(true)
            }
            Err(std::fs::TryLockError::WouldBlock) => Ok(false),
            Err(std::fs::TryLockError::Error(e)) => {
                Err(format!("{} could not be locked ({e})", path.display()))
            }
        }
    };
    if takes(false)? {
        return Ok(Probe::Free);
    }
    if !takes(true)? {
        return Ok(Probe::Exclusive);
    }
    Ok(if takes(false)? {
        Probe::Free
    } else {
        Probe::Shared
    })
}

/// A point in a read of one rendezvous file where a test acts ([`probing`]).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// The read is about to look at the file's lock.
    Reads,
    /// The read holds the lock it took.
    Holds,
}

/// What a test runs at each [`Step`] of a read, given the file read.
#[cfg(test)]
type ProbingHook = Box<dyn FnMut(&Path, Step)>;

#[cfg(test)]
thread_local! {
    static PROBING: std::cell::RefCell<Option<ProbingHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs this thread's probing hook, if a test set one, at `step` of a read of `path`.
#[cfg(test)]
fn probing(path: &Path, step: Step) {
    if let Some(mut hook) = PROBING.with_borrow_mut(Option::take) {
        hook(path, step);
        PROBING.with_borrow_mut(|slot| *slot = Some(hook));
    }
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
