// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervised control-socket listener with watchdog rebind and cold-`exec` retirement.

#[cfg(unix)]
use std::os::fd::{AsRawFd as _, RawFd};
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::sync::{Condvar, Mutex, PoisonError};
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
use aterm_uds::{CtlListener, CtlStream};

/// Listener `poll` timeout between queue drains.
#[cfg(unix)]
pub(crate) const TICK: Duration = Duration::from_secs(1);

/// Duration a non-empty queue with zero accepts may persist before rebind.
#[cfg(unix)]
pub(crate) const WEDGE_AFTER: Duration = Duration::from_secs(3);

/// The least time between two rebinds, so a cause a rebind cannot cure (a
/// process out of descriptors) produces a warning every few seconds instead of
/// a rebind storm.
#[cfg(unix)]
const REBIND_BACKOFF: Duration = Duration::from_secs(10);

/// The turn length while the queue stands still (see [`AcceptThread::run`]).
#[cfg(unix)]
const STALL_PACE: Duration = Duration::from_millis(100);

/// Connections taken per drain before the thread re-judges the queue: a flood
/// cannot keep the supervisor from ticking.
#[cfg(unix)]
const DRAIN_MAX: usize = 256;

/// The listener's published health — process-wide, like every other `metrics`
/// field, and written only by the listener thread (plus the two admission
/// counters the accept path bumps).
struct ListenerHealth {
    /// Connections `accept` returned, since start.
    accepts: AtomicU64,
    /// The metrics clock (`metrics::now_us`) at the last successful accept, or
    /// at supervisor start before the first one.
    last_accept_us: AtomicU64,
    /// The completed-connection queue length the last tick read (0 when the
    /// platform offers no reading).
    queue_pending: AtomicU64,
    /// Wedged sockets replaced.
    rebinds: AtomicU64,
    /// Wedges detected whose rebind could not complete.
    rebind_failures: AtomicU64,
    /// `accept` errors other than `EWOULDBLOCK`/`EINTR`.
    accept_errors: AtomicU64,
    /// Peers answered `ERR control server busy; retry` at admission: every
    /// request lane taken by work, or the open-connection cap reached with no
    /// parked connection to give way (the parker's turn-aways counted too).
    busy_replies: AtomicU64,
}

static HEALTH: ListenerHealth = ListenerHealth {
    accepts: AtomicU64::new(0),
    last_accept_us: AtomicU64::new(0),
    queue_pending: AtomicU64::new(0),
    rebinds: AtomicU64::new(0),
    rebind_failures: AtomicU64::new(0),
    accept_errors: AtomicU64::new(0),
    busy_replies: AtomicU64::new(0),
};

/// Record one peer rejected with a busy reply.
pub(crate) fn note_busy_reply() {
    HEALTH.busy_replies.fetch_add(1, Ordering::Relaxed);
}

/// Emit listener health metrics.
pub(crate) fn emit_metrics(
    ctx: &crate::metrics::wire::ReadCtx,
    s: &mut dyn crate::metrics::wire::FieldSink,
) {
    let h = snapshot(ctx.now_ns / 1000);
    s.count("control_accepts", h.accepts);
    s.opt_count("control_last_accept_age_ms", h.last_accept_age_ms, "-");
    s.count("control_queue_pending", h.queue_pending);
    s.count("control_rebinds", h.rebinds);
    s.count("control_rebind_failures", h.rebind_failures);
    s.count("control_accept_errors", h.accept_errors);
    s.count("control_busy_replies", h.busy_replies);
}

struct HealthSnapshot {
    accepts: u64,
    last_accept_age_ms: Option<u64>,
    queue_pending: u64,
    rebinds: u64,
    rebind_failures: u64,
    accept_errors: u64,
    busy_replies: u64,
}

/// The published health, with the last accept's age measured at `now_us` (the
/// `metrics::now_us` clock).
fn snapshot(now_us: u64) -> HealthSnapshot {
    let last = HEALTH.last_accept_us.load(Ordering::Relaxed);
    HealthSnapshot {
        accepts: HEALTH.accepts.load(Ordering::Relaxed),
        last_accept_age_ms: (last != 0).then(|| now_us.saturating_sub(last) / 1000),
        queue_pending: HEALTH.queue_pending.load(Ordering::Relaxed),
        rebinds: HEALTH.rebinds.load(Ordering::Relaxed),
        rebind_failures: HEALTH.rebind_failures.load(Ordering::Relaxed),
        accept_errors: HEALTH.accept_errors.load(Ordering::Relaxed),
        busy_replies: HEALTH.busy_replies.load(Ordering::Relaxed),
    }
}

/// Stamp "now" as the last accept (never 0: 0 means "not started").
#[cfg(unix)]
fn stamp_accept() {
    HEALTH
        .last_accept_us
        .store(crate::metrics::now_us().max(1), Ordering::Relaxed);
}

/// One accept thread's published progress, read by the watchdog — never by
/// touching the socket the thread serves.
#[cfg(unix)]
#[derive(Default)]
struct Beat {
    /// Bumped after every turn of the accept loop and every accepted peer: a
    /// value that stands still means the thread is stuck in some call.
    progress: AtomicU64,
    /// Connections this generation's thread accepted.
    accepts: AtomicU64,
    /// The queue length its last turn read; [`NO_READING`] for none.
    pending: AtomicU64,
}

/// A [`Beat::pending`] with no reading.
#[cfg(unix)]
const NO_READING: u64 = u64::MAX;

/// What the watchdog saw on one tick.
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
struct Reading {
    progress: u64,
    accepts: u64,
    pending: Option<u64>,
}

#[cfg(unix)]
impl Beat {
    fn read(&self) -> Reading {
        let pending = self.pending.load(Ordering::Relaxed);
        Reading {
            progress: self.progress.load(Ordering::Acquire),
            accepts: self.accepts.load(Ordering::Relaxed),
            pending: (pending != NO_READING).then_some(pending),
        }
    }
}

