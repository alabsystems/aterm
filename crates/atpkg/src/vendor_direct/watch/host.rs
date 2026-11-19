// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Lock-file rendezvous selecting which aterm host runs the vendor head watch.

use std::fs::File;
use std::io::{Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use super::{BUSY_RETRY, CADENCE, Ring};
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
    /// Not now; a waiter rings the host once the seat or the rendezvous is free, and hands the
    /// claim the lock it took ([`HostClaim::handed`]).
    Waits,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Standing {
    Watching,
    SeatTaken,
    WindowWatches,
    Unavailable,
}

/// What a waiter handed its claim ([`Hand`]).
enum Handed {
    /// Still blocked on the lock.
    Waiting,
    /// The seat, taken with this process's pid written in it.
    Seat(RecordLock),
    /// The rendezvous, taken exclusive.
    Hosts(Flock),
    /// The lock could not be opened or taken; the claim asks again on its own clock.
    Failed,
}

/// A claim's end of its waiter: what the waiter handed over, and the ring that tells the claim's
/// host.
struct Hand {
    handed: Mutex<Handed>,
    ring: Ring,
}

/// One detached thread that blocks on the seat or the rendezvous a standing session was refused,
/// takes it the moment its holder lets go, and hands it to the claim through `hand`, then rings
/// the host. Handing over, rather than letting go and ringing, keeps the seat with the session
/// whose waiter took it, so two standing sessions cannot pass it back and forth between their
/// waiters and their own takes; the seat goes first come. A blocking take cannot be interrupted,
/// so the thread reaches its claim only weakly, through its session's [`Line`] for that lock, and
/// lets go of what it took at once where no claim is there to take it.
struct Waiter {
    /// Why the claim waits: [`Standing::SeatTaken`] or [`Standing::WindowWatches`].
    standing: Standing,
    hand: Arc<Hand>,
}

/// The waiter threads one session runs over its store, one line per lock (the seat and the
/// rendezvous), which its head watch keeps across the claims it makes. A blocking take cannot be
/// interrupted, so a waiter outlives a claim let go while it blocks (Automatic updates turned off)
/// until its lock frees. The session's next claim refused that lock takes the still-blocked waiter
/// on, which then hands over to it, instead of arming a second beside it: a session holds at most
/// one waiter, and one open file, per lock however often it is turned off and on. A waiter that
/// takes its lock with no claim to hand it to lets it go at once.
#[derive(Clone, Default)]
pub(super) struct Waiters {
    seat: Arc<Mutex<Line>>,
    hosts: Arc<Mutex<Line>>,
    /// Each waiter thread armed, with what it waits on, so a test sees it end.
    #[cfg(test)]
    threads: Arc<Mutex<Vec<WaiterThread>>>,
}

/// A waiter thread a test follows, with what it waits on ([`Waiters`]).
#[cfg(test)]
type WaiterThread = (Standing, std::thread::JoinHandle<()>);

impl std::fmt::Debug for Waiters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let blocked =
            |line: &Mutex<Line>| line.lock().unwrap_or_else(PoisonError::into_inner).blocked;
        f.debug_struct("Waiters")
            .field("seat", &blocked(&self.seat))
            .field("hosts", &blocked(&self.hosts))
            .finish_non_exhaustive()
    }
}

impl Waiters {
    /// How many of the waiter threads armed have not ended.
    #[cfg(test)]
    pub(super) fn running(&self) -> usize {
        self.threads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(_, thread)| !thread.is_finished())
            .count()
    }
}

/// One lock's waiter thread, as the session's claims and the thread itself see it.
#[derive(Default)]
struct Line {
    /// A waiter thread blocks on the lock now.
    blocked: bool,
    /// The claim that waiter hands the lock to: the last one that armed it or took it on, dead
    /// once that claim is gone (let go, or its session exited).
    to: Weak<Hand>,
}

/// One host's claim on the store's rendezvous ([`Host`]).
pub(super) struct HostClaim {
    host: Host,
    prefix: PathBuf,
    hosts: Option<Flock>,
    seat: Option<RecordLock>,
    standing: Option<Standing>,
    /// The host's rest's ring, which a waiter rings; a session claim without one arms no waiter.
    ring: Option<Ring>,
    /// The waiter threads of this claim's session ([`Waiters`]).
    waiters: Waiters,
    waiter: Option<Waiter>,
    /// A waiter just failed: the next refusal answers on the clock rather than arming another at
    /// once, so a lock whose blocking take keeps failing costs one try per fallback, not a spin.
    waiter_failed: bool,
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
            .field("waiter", &self.waiter.as_ref().map(|w| w.standing))
            .finish_non_exhaustive()
    }
}

impl HostClaim {
    /// A claim whose session runs no waiter but the ones it arms (a test's: a head watch keeps its
    /// session's waiters across its claims, [`Self::with_waiters`]).
    #[cfg(test)]
    pub(super) fn new(host: Host, layout: &Layout, ring: Option<Ring>) -> Self {
        Self::with_waiters(host, layout, ring, Waiters::default())
    }

