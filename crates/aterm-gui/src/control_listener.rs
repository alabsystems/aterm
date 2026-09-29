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

#[cfg(unix)]
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// request lane taken by work, or the open-connection cap reached.
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
}

#[cfg(unix)]
impl Watch {
    fn new(now: Instant) -> Self {
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
        if self.unchanged_ticks >= STUCK_TICKS && still >= WEDGE_AFTER {
            return Some(Wedge::Stuck {
                for_ms: still.as_millis(),
            });
        }
        let stalled = self
            .stalled_since
            .map(|since| now.saturating_duration_since(since))?;
        (stalled >= WEDGE_AFTER).then(|| Wedge::Stalled {
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
            ..Self::new(now)
        };
    }
}

/// The connection handler every accept thread shares.
#[cfg(unix)]
type Serve = Arc<dyn Fn(CtlStream) + Send + Sync>;

/// THE TEST SEAMS, shared with every generation's accept thread. Each holds a
/// generation number (or `u64::MAX` for none) that the seam applies to.
#[cfg(all(unix, test))]
#[derive(Clone)]
struct Seams {
    /// While it names a thread's generation, that thread's accept path consumes
    /// nothing but keeps turning — the queue stands still.
    withhold: Arc<AtomicU64>,
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
}

#[cfg(all(unix, test))]
impl Seams {
    fn withheld(&self, generation: u64) -> bool {
        self.withhold.load(Ordering::Acquire) == generation
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
pub(crate) struct Supervisor<R: FnMut() -> Option<CtlListener>> {
    listener: CtlListener,
    rebind: R,
    #[cfg(test)]
    seams: Option<Seams>,
}

#[cfg(unix)]
impl<R: FnMut() -> Option<CtlListener>> Supervisor<R> {
    /// Take over `listener` (switched to non-blocking here) with `rebind` as the
    /// way to mint its replacement: a fresh listener already published at the
    /// same path, or `None` when that could not be done.
    pub(crate) fn new(listener: CtlListener, rebind: R) -> Self {
        if let Err(error) = listener.set_nonblocking(true) {
            // Still served: a blocking listener only loses the tick while idle,
            // which is the loop this replaced.
            aterm_log::warn!("control listener could not be made non-blocking: {error}");
        }
        stamp_accept();
        Self {
            listener,
            rebind,
            #[cfg(test)]
            seams: None,
        }
    }

    /// Serve until `stop()` (production: never). Each accepted stream is back
    /// in blocking mode (Darwin's `accept` inherits `O_NONBLOCK` from the
    /// listener) before `serve` sees it. The calling thread is the watchdog.
    pub(crate) fn run(
        self,
        serve: impl Fn(CtlStream) + Send + Sync + 'static,
        stop: impl Fn() -> bool,
    ) {
        let Self {
            listener,
            mut rebind,
            #[cfg(test)]
            seams,
        } = self;
        let serve: Serve = Arc::new(serve);
        // The generation being served; a thread whose generation it no longer
        // names is retired.
        let current = Arc::new(AtomicU64::new(0));
        let mut generation = 0;
        let mut beat = match AcceptThread::spawn(
            listener,
            generation,
            &current,
            &serve,
            #[cfg(test)]
            seams.clone(),
        ) {
            Ok(beat) => beat,
            // No thread to supervise: serve here, unsupervised — exactly the
            // loop this replaced, never nothing.
            Err(accept) => return accept.run(),
        };
        let mut watch = Watch::new(Instant::now());
        while !stop() {
            std::thread::sleep(TICK);
            let now = Instant::now();
            let Some(wedge) = watch.observe(beat.read(), now) else {
                continue;
            };
            match wedge {
                Wedge::Stuck { for_ms } => aterm_log::warn!(
                    "control listener wedged: its accept thread has not returned for {for_ms} ms \
                     (generation {generation}); rebinding the control socket"
                ),
                Wedge::Stalled { queued, for_ms } => aterm_log::warn!(
                    "control listener wedged: {queued} connection(s) queued and none accepted \
                     for {for_ms} ms (generation {generation}); rebinding the control socket"
                ),
            }
            watch.attempted(now);
            let Some(fresh) = rebind() else {
                HEALTH.rebind_failures.fetch_add(1, Ordering::Relaxed);
                aterm_log::warn!("control listener rebind failed; keeping the wedged socket");
                continue;
            };
            if let Err(error) = fresh.set_nonblocking(true) {
                HEALTH.rebind_failures.fetch_add(1, Ordering::Relaxed);
                aterm_log::warn!("control listener rebind: fresh socket not usable: {error}");
                continue;
            }
            generation += 1;
            // Retire the old thread first: if its call ever returns, it serves
            // what the old socket still yields and exits.
            current.store(generation, Ordering::Release);
            beat = match AcceptThread::spawn(
                fresh,
                generation,
                &current,
                &serve,
                #[cfg(test)]
                seams.clone(),
            ) {
                Ok(beat) => beat,
                Err(_unstarted) => {
                    // The fresh socket closed with its thread. A beat that
                    // never moves reads as stuck, so the next judgement
                    // rebinds again rather than leaving the path dead.
                    HEALTH.rebind_failures.fetch_add(1, Ordering::Relaxed);
                    aterm_log::warn!("control listener rebind: no accept thread could start");
                    Arc::new(Beat::default())
                }
            };
            watch.rebound(now);
            HEALTH.rebinds.fetch_add(1, Ordering::Relaxed);
            aterm_log::info!(
                "control listener rebound (generation {generation}); queued clients reconnect"
            );
        }
        // Stopped (tests): retire every accept thread.
        current.store(u64::MAX, Ordering::Release);
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
        let spawned = std::thread::Builder::new()
            .name(format!("aterm-control-accept-{generation}"))
            .spawn(move || {
                // Admission sits in front of every lane, and it holds locks the
                // UI thread contends (see the listener thread's own floor).
                crate::qos::set_self(crate::qos::Role::Responsive);
                let accept = started
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                if let Some(accept) = accept {
                    accept.run();
                }
            });
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
        // The accept-error streak's current pause (`control::accept_error_backoff`).
        let mut backoff = None;
        while !self.retired() {
            if stalled {
                std::thread::sleep(STALL_PACE);
            } else {
                wait_readable(&self.listener, TICK);
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
                drain(&self.listener, &serve, &mut backoff)
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
            drain(&self.listener, &serve, &mut backoff);
        }
    }
}

/// Park until `listener` is readable or `timeout` passes. Errors (EINTR
/// included) just end the wait: the caller drains and re-judges either way.
#[cfg(unix)]
fn wait_readable(listener: &CtlListener, timeout: Duration) {
    use std::os::fd::AsRawFd;
    let mut fd = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: one initialized pollfd for a descriptor `listener` keeps open for
    // the whole call.
    let _ = unsafe { libc::poll(&mut fd, 1, ms) };
}

/// Accept until the queue reports empty (or [`DRAIN_MAX`]). Returns how many
/// connections were handed to `serve`. `backoff` is the current accept-error
/// streak's pause, cleared by a successful accept.
#[cfg(unix)]
fn drain(
    listener: &CtlListener,
    serve: &impl Fn(CtlStream),
    backoff: &mut Option<Duration>,
) -> usize {
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
    accepted
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
/// file. The temporary name is at most as long as an `aterm-<pid>.sock` name,
/// so it fits wherever the published path fit.
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
    if let Err(error) = std::fs::rename(&temp, target) {
        return give_up(&format!("rename over the published path failed: {error}"));
    }
    *expected = socket_file_id(path);
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

    /// Run a supervisor on `path` in a thread: every accepted peer is answered
    /// `OK served` and closed. Returns the stop flag, the seams (both naming no
    /// generation yet) and the thread.
    fn supervised(path: &std::path::Path) -> (Arc<AtomicBool>, Seams, std::thread::JoinHandle<()>) {
        let listener = CtlListener::bind(path).expect("bind");
        let published = path.to_str().expect("utf8").to_string();
        let mut expected = socket_file_id(&published);
        let rebind_path = published.clone();
        let mut supervisor =
            Supervisor::new(listener, move || rebind_socket(&rebind_path, &mut expected));
        let seams = Seams {
            withhold: Arc::new(AtomicU64::new(u64::MAX)),
            park: Arc::new(AtomicU64::new(u64::MAX)),
            parked: Arc::new(AtomicU64::new(u64::MAX)),
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

    fn ask(path: &std::path::Path, patience: Duration) -> std::io::Result<String> {
        let mut stream = CtlStream::connect(path)?;
        stream.set_read_timeout(Some(patience))?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply)?;
        Ok(reply)
    }

    /// A new client on `path` is served within `within` (retrying through the
    /// moment of the swap).
    fn served_within(path: &std::path::Path, within: Duration) -> String {
        let deadline = Instant::now() + within;
        loop {
            match ask(path, Duration::from_secs(2)) {
                Ok(reply) if !reply.is_empty() => return reply,
                _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                other => panic!("no client was served after the rebind: {other:?}"),
            }
        }
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
    /// rebind within a few seconds, and serve a NEW client on the fresh socket
    /// at the SAME path. The healthy pre-wedge answer is the negative control:
    /// the same client code is served before the wedge.
    #[test]
    fn a_stalled_listener_is_detected_and_rebound() {
        let dir = scratch();
        let path = dir.path().join("w.sock");
        let (stop, seams, thread) = supervised(&path);
        let rebinds_before = HEALTH.rebinds.load(Ordering::Relaxed);

        assert_eq!(
            ask(&path, Duration::from_secs(5)).expect("healthy ask"),
            "OK served\n",
            "a healthy listener serves"
        );

        // Wedge generation 0: connections now sit in the kernel queue.
        seams.withhold.store(0, Ordering::Release);
        let inode_before = socket_file_id(path.to_str().expect("utf8"));
        let wedged_at = Instant::now();
        let queued = CtlStream::connect(&path).expect("connect into the wedged queue");
        queued
            .set_read_timeout(Some(Duration::from_secs(15)))
            .expect("deadline");

        // The queued client is released when the retired socket closes (reset
        // or EOF) — not served, and not left for 900 s.
        let mut sink = String::new();
        let released = (&queued).read_to_string(&mut sink);
        let waited = wedged_at.elapsed();
        assert!(
            released.is_err() || sink.is_empty(),
            "the wedged socket served a peer it was withholding: {sink:?}"
        );
        assert!(
            waited < WEDGE_AFTER + TICK * 4,
            "the wedge took {waited:?} to be repaired"
        );

        assert_eq!(served_within(&path, Duration::from_secs(10)), "OK served\n");
        assert_rebound_owner_only(&path, inode_before);
        assert!(HEALTH.rebinds.load(Ordering::Relaxed) > rebinds_before);

        stop.store(true, Ordering::Release);
        thread.join().expect("supervisor thread");
    }

    /// THE INCIDENT'S THREAD: stuck INSIDE its accept step, never returning,
    /// while connections queue. Nothing on the accept thread can notice this —
    /// which is why the judge is a different thread that reads only the beat.
    /// The watchdog must rebind within a few seconds and a NEW client must be
    /// served at the same path while the old thread is still stuck. Released
    /// afterwards, the retired thread serves what its old socket queued (and
    /// the client it holds is not lost) and exits.
    #[test]
    fn a_listener_stuck_inside_accept_is_detected_and_rebound() {
        let dir = scratch();
        let path = dir.path().join("p.sock");
        let (stop, seams, thread) = supervised(&path);
        let rebinds_before = HEALTH.rebinds.load(Ordering::Relaxed);
        assert_eq!(
            ask(&path, Duration::from_secs(5)).expect("healthy ask"),
            "OK served\n",
            "a healthy listener serves"
        );

        // Generation 0 parks inside its accept step on its next turn (it wakes
        // at least every TICK), and only once it is IN the park is anything
        // connected: a thread already past its park check would drain the
        // queue and serve it (see `Seams::parked`).
        seams.park.store(0, Ordering::Release);
        let parked_by = Instant::now() + Duration::from_secs(10);
        while seams.parked.load(Ordering::Acquire) != 0 {
            assert!(
                Instant::now() < parked_by,
                "generation 0 never reached its park"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let inode_before = socket_file_id(path.to_str().expect("utf8"));
        let wedged_at = Instant::now();
        let queued = CtlStream::connect(&path).expect("connect into the stuck queue");

        assert_eq!(served_within(&path, Duration::from_secs(12)), "OK served\n");
        let repaired = wedged_at.elapsed();
        eprintln!("a stuck accept thread was replaced after {repaired:?}");
        assert!(
            repaired < WEDGE_AFTER + TICK * 5,
            "the stuck listener took {repaired:?} to be replaced"
        );
        assert_rebound_owner_only(&path, inode_before);
        assert!(HEALTH.rebinds.load(Ordering::Relaxed) > rebinds_before);

        // The stuck call returns at last: the retired thread serves the client
        // its socket held rather than dropping it.
        seams.park.store(u64::MAX, Ordering::Release);
        queued
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("deadline");
        let mut reply = String::new();
        let _ = (&queued).read_to_string(&mut reply);
        assert_eq!(reply, "OK served\n", "the retired thread served its queue");

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
        let at = |s: u64| t0 + Duration::from_secs(s);

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