/// Why the watchdog judged the listening socket wedged.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wedge {
    /// The accept thread has made no progress at all — it is stuck inside a
    /// call (`accept`, the queue reading, an admission) — for this long.
    Stuck { for_ms: u128 },
    /// The accept thread turns, but its queue stays non-empty and nothing is
    /// accepted from it.
    Stalled { queued: u64, for_ms: u128 },
}

/// How many consecutive watchdog ticks must see no progress before a stuck
/// thread is judged (on top of [`WEDGE_AFTER`]): a process that was suspended
/// (a sleeping laptop, `SIGSTOP`) wakes both threads together, and the accept
/// thread turns within a [`TICK`] of waking — long before this many ticks.
#[cfg(unix)]
const STUCK_TICKS: u32 = 3;

/// Pure watchdog state machine: fed one [`Reading`] per tick, detects wedged listeners.
#[cfg(unix)]
struct Watch {
    last: Reading,
    /// When `last.progress` last changed, and how many ticks have seen it
    /// unchanged since.
    progress_at: Instant,
    unchanged_ticks: u32,
    /// The first tick that saw a non-empty queue with no accept since.
    stalled_since: Option<Instant>,
    last_rebind: Option<Instant>,
    /// How long a beat may stand still, or a queue go unaccepted, before it
    /// is a wedge: [`WEDGE_AFTER`] (a test's own pace, [`Pace`]).
    wedge_after: Duration,
}

#[cfg(unix)]
impl Watch {
    /// The judgement at the production pace, the one the listener runs: the
    /// pure tests below hold it to [`WEDGE_AFTER`]'s three seconds.
    #[cfg(test)]
    fn new(now: Instant) -> Self {
        Self::paced(now, Pace::PRODUCTION.wedge_after)
    }

    const fn paced(now: Instant, wedge_after: Duration) -> Self {
        Self {
            last: Reading {
                progress: 0,
                accepts: 0,
                pending: None,
            },
            progress_at: now,
            unchanged_ticks: 0,
            stalled_since: None,
            last_rebind: None,
            wedge_after,
        }
    }

    fn observe(&mut self, reading: Reading, now: Instant) -> Option<Wedge> {
        if reading.progress == self.last.progress {
            self.unchanged_ticks = self.unchanged_ticks.saturating_add(1);
        } else {
            self.progress_at = now;
            self.unchanged_ticks = 0;
        }
        if reading.accepts != self.last.accepts || reading.pending.is_none_or(|n| n == 0) {
            self.stalled_since = None;
        } else {
            self.stalled_since.get_or_insert(now);
        }
        self.last = reading;
        if self
            .last_rebind
            .is_some_and(|at| now.saturating_duration_since(at) < REBIND_BACKOFF)
        {
            return None;
        }
        let still = now.saturating_duration_since(self.progress_at);
        if self.unchanged_ticks >= STUCK_TICKS && still >= self.wedge_after {
            return Some(Wedge::Stuck {
                for_ms: still.as_millis(),
            });
        }
        let stalled = self
            .stalled_since
            .map(|since| now.saturating_duration_since(since))?;
        (stalled >= self.wedge_after).then(|| Wedge::Stalled {
            queued: reading.pending.unwrap_or(0),
            for_ms: stalled.as_millis(),
        })
    }

    /// A rebind was attempted at `now` (whether or not it completed): back off.
    const fn attempted(&mut self, now: Instant) {
        self.last_rebind = Some(now);
    }

    /// A fresh generation took over at `now`: judge it from scratch.
    fn rebound(&mut self, now: Instant) {
        *self = Self {
            last_rebind: Some(now),
            ..Self::paced(now, self.wedge_after)
        };
    }
}

/// The connection handler every accept thread shares.
#[cfg(unix)]
type Serve = Arc<dyn Fn(CtlStream) + Send + Sync>;

/// The way a supervisor mints a replacement listening socket at its path
/// ([`rebind_socket`] behind the caller's own bookkeeping).
#[cfg(unix)]
type Rebind = Box<dyn FnMut() -> Option<CtlListener> + Send>;

/// Handle for retiring the control listener before cold `exec` and reopening if `exec` fails.
#[derive(Clone, Default)]
pub(crate) struct Endpoint {
    #[cfg(unix)]
    shared: Arc<EndpointShared>,
}

#[cfg(unix)]
#[derive(Default)]
struct EndpointShared {
    endpoint_state: Mutex<EndpointState>,
    /// Rung when [`Endpoint::reopen`] hands the watchdog a listener to serve.
    handed_over: Condvar,
}

#[cfg(unix)]
#[derive(Default)]
struct EndpointState {
    /// The serving generation's listening descriptor. Its accept thread owns
    /// it and closes it only once retired, and a generation is retired only
    /// under this lock ([`Endpoint::install`]), so while it is here it names
    /// that listening socket.
    serving: Option<RawFd>,
    /// Every name a generation's listening socket was bound at — the published
    /// path, a rebind's temporary name: the local address every connection it
    /// accepted carries.
    names: Vec<PathBuf>,
    /// [`Endpoint::retire`] ran, and no [`Endpoint::reopen`] since: nothing
    /// serves and nothing is rebound.
    retired: bool,
    /// A listener [`Endpoint::reopen`] bound, not yet taken by the watchdog.
    handed: Option<CtlListener>,
    /// The supervisor's rebind, shared with [`Endpoint::reopen`].
    mint_listener: Option<Rebind>,
}

/// Sockets closed by [`Endpoint::retire`].
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Retired {
    /// Whether the serving listening socket was closed.
    pub(crate) listener: bool,
    /// Number of accepted connections shut down.
    pub(crate) connections: usize,
}

#[cfg(unix)]
impl Endpoint {
    /// Retire the listening socket onto `/dev/null` and shut down accepted connections.
    pub(crate) fn retire(&self) -> Retired {
        let (listener, names) = {
            let mut endpoint = self
                .shared
                .endpoint_state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            endpoint.retired = true;
            endpoint.handed = None;
            let listener = match endpoint.serving {
                Some(fd) if close_listening(fd) => {
                    endpoint.serving = None;
                    true
                }
                _ => false,
            };
            (listener, endpoint.names.clone())
        };
        Retired {
            listener,
            connections: shutdown_accepted(&names),
        }
    }

