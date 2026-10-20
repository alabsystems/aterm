// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SUPERVISED CONTROL LISTENER — the accept loop that notices when it has
//! stopped accepting, and repairs itself.
//!
//! **The incident (2026-09-25, the owner's live v0.93.0 window).** The control
//! listener thread sat in `accept(2)` while its listening socket held thirteen
//! completed, never-accepted connections (`lsof -Tfqs`: `QLEN=13`). Every new
//! client connected into that dead queue and waited out `aterm ctl`'s 900 s
//! exchange deadline; the window itself kept working, so nothing looked wrong
//! but the one surface agents drive it through. The accept loop was a single
//! blocking `for stream in listener.incoming()` with `Err(_) => continue`:
//! nothing could notice that it had stopped, and nothing could repair it.
//!
//! **What replaces it.** Two threads, so that the one that judges can never be
//! caught in the state it judges.
//!
//! * The ACCEPT THREAD never parks inside a blocking `accept`. The listener is
//!   non-blocking; the thread `poll`s it for readability with a [`TICK`]-long
//!   timeout, then drains `accept` until `EWOULDBLOCK`, so a lost wakeup costs
//!   at most one tick. Each turn it reads the socket's OWN completed-connection
//!   queue length ([`queue_pending`]: `soi_qlen` from
//!   `proc_pidfdinfo(PROC_PIDFDSOCKETINFO)` on macOS — the very number `lsof`
//!   printed — and the listening socket's read readiness elsewhere) and
//!   publishes a BEAT: progress, accepts, the queue reading.
//! * The WATCHDOG (the thread that started the listener) reads only that beat,
//!   once a tick, and makes no call on the socket at all. `O_NONBLOCK` does not
//!   guarantee `accept` returns — XNU skips the wait only when the completed
//!   queue is EMPTY — so the incident's state (a thread inside `accept`, a
//!   non-empty queue) is one a non-blocking accept loop can still enter. The
//!   watchdog judges it WEDGED when the beat stops moving for [`WEDGE_AFTER`]
//!   (the thread is stuck inside a call), or when the queue stays non-empty
//!   that long while nothing is accepted from it. It logs one warning with the
//!   numbers and REBINDS — a fresh socket bound at a temporary name in the
//!   same directory, locked to `0600`, renamed over the published path (so the
//!   path never stops naming a live listener), served by a NEW accept thread.
//!   The old thread and its socket are retired: if its call ever returns, it
//!   serves what the old socket still yields and exits. The token file is
//!   untouched, so every client simply reconnects with the same credential; a
//!   client left in the old queue is told nothing by a stuck socket, and
//!   `aterm ctl`'s connect deadline names that failure within seconds.
//!
//! The health of all of this is published on `metrics` ([`fields_text`]).
//!
//! **Retired before a cold `exec`** ([`Endpoint`], measured 2026-09-29). An
//! `exec` from a multi-threaded process completes on macOS only once every
//! other thread has been scheduled and terminated, and close-on-exec
//! descriptors close only at its end. UTILITY and BACKGROUND threads — mostly
//! libdispatch workqueue threads — must each get CPU to die, and a saturated
//! machine starves them for seconds (up to 2.36 s measured at load 78–176,
//! 11.6 s under a build; unbounded). Throughout, the listening socket and
//! every connection it accepted stayed open with no thread left to serve them:
//! a client connected into a queue nothing would accept and waited out `aterm
//! ctl`'s 10 s connect probe, and a client holding a connection waited for an
//! answer that could not come. The cold update lane now retires the endpoint
//! right before the `exec` — a fresh connect is refused and a held connection
//! reads EOF at once — and reopens it at the same path, with the same token,
//! if the `exec` returns.

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

/// How long the listener thread parks in `poll` before it re-drains and
/// re-judges the queue, when no connection arrives to wake it sooner.
#[cfg(unix)]
pub(crate) const TICK: Duration = Duration::from_secs(1);

/// How long the socket's completed-connection queue may stay non-empty with
/// no successful `accept` before the listening socket is judged wedged.
#[cfg(unix)]
pub(crate) const WEDGE_AFTER: Duration = Duration::from_secs(3);