    /// A claim of the session whose waiter threads are `waiters`, kept across its claims.
    pub(super) fn with_waiters(
        host: Host,
        layout: &Layout,
        ring: Option<Ring>,
        waiters: Waiters,
    ) -> Self {
        Self {
            host,
            prefix: layout.prefix.clone(),
            hosts: None,
            seat: None,
            standing: None,
            ring,
            waiters,
            waiter: None,
            waiter_failed: false,
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
        if let Some(standing) = self.take_handed() {
            return (Admit::Waits, standing);
        }
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
                    return (self.arm(Standing::SeatTaken), Standing::SeatTaken);
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
                (self.arm(Standing::WindowWatches), Standing::WindowWatches)
            }
            Err(std::fs::TryLockError::Error(_)) => (Admit::Later(CADENCE), Standing::Unavailable),
        }
    }

    /// Take what this claim's waiter left, in one read of its slot: the seat or the rendezvous
    /// it took becomes this claim's, and a waiter that failed is noted so the next refusal
    /// answers on the clock; either way the waiter is over. `Some` of what the claim waits for
    /// while its waiter still waits; `None` once it is over, or with no waiter.
    fn take_handed(&mut self) -> Option<Standing> {
        let waiter = self.waiter.as_ref()?;
        let handed = std::mem::replace(
            &mut *waiter
                .hand
                .handed
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
            Handed::Waiting,
        );
        match handed {
            Handed::Waiting => return Some(waiter.standing),
            Handed::Seat(held) => self.seat = Some(held),
            Handed::Hosts(held) => self.hosts = Some(held),
            Handed::Failed => self.waiter_failed = true,
        }
        self.waiter = None;
        None
    }

    /// Take what this session's waiter handed over (its lock, or its failure) with no other
    /// look: whatever the store's folder or the runner's admission clock says, the hand-over is
    /// this claim's from here. True when a waiter was over and is taken; false with none, or one
    /// that still waits.
    pub(super) fn absorb(&mut self) -> bool {
        self.waiter.is_some() && self.take_handed().is_none()
    }

    /// Arm a waiter for the lock `standing` was refused, answering [`Admit::Waits`]: the waiter
    /// an earlier claim of this session armed on that lock, while it still blocks, else a new one.
    /// Where none can be armed (no ring, a waiter that just failed, a thread refused), the refusal
    /// answers on the clock as before ([`Admit::Later`] of [`CADENCE`]).
    fn arm(&mut self, standing: Standing) -> Admit {
        let just_failed = std::mem::take(&mut self.waiter_failed);
        let Some(ring) = self.ring.clone().filter(|_| !just_failed) else {
            return Admit::Later(CADENCE);
        };
        let seat = standing == Standing::SeatTaken;
        let line = Arc::clone(if seat {
            &self.waiters.seat
        } else {
            &self.waiters.hosts
        });
        let hand = Arc::new(Hand {
            handed: Mutex::new(Handed::Waiting),
            ring,
        });
        // Held while the thread spawns: a waiter that takes its lock at once reads its line only
        // once it names this claim and says it blocks.
        let mut held = line.lock().unwrap_or_else(PoisonError::into_inner);
        held.to = Arc::downgrade(&hand);
        if !held.blocked {
            let path = self.prefix.join(if seat { SEAT_FILE } else { HOSTS_FILE });
            let waiting = Arc::clone(&line);
            let spawned = std::thread::Builder::new()
                .name(String::from(if seat {
                    "atpkg-seat-waiter"
                } else {
                    "atpkg-hosts-waiter"
                }))
                .spawn(move || wait_and_hand(&path, seat, &waiting));
            match spawned {
                Ok(thread) => {
                    held.blocked = true;
                    // The thread runs detached; a test keeps its handle to see it end.
                    #[cfg(test)]
                    self.waiters
                        .threads
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push((standing, thread));
                    #[cfg(not(test))]
                    drop(thread);
                }
                Err(_) => {
                    held.to = Weak::new();
                    return Admit::Later(CADENCE);
                }
            }
        }
        drop(held);
        self.waiter = Some(Waiter { standing, hand });
        Admit::Waits
    }

    /// Whether this session's claim waits on its waiter's ring: nothing it needs admission for
    /// is due until then.
    pub(super) fn waits(&self) -> bool {
        self.waiter.as_ref().is_some_and(|w| {
            matches!(
                *w.hand.handed.lock().unwrap_or_else(PoisonError::into_inner),
                Handed::Waiting
            )
        })
    }

    /// Whether this session's waiter ended (it handed over its lock, or failed), so the next
    /// admission takes what it left at once.
    pub(super) fn handed(&self) -> bool {
        self.waiter.as_ref().is_some_and(|w| {
            !matches!(
                *w.hand.handed.lock().unwrap_or_else(PoisonError::into_inner),
                Handed::Waiting
            )
        })
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

    #[cfg(test)]
    pub(super) const fn holds_seat(&self) -> bool {
        self.seat.is_some()
    }

    /// A round is over: a session lets the rendezvous go, so a window can take it.
    pub(super) fn end_round(&mut self) {
        if self.host == Host::Session {
            self.hosts = None;
        }
    }
}

/// [`Waiter`]'s thread: block on `path`'s seat (`seat`) or rendezvous lock, then hand what it took
/// to the claim its `line` names and ring that claim's host, or let go of it at once where that
/// claim is gone.
fn wait_and_hand(path: &Path, seat: bool, line: &Mutex<Line>) {
    let handed = open_private(path, true).map_or(Handed::Failed, |file| {
        if seat {
            RecordLock::lock(file, MAX_SEAT_BYTES).map_or(Handed::Failed, |mut held| {
                let _ = write_pid(&mut held);
                Handed::Seat(held)
            })
        } else {
            Flock::lock(file).map_or(Handed::Failed, Handed::Hosts)
        }
    });
    let to = {
        let mut line = line.lock().unwrap_or_else(PoisonError::into_inner);
        line.blocked = false;
        std::mem::take(&mut line.to)
    };
    let Some(hand) = to.upgrade() else {
        // No claim takes it: `handed` lets go of its lock here.
        return;
    };
    let ring = hand.ring.clone();
    *hand.handed.lock().unwrap_or_else(PoisonError::into_inner) = handed;
    drop(hand);
    ring.ring();
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