    /// Rebind and serve a fresh listening socket after a retired `exec` returns.
    pub(crate) fn reopen(&self) -> bool {
        let mut endpoint = self
            .shared
            .endpoint_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !endpoint.retired {
            return true;
        }
        endpoint.retired = false;
        let Some(mint_listener) = endpoint.mint_listener.as_mut() else {
            return true;
        };
        let Some(fresh) = mint_listener() else {
            return false;
        };
        endpoint.handed = Some(fresh);
        self.shared.handed_over.notify_all();
        true
    }

    fn is_retired(&self) -> bool {
        self.shared
            .endpoint_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retired
    }

    fn adopt(&self, mint_listener: Rebind) {
        self.shared
            .endpoint_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .mint_listener = Some(mint_listener);
    }

    /// Make `listener` the serving generation `generation` — which retires
    /// every earlier one — unless the endpoint is retired: then it is closed
    /// here and `None` comes back.
    fn install(
        &self,
        listener: CtlListener,
        generation: u64,
        current: &AtomicU64,
    ) -> Option<CtlListener> {
        let mut endpoint = self
            .shared
            .endpoint_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if endpoint.retired {
            return None;
        }
        current.store(generation, Ordering::Release);
        endpoint.serving = Some(listener.as_raw_fd());
        if let Some(name) = bound_name(&listener)
            && !endpoint.names.contains(&name)
        {
            endpoint.names.push(name);
        }
        Some(listener)
    }

    /// An installed listener whose accept thread never started is about to
    /// close: stop naming it.
    fn forget(&self, listener: &CtlListener) {
        let mut endpoint = self
            .shared
            .endpoint_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if endpoint.serving == Some(listener.as_raw_fd()) {
            endpoint.serving = None;
        }
    }

    /// Wait out one watchdog tick, cut short by a listener [`Self::reopen`]
    /// hands over, which comes back.
    fn tick(&self, tick: Duration) -> Option<CtlListener> {
        let endpoint = self
            .shared
            .endpoint_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (mut endpoint, _) = self
            .shared
            .handed_over
            .wait_timeout_while(endpoint, tick, |endpoint| endpoint.handed.is_none())
            .unwrap_or_else(PoisonError::into_inner);
        endpoint.handed.take()
    }

    /// The watchdog's replacement for a wedged socket; none while retired.
    fn replacement(&self) -> Option<CtlListener> {
        let mut endpoint = self
            .shared
            .endpoint_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if endpoint.retired {
            return None;
        }
        endpoint
            .mint_listener
            .as_mut()
            .and_then(|mint_listener| mint_listener())
    }

    /// The supervisor stopped (tests): retire every accept thread, and name
    /// no listener any more.
    fn stop(&self, current: &AtomicU64) {
        let mut endpoint = self
            .shared
            .endpoint_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        endpoint.serving = None;
        current.store(u64::MAX, Ordering::Release);
    }
}

/// The name a listening socket was bound at, if it has one.
#[cfg(unix)]
fn bound_name(listener: &CtlListener) -> Option<PathBuf> {
    listener
        .local_addr()
        .ok()?
        .as_pathname()
        .map(Path::to_path_buf)
}

/// Close the listening socket at `fd` by `shutdown` + `dup2`ing `/dev/null` onto
/// it with `FD_CLOEXEC`.
#[cfg(unix)]
fn close_listening(fd: RawFd) -> bool {
    let Ok(null) = std::fs::File::open("/dev/null") else {
        aterm_log::warn!(
            "control endpoint: /dev/null could not be opened; the listener stays open"
        );
        return false;
    };
    // SAFETY: `fd` is an open listening socket owned by the serving generation,
    // for the reason the `dup2` below states, and `shutdown` reads and writes no
    // memory.
    let _ = unsafe { libc::shutdown(fd, libc::SHUT_RDWR) };
    loop {
        // SAFETY: both descriptors are open for the whole call: `null` is
        // borrowed across it, and `fd` is the serving generation's listener,
        // which its accept thread closes only once retired — never while
        // `EndpointState::serving` names it, under the lock the caller holds.
        let duplicated = unsafe { libc::dup2(null.as_raw_fd(), fd) };
        if duplicated == fd {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            aterm_log::warn!("control endpoint: the listener could not be closed: {error}");
            return false;
        }
    }
    // SAFETY: `fd` is open (/dev/null) and `F_SETFD` takes an integer flag.
    let _ = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    true
}

/// Shut down both halves ([`aterm_uds::hangup::hang_up`]) of every connected
/// socket in this process whose local address matches `names`.
#[cfg(unix)]
fn shutdown_accepted(names: &[PathBuf]) -> usize {
    if names.is_empty() {
        return 0;
    }
    let Ok(listing) = std::fs::read_dir("/dev/fd") else {
        return 0;
    };
    let descriptors: Vec<RawFd> = listing
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .collect();
    descriptors
        .into_iter()
        .filter(|&fd| shutdown_if_accepted_at(fd, names))
        .count()
}

/// Inspect and shut down `fd` via a `F_DUPFD_CLOEXEC` duplicate so concurrent
/// closes on other threads cannot race.
#[cfg(unix)]
fn shutdown_if_accepted_at(fd: RawFd, names: &[PathBuf]) -> bool {
    use std::os::fd::FromRawFd as _;
    if !is_socket(fd) {
        return false;
    }
    // SAFETY: `F_DUPFD_CLOEXEC` duplicates `fd` if open or fails with `EBADF`.
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if copy < 0 {
        return false;
    }
    if !is_socket(copy) {
        // Descriptor reused for a non-socket; leak the duplicate above `STDERR_FILENO`.
        return false;
    }
    // SAFETY: `copy` was just created by this call, nothing else owns it, and
    // it names a socket, so closing it releases no lock.
    let copy = CtlStream::from(unsafe { std::os::fd::OwnedFd::from_raw_fd(copy) });
    copy.local_addr()
        .ok()
        .and_then(|address| address.as_pathname().map(Path::to_path_buf))
        .is_some_and(|name| names.contains(&name))
        && aterm_uds::hangup::hang_up(&copy)
}