/// The least time between two rebinds, so a cause a rebind cannot cure (a
/// process out of descriptors) produces a warning every few seconds instead of
/// a rebind storm.
#[cfg(unix)]
const REBIND_BACKOFF: Duration = Duration::from_secs(10);

/// The turn length while the queue stands still (see [`Supervisor::tick`]).
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

/// Count one peer turned away with the busy reply.
pub(crate) fn note_busy_reply() {
    HEALTH.busy_replies.fetch_add(1, Ordering::Relaxed);
}

/// The listener fields of the `metrics` summary line — a leading space, so it
/// splices straight onto the line. `control_last_accept_age_ms` is `-` until
/// the supervisor has started. (Read over the socket, the age is the reading
/// connection's own accept; the fields that tell a wedge are `rebinds`,
/// `rebind_failures` and `queue_pending`, which a caller that reached the
/// instance some other way — the file log, a later healthy read — can trust.)
#[must_use]
pub(crate) fn fields_text() -> String {
    let h = snapshot();
    format!(
        " control_accepts={} control_last_accept_age_ms={} control_queue_pending={} \
         control_rebinds={} control_rebind_failures={} control_accept_errors={} \
         control_busy_replies={}",
        h.accepts,
        h.last_accept_age_ms
            .map_or_else(|| "-".to_string(), |ms| ms.to_string()),
        h.queue_pending,
        h.rebinds,
        h.rebind_failures,
        h.accept_errors,
        h.busy_replies,
    )
}