/// Whether `fd` names a socket; `false` for a number that names nothing.
#[cfg(unix)]
fn is_socket(fd: RawFd) -> bool {
    let mut info = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `info` is a valid `MaybeUninit<libc::stat>` pointer; `fstat`
    // writes exactly one `struct stat` through the pointer and reads nothing
    // else; the descriptor is inspected, never consumed.
    if unsafe { libc::fstat(fd, info.as_mut_ptr()) } != 0 {
        return false;
    }
    // SAFETY: `fstat` returned 0, initializing `info`.
    let info = unsafe { info.assume_init() };
    info.st_mode & libc::S_IFMT == libc::S_IFSOCK
}

/// The watchdog's pace: how long an idle accept thread parks between beats
/// and how long a stand-still is before it is a wedge.
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
struct Pace {
    tick: Duration,
    wedge_after: Duration,
}

#[cfg(unix)]
impl Pace {
    const PRODUCTION: Self = Self {
        tick: TICK,
        wedge_after: WEDGE_AFTER,
    };
}

/// Test seams shared with accept threads, keyed by generation (`u64::MAX` = none).
#[cfg(all(unix, test))]
#[derive(Clone)]
struct Seams {
    /// While it names a thread's generation, that thread's accept path consumes
    /// nothing but keeps turning — the queue stands still.
    withhold: Arc<AtomicU64>,
    /// Generation that has observed `withhold` (`u64::MAX` for none).
    withholding: Arc<AtomicU64>,
    /// Generation whose accept thread should park inside its accept step.
    park: Arc<AtomicU64>,
    /// Generation currently parked inside its accept step (`u64::MAX` for none).
    parked: Arc<AtomicU64>,
    /// The watchdog's pace for this test ([`Pace::PRODUCTION`] unless the test
    /// shortens it).
    pace: Pace,
}

#[cfg(all(unix, test))]
impl Seams {
    fn withheld(&self, generation: u64) -> bool {
        let withheld = self.withhold.load(Ordering::Acquire) == generation;
        if withheld {
            self.withholding.store(generation, Ordering::Release);
        }
        withheld
    }

    fn park_while_named(&self, generation: u64) {
        if self.park.load(Ordering::Acquire) != generation {
            return;
        }
        self.parked.store(generation, Ordering::Release);
        while self.park.load(Ordering::Acquire) == generation {
            std::thread::sleep(Duration::from_millis(10));
        }
        self.parked.store(u64::MAX, Ordering::Release);
    }
}

/// Supervised accept loop pairing an accept thread with a watchdog that rebinds on wedge.
#[cfg(unix)]
pub(crate) struct Supervisor<R: FnMut() -> Option<CtlListener> + Send + 'static> {
    listener: CtlListener,
    rebind: R,
    endpoint: Endpoint,
    #[cfg(test)]
    seams: Option<Seams>,
}

#[cfg(unix)]
impl<R: FnMut() -> Option<CtlListener> + Send + 'static> Supervisor<R> {
    /// Create a supervisor for `listener` (set non-blocking) with replacement factory `rebind`.
    pub(crate) fn new(listener: CtlListener, rebind: R, endpoint: Endpoint) -> Self {
        if let Err(error) = listener.set_nonblocking(true) {
            aterm_log::warn!("control listener could not be made non-blocking: {error}");
        }
        stamp_accept();
        Self {
            listener,
            rebind,
            endpoint,
            #[cfg(test)]
            seams: None,
        }
    }

    /// Run the watchdog loop until stopped, serving each accepted connection in blocking mode via `serve`.
    pub(crate) fn run(
        self,
        serve: impl Fn(CtlStream) + Send + Sync + 'static,
        stop: impl Fn() -> bool,
    ) {
        let Self {
            listener,
            rebind,
            endpoint,
            #[cfg(test)]
            seams,
        } = self;
        endpoint.adopt(Box::new(rebind));
        let serve: Serve = Arc::new(serve);
        let current = Arc::new(AtomicU64::new(0));
        let mut generation = 0;
        let mut beat = match endpoint.install(listener, generation, &current) {
            None => Arc::new(Beat::default()),
            Some(listener) => match AcceptThread::spawn(
                listener,
                generation,
                &current,
                &serve,
                #[cfg(test)]
                seams.clone(),
            ) {
                Ok(beat) => beat,
                Err(accept) => return accept.run(),
            },
        };
        #[cfg(test)]
        let pace = seams.as_ref().map_or(Pace::PRODUCTION, |s| s.pace);
        #[cfg(not(test))]
        let pace = Pace::PRODUCTION;
        let mut watch = Watch::paced(Instant::now(), pace.wedge_after);
        while !stop() {
            let handed = endpoint.tick(pace.tick);
            let now = Instant::now();
            let reopened = handed.is_some();
            let fresh = match handed {
                Some(fresh) => fresh,
                None => {
                    if endpoint.is_retired() {
                        continue;
                    }
                    let Some(wedge) = watch.observe(beat.read(), now) else {
                        continue;
                    };
                    match wedge {
                        Wedge::Stuck { for_ms } => aterm_log::warn!(
                            "control listener wedged: its accept thread has not returned for \
                             {for_ms} ms (generation {generation}); rebinding the control socket"
                        ),
                        Wedge::Stalled { queued, for_ms } => aterm_log::warn!(
                            "control listener wedged: {queued} connection(s) queued and none \
                             accepted for {for_ms} ms (generation {generation}); rebinding the \
                             control socket"
                        ),
                    }
                    watch.attempted(now);
                    let Some(fresh) = endpoint.replacement() else {
                        if !endpoint.is_retired() {
                            HEALTH.rebind_failures.fetch_add(1, Ordering::Relaxed);
                            aterm_log::warn!(
                                "control listener rebind failed; keeping the wedged socket"
                            );
                        }
                        continue;
                    };
                    fresh
                }
            };
            if let Err(error) = fresh.set_nonblocking(true) {
                HEALTH.rebind_failures.fetch_add(1, Ordering::Relaxed);
                aterm_log::warn!("control listener rebind: fresh socket not usable: {error}");
                continue;
            }
            generation += 1;
            let Some(fresh) = endpoint.install(fresh, generation, &current) else {
                continue;
            };
            beat = match AcceptThread::spawn(
                fresh,
                generation,
                &current,
                &serve,
                #[cfg(test)]
                seams.clone(),
            ) {
                Ok(beat) => beat,
                Err(unstarted) => {
                    endpoint.forget(&unstarted.listener);
                    drop(unstarted);
                    HEALTH.rebind_failures.fetch_add(1, Ordering::Relaxed);
                    aterm_log::warn!("control listener rebind: no accept thread could start");
                    Arc::new(Beat::default())
                }
            };
            watch.rebound(now);
            if reopened {
                aterm_log::info!(
                    "control listener reopened (generation {generation}) after a cold exec \
                     that did not happen"
                );
            } else {
                HEALTH.rebinds.fetch_add(1, Ordering::Relaxed);
                aterm_log::info!(
                    "control listener rebound (generation {generation}); queued clients reconnect"
                );
            }
        }
        endpoint.stop(&current);
    }
}

/// One generation's accept loop: the listening socket, what it answers to,
/// and the beat it publishes.
#[cfg(unix)]
struct AcceptThread {
    listener: CtlListener,
    generation: u64,
    current: Arc<AtomicU64>,
    serve: Serve,
    beat: Arc<Beat>,
    #[cfg(test)]
    seams: Option<Seams>,
}

#[cfg(unix)]
impl AcceptThread {
    /// Start this generation's accept thread; its beat on success, or the
    /// unstarted loop (to run inline) when no thread could start.
    fn spawn(
        listener: CtlListener,
        generation: u64,
        current: &Arc<AtomicU64>,
        serve: &Serve,
        #[cfg(test)] seams: Option<Seams>,
    ) -> Result<Arc<Beat>, Self> {
        let beat = Arc::new(Beat::default());
        let accept = Self {
            listener,
            generation,
            current: current.clone(),
            serve: serve.clone(),
            beat: beat.clone(),
            #[cfg(test)]
            seams,
        };
        let slot = Arc::new(std::sync::Mutex::new(Some(accept)));
        let started = slot.clone();
        let spawned = crate::qos::spawn_lane_thread(
            format!("aterm-control-accept-{generation}"),
            crate::qos::Role::Responsive,
            move || {
                let accept = started
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                if let Some(accept) = accept {
                    accept.run();
                }
            },
        );
        match spawned {
            Ok(_) => Ok(beat),
            Err(error) => {
                aterm_log::warn!("control listener: accept thread did not start: {error}");
                slot.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                    .map_or(Ok(beat), Err)
            }
        }
    }

    fn retired(&self) -> bool {
        self.current.load(Ordering::Acquire) != self.generation
    }

    #[cfg(test)]
    fn withheld(&self) -> bool {
        self.seams
            .as_ref()
            .is_some_and(|s| s.withheld(self.generation))
    }

    #[cfg(not(test))]
    #[allow(
        clippy::unused_self,
        reason = "the test build reads the wedge seam from self"
    )]
    const fn withheld(&self) -> bool {
        false
    }

    /// Serve until retired. Each turn: park until readable (or [`TICK`]),
    /// drain, read the queue, beat.
    fn run(self) {
        let serve = |stream: CtlStream| {
            (self.serve)(stream);
            self.beat.progress.fetch_add(1, Ordering::Release);
        };
        let mut stalled = false;
        #[cfg(test)]
        let tick = self.seams.as_ref().map_or(TICK, |s| s.pace.tick);
        #[cfg(not(test))]
        let tick = TICK;
        let mut backoff = None;
        while !self.retired() {
            if stalled {
                std::thread::sleep(STALL_PACE);
            } else {
                wait_readable(&self.listener, tick);
            }
            if self.retired() {
                break;
            }
            #[cfg(test)]
            if let Some(seams) = &self.seams {
                seams.park_while_named(self.generation);
            }
            let accepted = if self.withheld() {
                0
            } else {
                let Some(accepted) = drain(&self.listener, &serve, &mut backoff) else {
                    // Descriptor retired onto /dev/null (`ENOTSOCK`/`EINVAL`).
                    return;
                };
                accepted
            };
            let pending = queue_pending(&self.listener);
            self.beat.accepts.fetch_add(
                u64::try_from(accepted).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
            self.beat
                .pending
                .store(pending.map_or(NO_READING, u64::from), Ordering::Relaxed);
            HEALTH
                .queue_pending
                .store(u64::from(pending.unwrap_or(0)), Ordering::Relaxed);
            self.beat.progress.fetch_add(1, Ordering::Release);
            stalled = accepted == 0 && pending.is_some_and(|n| n > 0);
        }
        if !self.withheld() {
            let _ = drain(&self.listener, &serve, &mut backoff);
        }
    }
}

/// Park until `listener` is readable or `timeout` passes. Errors (EINTR
/// included) just end the wait: the caller drains and re-judges either way.
#[cfg(unix)]
fn wait_readable(listener: &CtlListener, timeout: Duration) {
    let _ = poll_readable(listener.as_raw_fd(), timeout);
}

/// Poll `fd` for readability or hangup/error up to `timeout`.
#[cfg(unix)]
pub(crate) fn poll_readable(fd: RawFd, timeout: Duration) -> std::io::Result<bool> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: `pfd` is a valid single-element `pollfd` array for a descriptor
    // the caller keeps open for the whole call.
    match unsafe { libc::poll(&mut pfd, 1, ms) } {
        n if n > 0 => Ok(pfd.revents != 0),
        0 => Ok(false),
        _ => Err(std::io::Error::last_os_error()),
    }
}