/// The JSON twin of [`fields_text`]: a leading comma, field-for-field.
#[must_use]
pub(crate) fn fields_json() -> String {
    let h = snapshot();
    format!(
        ",\"control_accepts\":{},\"control_last_accept_age_ms\":{},\
         \"control_queue_pending\":{},\"control_rebinds\":{},\
         \"control_rebind_failures\":{},\"control_accept_errors\":{},\
         \"control_busy_replies\":{}",
        h.accepts,
        h.last_accept_age_ms
            .map_or_else(|| "null".to_string(), |ms| ms.to_string()),
        h.queue_pending,
        h.rebinds,
        h.rebind_failures,
        h.accept_errors,
        h.busy_replies,
    )
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

fn snapshot() -> HealthSnapshot {
    let last = HEALTH.last_accept_us.load(Ordering::Relaxed);
    HealthSnapshot {
        accepts: HEALTH.accepts.load(Ordering::Relaxed),
        last_accept_age_ms: (last != 0)
            .then(|| crate::metrics::now_us().saturating_sub(last) / 1000),
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

/// THE WATCHDOG'S JUDGEMENT, pure: fed one [`Reading`] per tick, it says when
/// the listening socket is wedged. Kept apart from every socket call so it can
/// never be caught in the state it judges.
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

    fn paced(now: Instant, wedge_after: Duration) -> Self {
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
    fn attempted(&mut self, now: Instant) {
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

/// THE CONTROL ENDPOINT'S RETIRE HANDLE: what the cold update lane closes right
/// before its `exec`, and opens again if the `exec` returns (module doc). The
/// App keeps one beside `sock_bound`; the supervisor serving the socket holds
/// the other end. One that no supervisor ever served retires nothing.
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

/// What [`Endpoint::retire`] closed.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Retired {
    /// The serving listening socket.
    pub(crate) listener: bool,
    /// Accepted connections, shut down both ways.
    pub(crate) connections: usize,
}

#[cfg(unix)]
impl Endpoint {
    /// Stop answering, at once, without waiting on any other thread: close
    /// the serving listening socket — every connection still queued on it is
    /// reset and a new connect is refused — and shut down, both ways, every
    /// connection it accepted, wherever it is held, so its client reads EOF
    /// now rather than when the process is finally replaced. The token and
    /// the socket file stay where they are, for [`Self::reopen`] or the
    /// process that replaces this one.
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

    /// Serve again after [`Self::retire`] (a cold `exec` that returned): a
    /// fresh listening socket at the same path, with the same token, bound
    /// here — a client that connects once this returns is queued on it — and
    /// handed to the watchdog, which serves it as it serves a rebind.
    /// `false` when none could be bound; the watchdog then judges the
    /// endpoint dead and rebinds on its own.
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
        // No supervisor yet: it serves its own listener when it starts.
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

/// Close the listening socket `fd` names without freeing the descriptor
/// number: /dev/null is duplicated over it. Its accept thread owns `fd` and
/// closes it when that thread ends, so the number must stay allocated until
/// then — after a plain `close` the process could reuse it under a thread that
/// still polls it, and that thread's own close would then close the reuser's.
/// The socket closes with its last descriptor: every connection still queued
/// on it is reset, and a new connect is refused. The accept thread finds
/// /dev/null under the number (`accept` fails ENOTSOCK) and ends. `dup2` does
/// not wait for that thread. On macOS a `poll` holds no reference to the
/// socket, so it closes at the `dup2`. Linux's `poll` holds a reference until
/// it returns (up to a [`TICK`]), so the socket is shut down first. On Linux
/// that refuses every new connect at once and wakes the `poll`. On macOS it
/// fails with ENOTCONN and changes nothing (measured 2026-09-30), and the
/// `dup2` does the work — unless a child another thread forked has not exec'd
/// yet: it holds a copy of the descriptor, and the socket listens until that
/// exec (26.6 drops `FD_CLOFORK`, so there is no opting out; measured).
#[cfg(unix)]
fn close_listening(fd: RawFd) -> bool {
    let Ok(null) = std::fs::File::open("/dev/null") else {
        aterm_log::warn!(
            "control endpoint: /dev/null could not be opened; the listener stays open"
        );
        return false;
    };
    // SAFETY: `fd` is open, for the reason the `dup2` below states, and
    // `shutdown` reads and writes no memory.
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
    // `dup2` clears close-on-exec on the copy; the `exec` this precedes must
    // not carry it into the successor.
    // SAFETY: `fd` is open (it names /dev/null now), and F_SETFD reads no memory.
    let _ = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    true
}

/// Shut down, both ways, every connected socket in this process whose local
/// address is one of `names`: the connections a listening socket bound at one
/// of them accepted, wherever each is held — a request or wait lane, the
/// parker, a subscription, a relay — so no path that holds one keeps any
/// bookkeeping for this. A retired generation's listening socket carries one
/// of the names too; Darwin refuses its shutdown (ENOTCONN) and it is not
/// counted, while Linux shuts it (harmless: retiring closes every listener)
/// and counts it.
///
/// Each half is shut on its own ([`aterm_uds::hangup::hang_up`]). A client
/// that half-closed after its request (`printf … | nc -U`, parked in a verb)
/// has shut its connection's read half here, and Darwin refuses a both-ways
/// `shutdown` of it — ENOTCONN, as for a listening socket — and shuts NOTHING:
/// such a client read its EOF only when the process was replaced, and was not
/// counted.
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

/// [`shutdown_accepted`] for one descriptor number, which another thread owns
/// and may close — and the process reuse — at any moment. So the socket is
/// read and shut down through a duplicate, which names the socket that was
/// inspected whatever happens to the number. And only a SOCKET is ever
/// duplicated: closing any descriptor of a file drops every POSIX record lock
/// this process holds on it, so a duplicate that turns out to name something
/// else (the number was reused between the two calls) is leaked rather than
/// closed — close-on-exec, it goes with the `exec`.
#[cfg(unix)]
fn shutdown_if_accepted_at(fd: RawFd, names: &[PathBuf]) -> bool {
    use std::os::fd::FromRawFd as _;
    if !is_socket(fd) {
        return false;
    }
    // SAFETY: F_DUPFD_CLOEXEC reads no memory; a number that names no open
    // descriptor fails with EBADF.
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if copy < 0 {
        return false;
    }
    if !is_socket(copy) {
        // The number was reused under us: leak the duplicate (above).
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
    // SAFETY: `fstat` writes exactly one `struct stat` through the pointer and
    // reads nothing else; the descriptor is inspected, never consumed.
    if unsafe { libc::fstat(fd, info.as_mut_ptr()) } != 0 {
        return false;
    }
    // SAFETY: `fstat` returned 0, so it initialized the structure.
    let info = unsafe { info.assume_init() };
    info.st_mode & libc::S_IFMT == libc::S_IFSOCK
}

/// The watchdog's pace: how long an idle accept thread parks between beats
/// and how long a stand-still is before it is a wedge. [`Pace::PRODUCTION`]
/// everywhere but a test that sets its own ([`Seams::pace`]).
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

/// THE TEST SEAMS, shared with every generation's accept thread. Each holds a
/// generation number (or `u64::MAX` for none) that the seam applies to.
#[cfg(all(unix, test))]
#[derive(Clone)]
struct Seams {
    /// While it names a thread's generation, that thread's accept path consumes
    /// nothing but keeps turning — the queue stands still.
    withhold: Arc<AtomicU64>,
    /// The generation that has SEEN `withhold` name it (`u64::MAX` for none).
    /// Like `park`, `withhold` takes effect at a thread's next check, not when it
    /// is stored: a thread already past the check, still draining the queue, can
    /// serve a client that connects before this names it (measured 2026-09-29
    /// under load: "the wedged socket served a peer it was withholding").
    withholding: Arc<AtomicU64>,
    /// While it names a thread's generation, that thread blocks INSIDE its
    /// accept step and does not return — the incident's thread, parked in
    /// `accept(2)` with connections queued.
    park: Arc<AtomicU64>,
    /// The generation now INSIDE its park (`u64::MAX` for none). `park` takes
    /// effect at a thread's next park check, not when it is stored: a thread
    /// already past that check drains the queue as usual, so a test that
    /// connects before this names the thread can be served by it (measured
    /// 2026-09-28 under load: "replaced after 90µs", the socket never rebound).
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

/// The accept loop, supervised. The listening socket is served by an ACCEPT
/// THREAD per generation, and judged by a WATCHDOG — the thread that calls
/// [`Supervisor::run`] — which reads only the accept thread's published
/// [`Beat`], never the socket. So the watchdog cannot be caught in the state it
/// judges: an accept thread stuck inside `accept(2)` (or inside the queue
/// reading, which takes the same socket lock) stops beating, and the watchdog
/// rebinds a fresh socket over the path and starts a new accept thread on it.
/// The stuck thread and its socket are abandoned (retired: if the call ever
/// returns, the thread serves what its socket still yields and exits).
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
    /// Take over `listener` (switched to non-blocking here) with `rebind` as the
    /// way to mint its replacement: a fresh listener already published at the
    /// same path, or `None` when that could not be done. `endpoint` is the
    /// handle the cold update lane retires it by.
    pub(crate) fn new(listener: CtlListener, rebind: R, endpoint: Endpoint) -> Self {
        if let Err(error) = listener.set_nonblocking(true) {
            // Still served: a blocking listener only loses the tick while idle,
            // which is the loop this replaced.
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

    /// Serve until `stop()` (production: never). Each accepted stream is back
    /// in blocking mode (Darwin's `accept` inherits `O_NONBLOCK` from the
    /// listener) before `serve` sees it. The calling thread is the watchdog.
    /// While the endpoint is retired it judges nothing; a listener
    /// [`Endpoint::reopen`] hands over is served as a rebind is.
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
        // The generation being served; a thread whose generation it no longer
        // names is retired.
        let current = Arc::new(AtomicU64::new(0));
        let mut generation = 0;
        let mut beat = match endpoint.install(listener, generation, &current) {
            // Retired before it served: nothing to watch until it reopens.
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
                // No thread to supervise: serve here, unsupervised — exactly
                // the loop this replaced, never nothing.
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
            // Retire the old thread first (inside `install`): if its call ever
            // returns, it serves what the old socket still yields and exits.
            // Retired meanwhile, the fresh socket closes there instead.
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
                    // The fresh socket closes with its thread. A beat that
                    // never moves reads as stuck, so the next judgement
                    // rebinds again rather than leaving the path dead.
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
        // Stopped (tests): retire every accept thread.
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
        // The unstarted loop comes back through this slot if the spawn fails
        // (a closure the builder never ran is dropped, not returned).
        let slot = Arc::new(std::sync::Mutex::new(Some(accept)));
        let started = slot.clone();
        let spawned = crate::qos::spawn_lane_thread(
            format!("aterm-control-accept-{generation}"),
            crate::qos::Role::Responsive,
            move || {
                // Admission sits in front of every lane, and it holds locks the
                // UI thread contends (see the listener thread's own floor).
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
    fn withheld(&self) -> bool {
        false
    }

    /// Serve until retired. Each turn: park until readable (or [`TICK`]),
    /// drain, read the queue, beat.
    ///
    /// While the queue stands still the socket stays READABLE, so `poll` would
    /// return at once and the loop would spin; a stalled turn paces itself
    /// with [`STALL_PACE`] instead.
    fn run(self) {
        let serve = |stream: CtlStream| {
            (self.serve)(stream);
            self.beat.progress.fetch_add(1, Ordering::Release);
        };
        let mut stalled = false;
        // How long an idle turn parks: [`TICK`], or a test's own pace.
        #[cfg(test)]
        let tick = self.seams.as_ref().map_or(TICK, |s| s.pace.tick);
        #[cfg(not(test))]
        let tick = TICK;
        // The accept-error streak's current pause (`control::accept_error_backoff`).
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
                    // The descriptor names no socket any more: the endpoint
                    // closed it under this thread (`Endpoint::retire`), and
                    // there is nothing left to accept.
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
        // Retired: whatever the old socket will still yield is served, not
        // dropped; what it will not yield is reset when it closes, and the
        // client retries.
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

/// Park until `fd` reports an event or `timeout` passes: `Ok(true)` on any
/// event (`POLLIN`, or the `POLLHUP`/`POLLERR`/`POLLNVAL` poll always reports,
/// after which a read returns at once), `Ok(false)` on the timeout, and
/// `poll`'s own error otherwise, `EINTR` included, for the caller to re-judge.
/// The caller keeps `fd` open for the call.
#[cfg(unix)]
pub(crate) fn poll_readable(fd: RawFd, timeout: Duration) -> std::io::Result<bool> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: one initialized pollfd for a descriptor the caller keeps open
    // for the whole call.
    match unsafe { libc::poll(&mut pfd, 1, ms) } {
        n if n > 0 => Ok(pfd.revents != 0),
        0 => Ok(false),
        _ => Err(std::io::Error::last_os_error()),
    }
}

/// Accept until the queue reports empty (or [`DRAIN_MAX`]). Returns how many
/// connections were handed to `serve`, or `None` once the descriptor names no
/// socket at all (a retired endpoint's, [`close_listening`]). `backoff` is the
/// current accept-error streak's pause, cleared by a successful accept.
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
                // ECONNABORTED (a peer gone before it was taken), EMFILE…: the
                // next tick retries, and a queue this cannot drain is exactly
                // what the wedge judgement watches.
                let n = HEALTH.accept_errors.fetch_add(1, Ordering::Relaxed);
                if n.is_power_of_two() || n == 0 {
                    aterm_log::warn!("control listener accept failed ({} so far): {error}", n + 1);
                }
                // Descriptor or buffer exhaustion comes straight back on the
                // next `accept`: pause (10 ms, doubling to 1 s — well inside the
                // watchdog's patience) and say so once per streak.
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

/// The listening socket's completed-connection queue length — the kernel's own
/// count of connections waiting for `accept`.
///
/// macOS: `soi_qlen`, via `proc_pidfdinfo(getpid(), fd, PROC_PIDFDSOCKETINFO)`
/// (the number `lsof -Tq` prints as `QLEN`). `libc` does not bind
/// `struct socket_fdinfo`, so the reply is read from a buffer of the header's
/// exact size at the header's offsets (`<sys/proc_info.h>`, measured with
/// `offsetof` on Darwin 25.6: size 792, `psi.soi_family` 184, `psi.soi_options`
/// 188, `psi.soi_qlen` 194), and refused unless it describes an `AF_UNIX`
/// socket with `SO_ACCEPTCONN` set — so a layout that ever moved reads as "no
/// reading", never as a wrong number. `queue_pending_reads_the_kernel_queue`
/// pins it against a real queue.
///
/// Elsewhere: whether the listening socket is readable, which for a listener
/// means exactly "the accept queue is not empty" (1 or 0).
#[cfg(target_os = "macos")]
pub(crate) fn queue_pending(listener: &CtlListener) -> Option<u32> {
    use std::os::fd::AsRawFd;
    const PROC_PIDFDSOCKETINFO: libc::c_int = 3;
    const SOCKET_FDINFO_SIZE: usize = 792;
    const SOI_FAMILY: usize = 184;
    const SOI_OPTIONS: usize = 188;
    const SOI_QLEN: usize = 194;
    // libproc's entry point (in libSystem). Declared here rather than in the
    // first-party `libc` crate: this is its one caller, and the reply is read
    // by offset below, not through a bound struct.
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
    // SAFETY: one initialized pollfd; a zero timeout never parks.
    let ready = unsafe { libc::poll(&mut fd, 1, 0) };
    (ready >= 0).then_some(u32::from(ready > 0 && fd.revents & libc::POLLIN != 0))
}

/// A socket file's identity: `(device, inode)`, or `None` when `path` is not a
/// socket (missing, replaced by something else).
#[cfg(unix)]
pub(crate) fn socket_file_id(path: &str) -> Option<(u64, u64)> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let meta = std::fs::symlink_metadata(path).ok()?;
    meta.file_type()
        .is_socket()
        .then(|| (meta.dev(), meta.ino()))
}

/// Mint the replacement for a wedged listening socket at `path`: bind a fresh
/// socket at a temporary name in the same directory, lock it to `0600`, and
/// rename it over `path` — but only while `path` still names the socket this
/// process published (`*expected`), so a rebind can never take over an
/// endpoint something else now owns. On success `*expected` moves to the new
/// file, as bound here. The temporary name is at most as long as an
/// `aterm-<pid>.sock` name, so it fits wherever the published path fit.
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
    // The fresh socket's identity, read at its temporary name: the rename
    // keeps it, and once renamed the path could already name another's.
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
        // Short and private: a unix-socket path must fit in 104 bytes.
        aterm_tempfile::TempDir::new_in("/tmp").expect("scratch dir")
    }

    /// The macOS reading is the kernel's own queue: N connects nobody accepted
    /// read as N, and an emptied queue reads 0. (Elsewhere the reading is the
    /// 0/1 readiness, pinned the same way.)
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
    /// answered `OK served` and closed. Returns the stop flag, the seams (both
    /// naming no generation yet) and the thread.
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

    /// Bound `stream`'s reads by `patience`. A supervisor that accepted,
    /// answered and closed the connection before this runs — a client thread
    /// descheduled after its `connect` on a loaded host — leaves a socket
    /// Darwin refuses every option on (`EINVAL`, measured on macOS 26.6 once
    /// the peer's last descriptor closed), though its answer is still queued
    /// and no read on it can block any more. That is how the healthy `ask`
    /// failed once at load ~100 ("healthy ask: EINVAL"). So, as `aterm ctl`
    /// does (`open_accepted`), the read goes ahead unarmed. Only an OS error
    /// counts: std refuses a zero timeout with no errno.
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

    /// The one line a fresh client on `path` is answered — `OK served` — or
    /// "" when the connection ends with none. A LINE, not the stream to its
    /// end: the end is read only once every descriptor of the supervisor's
    /// side has closed, and a sibling test's fork holds a copy of one until
    /// its child execs, past `patience` under load.
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

    /// The one line a new client on `path` is answered once the watchdog has
    /// rebound it — two facts, each waited for with a hang detector
    /// ([`aterm_spec::wait::MUST_HAPPEN`]), never a clock: the path names a
    /// socket other than `before` (the rename is the rebind's last step, and
    /// the fresh socket listens from before it, so a client connecting now is
    /// queued on it), and then that client is answered. No client is sent
    /// into the retiring queue, which a sibling test's fork can hold open
    /// past its close.
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

    /// THE QUEUE STANDS STILL. The accept path stops consuming (the seam
    /// withholds every accept on generation 0) while clients keep connecting
    /// into the kernel queue. The watchdog must see the queue stand still,
    /// rebind, and serve a NEW client on the fresh socket at the SAME path.
    /// The healthy pre-wedge answer is the negative control: the same client
    /// code is served before the wedge. At a test's pace (a 50 ms tick, a
    /// 300 ms wedge).
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

        // Wedge generation 0: connections now sit in the kernel queue — once
        // its thread has seen the wedge (see `Seams::withholding`).
        seams.withhold.store(0, Ordering::Release);
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the accept thread to reach its withhold check",
            || (seams.withholding.load(Ordering::Acquire) == 0).then_some(()),
        );
        let inode_before = socket_file_id(path.to_str().expect("utf8"));
        let queued = CtlStream::connect(&path).expect("connect into the wedged queue");
        arm(&queued, aterm_spec::wait::MUST_HAPPEN).expect("deadline");

        // The queued client is released when the retired socket closes (reset
        // or EOF) — not served, and not left for 900 s.
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

    /// THE INCIDENT'S THREAD: stuck INSIDE its accept step, never returning,
    /// while connections queue. Nothing on the accept thread can notice this —
    /// which is why the judge is a different thread that reads only the beat.
    /// The watchdog must rebind and a NEW client must be served at the same
    /// path while the old thread is still stuck. Released afterwards, the
    /// retired thread serves what its old socket queued (and the client it
    /// holds is not lost) and exits. At a test's pace (a 100 ms tick, a 1 s
    /// wedge — long enough that a starved healthy thread is not judged stuck
    /// before the park).
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

        // Generation 0 parks inside its accept step on its next turn (it wakes
        // at least every TICK), and only once it is IN the park is anything
        // connected: a thread already past its park check would drain the
        // queue and serve it (see `Seams::parked`).
        seams.park.store(0, Ordering::Release);
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "generation 0 to reach its park",
            || (seams.parked.load(Ordering::Acquire) == 0).then_some(()),
        );
        let inode_before = socket_file_id(path.to_str().expect("utf8"));
        let queued = CtlStream::connect(&path).expect("connect into the stuck queue");
        // Armed while nothing can serve it: once the park is released below,
        // the retired thread may answer and close before an arm made then.
        arm(&queued, aterm_spec::wait::MUST_HAPPEN).expect("deadline");

        assert_eq!(served_after_rebind(&path, inode_before), "OK served\n");
        assert_rebound_owner_only(&path, inode_before);
        assert!(HEALTH.rebinds.load(Ordering::Relaxed) > rebinds_before);

        // The stuck call returns at last: the retired thread serves the client
        // its socket held rather than dropping it.
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

    /// The judgement, tick by tick. A beat that moves is healthy however full
    /// the queue; a beat that stands still is stuck only after [`STUCK_TICKS`]
    /// ticks AND [`WEDGE_AFTER`] (a process waking from suspension turns
    /// within a tick); a turning thread whose queue stands still is stalled
    /// after [`WEDGE_AFTER`]; and a rebind backs off.
    #[test]
    fn the_watch_judges_stuck_stalled_and_healthy() {
        let t0 = Instant::now();
        // Tick `s` of the production pace (one a second), so the judgement
        // below is the one the listener runs.
        let at = |s: u64| t0 + Pace::PRODUCTION.tick * u32::try_from(s).expect("a few ticks");

        // Healthy: progress moves every tick, the queue drains.
        let mut watch = Watch::new(t0);
        for s in 1..=10 {
            assert_eq!(watch.observe(reading(s, s, Some(1)), at(s)), None);
        }

        // Stuck: progress frozen. Not before WEDGE_AFTER, even after many
        // ticks; not on one long gap (a suspended process) either.
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

        // Stalled: the thread turns, the queue holds 5, nothing is accepted.
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
        // An accept clears the stall.
        assert_eq!(watch.observe(reading(6, 8, Some(5)), at(6)), None);

        // A rebind backs off: the same stall right after is not re-judged
        // until REBIND_BACKOFF has passed.
        watch.rebound(at(6));
        for s in 7..=15 {
            assert_eq!(watch.observe(reading(s, 0, Some(5)), at(s)), None, "{s}");
        }
        assert!(matches!(
            watch.observe(reading(16, 0, Some(5)), at(16)),
            Some(Wedge::Stalled { .. })
        ));
    }

    /// A RETIRE REACHES A CLIENT THAT HALF-CLOSED after its request (`printf …
    /// | nc -U`, parked in a verb) and counts it, beside one that did not. The
    /// accepted ends stay held, as a lane holds them, so only the shutdown can
    /// end either connection; the listener is gone first, as `retire` closes
    /// it before this runs.
    ///
    /// NEGATIVE CONTROL (run by restoring the both-ways `shutdown`): Darwin
    /// refused it for the half-closed connection, whose read half the client's
    /// half-close had shut (ENOTCONN, as for a listening socket), and shut
    /// nothing — that client read `WouldBlock`, not its end-of-file, and the
    /// count was 1.
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