/// Accept until the queue reports empty (or [`DRAIN_MAX`]). Returns how many
/// connections were handed to `serve`, or `None` once the descriptor names no
/// socket at all (a retired endpoint's, [`close_listening`]).
#[cfg(unix)]
fn drain(
    listener: &CtlListener,
    serve: &impl Fn(CtlStream),
    backoff: &mut Option<Duration>,
) -> Option<usize> {
    let mut accepted = 0;
    while accepted < DRAIN_MAX {
        match listener.accept() {
            Ok((stream, _)) => {
                accepted += 1;
                *backoff = None;
                HEALTH.accepts.fetch_add(1, Ordering::Relaxed);
                stamp_accept();
                let _ = stream.set_nonblocking(false);
                serve(stream);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.raw_os_error() == Some(libc::ENOTSOCK) => return None,
            Err(error) => {
                let n = HEALTH.accept_errors.fetch_add(1, Ordering::Relaxed);
                if n.is_power_of_two() || n == 0 {
                    aterm_log::warn!("control listener accept failed ({} so far): {error}", n + 1);
                }
                if let Some(wait) = crate::control::accept_error_backoff(&error, *backoff) {
                    if backoff.is_none() {
                        crate::logging::stderr_line!(
                            "aterm-gui: control socket accept failing ({error}); backing off"
                        );
                    }
                    *backoff = Some(wait);
                    std::thread::sleep(wait);
                }
                break;
            }
        }
    }
    Some(accepted)
}

/// Read the listening socket's completed-connection queue length (`soi_qlen` on macOS, `poll` elsewhere).
#[cfg(target_os = "macos")]
pub(crate) fn queue_pending(listener: &CtlListener) -> Option<u32> {
    use std::os::fd::AsRawFd;
    const PROC_PIDFDSOCKETINFO: libc::c_int = 3;
    const SOCKET_FDINFO_SIZE: usize = 792;
    const SOI_FAMILY: usize = 184;
    const SOI_OPTIONS: usize = 188;
    const SOI_QLEN: usize = 194;
    unsafe extern "C" {
        fn proc_pidfdinfo(
            pid: libc::c_int,
            fd: libc::c_int,
            flavor: libc::c_int,
            buffer: *mut libc::c_void,
            buffersize: libc::c_int,
        ) -> libc::c_int;
    }
    #[repr(C, align(8))]
    struct SocketFdInfo([u8; SOCKET_FDINFO_SIZE]);
    let mut info = SocketFdInfo([0; SOCKET_FDINFO_SIZE]);
    // SAFETY: `info` is a writable, 8-aligned buffer of exactly the size passed,
    // which is the size the kernel requires for this flavor.
    let got = unsafe {
        proc_pidfdinfo(
            libc::getpid(),
            listener.as_raw_fd(),
            PROC_PIDFDSOCKETINFO,
            info.0.as_mut_ptr().cast(),
            SOCKET_FDINFO_SIZE as libc::c_int,
        )
    };
    if usize::try_from(got).ok() != Some(SOCKET_FDINFO_SIZE) {
        return fallback_pending(listener);
    }
    let bytes = &info.0;
    let family = i32::from_ne_bytes([
        bytes[SOI_FAMILY],
        bytes[SOI_FAMILY + 1],
        bytes[SOI_FAMILY + 2],
        bytes[SOI_FAMILY + 3],
    ]);
    let options = i16::from_ne_bytes([bytes[SOI_OPTIONS], bytes[SOI_OPTIONS + 1]]);
    let qlen = i16::from_ne_bytes([bytes[SOI_QLEN], bytes[SOI_QLEN + 1]]);
    if family != libc::AF_UNIX || i32::from(options) & libc::SO_ACCEPTCONN == 0 || qlen < 0 {
        return fallback_pending(listener);
    }
    u32::try_from(qlen).ok()
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn queue_pending(listener: &CtlListener) -> Option<u32> {
    fallback_pending(listener)
}

/// A listener's read readiness as a 0/1 queue reading.
#[cfg(unix)]
fn fallback_pending(listener: &CtlListener) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut fd = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `fd` is a valid single-element `pollfd` array with zero timeout.
    let ready = unsafe { libc::poll(&mut fd, 1, 0) };
    (ready >= 0).then_some(u32::from(ready > 0 && fd.revents & libc::POLLIN != 0))
}

/// Return `(device, inode)` when `path` is a socket file.
#[cfg(unix)]
pub(crate) fn socket_file_id(path: &str) -> Option<(u64, u64)> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let meta = std::fs::symlink_metadata(path).ok()?;
    meta.file_type()
        .is_socket()
        .then(|| (meta.dev(), meta.ino()))
}

/// Bind a replacement socket at a temp path (`0600`) and atomically rename it over `path` if owned.
#[cfg(unix)]
pub(crate) fn rebind_socket(path: &str, expected: &mut Option<(u64, u64)>) -> Option<CtlListener> {
    let target = std::path::Path::new(path);
    let dir = crate::control_auth::dir_of_socket(path);
    let temp = dir.join(format!(".r{}", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    let fresh = match CtlListener::bind(&temp) {
        Ok(listener) => listener,
        Err(error) => {
            aterm_log::warn!("control rebind: bind at {} failed: {error}", temp.display());
            return None;
        }
    };
    let give_up = |why: &str| {
        aterm_log::warn!("control rebind: {why}; leaving {path} as it is");
        let _ = std::fs::remove_file(&temp);
        None
    };
    if let Some(temp) = temp.to_str() {
        crate::control_auth::lock_socket_file(temp);
    }
    let current = socket_file_id(path);
    if expected.is_none() || current != *expected {
        return give_up("the path no longer names the socket this instance published");
    }
    let Some(fresh_id) = temp.to_str().and_then(socket_file_id) else {
        return give_up("the fresh socket is gone from its temporary name");
    };
    if let Err(error) = std::fs::rename(&temp, target) {
        return give_up(&format!("rename over the published path failed: {error}"));
    }
    *expected = Some(fresh_id);
    Some(fresh)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    fn scratch() -> aterm_tempfile::TempDir {
        crate::test_scratch::short_dir("a listener's socket")
    }

    /// The macOS reading is the kernel's own queue: N connects nobody accepted
    /// read as N, and an emptied queue reads 0.
    #[test]
    fn queue_pending_reads_the_kernel_queue() {
        let dir = scratch();
        let path = dir.path().join("q.sock");
        let listener = CtlListener::bind(&path).expect("bind");
        assert_eq!(
            queue_pending(&listener),
            Some(0),
            "an idle listener queues nothing"
        );
        let clients: Vec<_> = (0..3)
            .map(|_| CtlStream::connect(&path).expect("connect"))
            .collect();
        let expected = if cfg!(target_os = "macos") { 3 } else { 1 };
        assert_eq!(queue_pending(&listener), Some(expected));
        for _ in 0..3 {
            drop(listener.accept().expect("accept"));
        }
        assert_eq!(queue_pending(&listener), Some(0), "a drained queue reads 0");
        drop(clients);
    }

    /// Run a supervisor on `path` in a thread at `pace`: every accepted peer is
    /// answered `OK served` and closed.
    fn supervised(
        path: &std::path::Path,
        pace: Pace,
    ) -> (Arc<AtomicBool>, Seams, std::thread::JoinHandle<()>) {
        let listener = CtlListener::bind(path).expect("bind");
        let published = path.to_str().expect("utf8").to_string();
        let mut expected = socket_file_id(&published);
        let rebind_path = published.clone();
        let mut supervisor = Supervisor::new(
            listener,
            move || rebind_socket(&rebind_path, &mut expected),
            Endpoint::default(),
        );
        let seams = Seams {
            withhold: Arc::new(AtomicU64::new(u64::MAX)),
            withholding: Arc::new(AtomicU64::new(u64::MAX)),
            park: Arc::new(AtomicU64::new(u64::MAX)),
            parked: Arc::new(AtomicU64::new(u64::MAX)),
            pace,
        };
        supervisor.seams = Some(seams.clone());
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = std::thread::spawn(move || {
            supervisor.run(
                |mut stream| {
                    let _ = stream.write_all(b"OK served\n");
                },
                move || stopping.load(Ordering::Acquire),
            );
        });
        (stop, seams, thread)
    }

    /// Set read timeout on `stream`, ignoring `EINVAL` when Darwin's peer already closed.
    fn arm(stream: &CtlStream, patience: Duration) -> std::io::Result<()> {
        match stream.set_read_timeout(Some(patience)) {
            Err(error)
                if cfg!(target_vendor = "apple")
                    && error.kind() == std::io::ErrorKind::InvalidInput
                    && error.raw_os_error().is_some() =>
            {
                Ok(())
            }
            other => other,
        }
    }

    /// The one line a fresh client on `path` is answered (`OK served`), or "" when the
    /// connection ends with none.
    fn ask(path: &std::path::Path, patience: Duration) -> std::io::Result<String> {
        let stream = CtlStream::connect(path)?;
        arm(&stream, patience)?;
        read_reply(&stream)
    }

    fn read_reply(stream: &CtlStream) -> std::io::Result<String> {
        use std::io::BufRead as _;
        let mut reply = String::new();
        std::io::BufReader::new(stream).read_line(&mut reply)?;
        Ok(reply)
    }

    /// The one line a new client on `path` is answered once the watchdog has rebound it,
    /// each step waited for with a hang detector ([`aterm_spec::wait::MUST_HAPPEN`]): the
    /// path names a socket other than `before` (the rename is the rebind's last step, and
    /// the fresh socket listens before it), then that client is answered.
    fn served_after_rebind(path: &std::path::Path, before: Option<(u64, u64)>) -> String {
        let published = path.to_str().expect("utf8");
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the watchdog to rebind the published path",
            || (socket_file_id(published) != before).then_some(()),
        );
        ask(path, aterm_spec::wait::MUST_HAPPEN).expect("the fresh socket answers")
    }

    fn assert_rebound_owner_only(path: &std::path::Path, before: Option<(u64, u64)>) {
        assert_ne!(
            socket_file_id(path.to_str().expect("utf8")),
            before,
            "the published path must name the fresh socket"
        );
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
        };
        assert_eq!(mode, 0o600, "the rebound socket is owner-only");
    }

    /// Verify the watchdog rebinds and serves new clients when the accept queue stalls.
    #[test]
    fn a_stalled_listener_is_detected_and_rebound() {
        let dir = scratch();
        let path = dir.path().join("w.sock");
        let pace = Pace {
            tick: Duration::from_millis(50),
            wedge_after: Duration::from_millis(300),
        };
        let (stop, seams, thread) = supervised(&path, pace);
        let rebinds_before = HEALTH.rebinds.load(Ordering::Relaxed);

        assert_eq!(
            ask(&path, aterm_spec::wait::MUST_HAPPEN).expect("healthy ask"),
            "OK served\n",
            "a healthy listener serves"
        );

        seams.withhold.store(0, Ordering::Release);
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the accept thread to reach its withhold check",
            || (seams.withholding.load(Ordering::Acquire) == 0).then_some(()),
        );
        let inode_before = socket_file_id(path.to_str().expect("utf8"));
        let queued = CtlStream::connect(&path).expect("connect into the wedged queue");
        arm(&queued, aterm_spec::wait::MUST_HAPPEN).expect("deadline");

        let mut sink = String::new();
        let released = (&queued).read_to_string(&mut sink);
        assert!(
            released.is_err() || sink.is_empty(),
            "the wedged socket served a peer it was withholding: {sink:?}"
        );

        assert_eq!(served_after_rebind(&path, inode_before), "OK served\n");
        assert_rebound_owner_only(&path, inode_before);
        assert!(HEALTH.rebinds.load(Ordering::Relaxed) > rebinds_before);

        stop.store(true, Ordering::Release);
        thread.join().expect("supervisor thread");
    }

    /// Verify the watchdog rebinds when the accept thread blocks inside its accept step,
    /// and that the retired thread drains its old queue once unblocked.
    #[test]
    fn a_listener_stuck_inside_accept_is_detected_and_rebound() {
        let dir = scratch();
        let path = dir.path().join("p.sock");
        let pace = Pace {
            tick: Duration::from_millis(100),
            wedge_after: Duration::from_secs(1),
        };
        let (stop, seams, thread) = supervised(&path, pace);
        let rebinds_before = HEALTH.rebinds.load(Ordering::Relaxed);
        assert_eq!(
            ask(&path, aterm_spec::wait::MUST_HAPPEN).expect("healthy ask"),
            "OK served\n",
            "a healthy listener serves"
        );

        seams.park.store(0, Ordering::Release);
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "generation 0 to reach its park",
            || (seams.parked.load(Ordering::Acquire) == 0).then_some(()),
        );
        let inode_before = socket_file_id(path.to_str().expect("utf8"));
        let queued = CtlStream::connect(&path).expect("connect into the stuck queue");
        arm(&queued, aterm_spec::wait::MUST_HAPPEN).expect("deadline");

        assert_eq!(served_after_rebind(&path, inode_before), "OK served\n");
        assert_rebound_owner_only(&path, inode_before);
        assert!(HEALTH.rebinds.load(Ordering::Relaxed) > rebinds_before);

        seams.park.store(u64::MAX, Ordering::Release);
        assert_eq!(
            read_reply(&queued).expect("the retired thread's answer"),
            "OK served\n",
            "the retired thread served its queue"
        );

        stop.store(true, Ordering::Release);
        thread.join().expect("supervisor thread");
    }

    fn reading(progress: u64, accepts: u64, pending: Option<u64>) -> Reading {
        Reading {
            progress,
            accepts,
            pending,
        }
    }

    /// The judgement, tick by tick. A beat that moves is healthy however full the queue; a
    /// beat that stands still is stuck only after [`STUCK_TICKS`] ticks and [`WEDGE_AFTER`]
    /// (a process waking from suspension turns within a tick); a turning thread whose
    /// queue stands still is stalled after [`WEDGE_AFTER`]; and a rebind backs off.
    #[test]
    fn the_watch_judges_stuck_stalled_and_healthy() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Pace::PRODUCTION.tick * u32::try_from(s).expect("a few ticks");

        let mut watch = Watch::new(t0);
        for s in 1..=10 {
            assert_eq!(watch.observe(reading(s, s, Some(1)), at(s)), None);
        }

        let mut watch = Watch::new(t0);
        assert_eq!(watch.observe(reading(1, 0, Some(0)), at(1)), None);
        assert_eq!(
            watch.observe(reading(1, 0, Some(0)), at(60)),
            None,
            "one stale tick after a long gap is a suspension, not a wedge"
        );
        assert_eq!(watch.observe(reading(1, 0, Some(0)), at(61)), None);
        assert!(matches!(
            watch.observe(reading(1, 0, Some(0)), at(62)),
            Some(Wedge::Stuck { .. })
        ));

        let mut watch = Watch::new(t0);
        assert_eq!(watch.observe(reading(1, 7, Some(5)), at(1)), None);
        assert_eq!(watch.observe(reading(2, 7, Some(5)), at(2)), None);
        assert_eq!(watch.observe(reading(3, 7, Some(5)), at(3)), None);
        assert_eq!(watch.observe(reading(4, 7, Some(5)), at(4)), None);
        assert_eq!(
            watch.observe(reading(5, 7, Some(5)), at(5)),
            Some(Wedge::Stalled {
                queued: 5,
                for_ms: 3000
            })
        );
        assert_eq!(watch.observe(reading(6, 8, Some(5)), at(6)), None);

        watch.rebound(at(6));
        for s in 7..=15 {
            assert_eq!(watch.observe(reading(s, 0, Some(5)), at(s)), None, "{s}");
        }
        assert!(matches!(
            watch.observe(reading(16, 0, Some(5)), at(16)),
            Some(Wedge::Stalled { .. })
        ));
    }

    /// Verify `shutdown_accepted` delivers EOF to both full-duplex and half-closed clients.
    #[test]
    fn shutting_accepted_connections_down_reaches_a_client_that_half_closed() {
        let dir = scratch();
        let path = dir.path().join("h.sock");
        let listener = CtlListener::bind(&path).expect("bind");
        let open = CtlStream::connect(&path).expect("connect");
        let (open_held, _) = listener.accept().expect("accept");
        let half_closed = CtlStream::connect(&path).expect("connect");
        let (half_closed_held, _) = listener.accept().expect("accept");
        (&half_closed)
            .write_all(b"AUTH ok\nawait idle\n")
            .expect("the request");
        half_closed
            .shutdown(std::net::Shutdown::Write)
            .expect("the client half-closes after its request");
        drop(listener);

        assert_eq!(
            shutdown_accepted(&[path]),
            2,
            "both accepted connections are shut down"
        );
        for (client, what) in [
            (&open, "the open client"),
            (&half_closed, "the half-closed client"),
        ] {
            client.set_nonblocking(true).expect("non-blocking");
            assert_eq!(
                (&*client).read(&mut [0u8; 16]).map_err(|e| e.kind()),
                Ok(0),
                "{what} reads its end-of-file"
            );
        }
        drop((open_held, half_closed_held));
    }

    /// A rebind never takes over an endpoint this process no longer owns: when
    /// the path names some other socket, nothing is renamed and the other
    /// socket keeps its path.
    #[test]
    fn rebind_refuses_a_path_it_does_not_own() {
        let dir = scratch();
        let path = dir.path().join("o.sock");
        let published = path.to_str().expect("utf8").to_string();
        let ours = CtlListener::bind(&path).expect("bind ours");
        let mut expected = socket_file_id(&published);
        std::fs::remove_file(&path).expect("unlink ours");
        let theirs = CtlListener::bind(&path).expect("someone else binds the path");
        let theirs_id = socket_file_id(&published);
        assert!(rebind_socket(&published, &mut expected).is_none());
        assert_eq!(
            socket_file_id(&published),
            theirs_id,
            "their socket keeps the path"
        );
        drop((ours, theirs));
    }
}
