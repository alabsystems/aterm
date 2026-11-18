// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervised control-socket listener with watchdog rebind and cold-`exec` retirement.
//!
//! At rest almost nothing here wakes. The accept thread parks in one `poll(2)` on its listener and
//! its generation's quit bell, with no timeout; the watchdog parks in one `poll(2)` on the same
//! listener, its doorbell and the supervisor's stop, bounded only by its [`REST_LOOK`]. The
//! watchdog arms (judges the accept thread once a [`TICK`]) only while there is something to
//! judge: the accept thread is in a turn (the listener's readiness woke both, or the thread rings
//! the doorbell as it leaves its park while the watchdog rests), or the kernel counts a connection
//! in the listener's completed-connection queue. That count is read on every wake, the rest
//! look's included, so a parked thread that missed its wake is caught by the watchdog's own poll
//! of the listener, and a queue whose readiness never fires at all by the rest look. It disarms
//! at its next tick once the thread is parked over an empty queue.
//!
//! The listener holds one descriptor in reserve ([`Reserve`]) and gives it up for each accept it
//! makes while it holds it, so a client the process has no descriptor for is told busy rather than
//! hung up (save in the races and failures [`Reserve`] names).

#[cfg(unix)]
use std::os::fd::{AsFd as _, AsRawFd as _, RawFd};
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::sync::{Mutex, OnceLock, PoisonError};
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
use aterm_uds::ready::Doorbell;
#[cfg(unix)]
use aterm_uds::stop::Stop;
#[cfg(unix)]
use aterm_uds::{CtlListener, CtlStream};

/// The watchdog's judging pace while armed, and the accept thread's `poll` timeout only where it
/// has no quit bell to wait on (a process out of descriptors).
#[cfg(unix)]
pub(crate) const TICK: Duration = Duration::from_secs(1);

/// The longest the disarmed watchdog rests before it reads the listener's completed-connection
/// queue by the kernel's own count. A queued connection whose read readiness never fires (a lost
/// wakeup on the socket itself, which the watchdog's poll of the listener shares with the accept
/// thread's) is seen within this, then judged within [`WEDGE_AFTER`]: a bound well inside a
/// client's patience, at one wakeup a minute. On every Unix: macOS counts the queue itself
/// (`soi_qlen`), and elsewhere the reading is a zero-timeout `poll(2)`, which has the kernel
/// recompute the socket's readiness from its queue as it stands, so it sees a connection whose
/// wakeup was lost.
#[cfg(unix)]
const REST_LOOK: Option<Duration> = Some(Duration::from_secs(60));

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

struct ListenerHealth {
    /// Connections `accept` returned, since start, those [`Reserve::accept`] turned away
    /// included.
    accepts: AtomicU64,
    /// The metrics clock (`metrics::now_us`) at the last successful accept, or
    /// at supervisor start before the first one.
    last_accept_us: AtomicU64,
    /// The completed-connection queue length the accept thread's last turn or the watchdog's last
    /// look read (0 when the platform offers no reading). A turn ends with a drain, so at rest
    /// this reads 0; a queue nobody accepts from reads what waits in it.
    queue_pending: AtomicU64,
    /// Wedged sockets replaced.
    rebinds: AtomicU64,
    /// Wedges detected whose rebind could not complete.
    rebind_failures: AtomicU64,
    /// `accept` errors other than `EWOULDBLOCK`/`EINTR`.
    accept_errors: AtomicU64,
    /// Peers answered `ERR control server busy; retry` at admission: every
    /// request lane taken by work, or the open-connection cap reached with no
    /// parked connection to give way (the parker's turn-aways counted too), or
    /// no descriptor left (`EMFILE` or `ENFILE`) for the listener's reserve once
    /// the connection was accepted ([`Reserve::accept`]).
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

/// Request lines taken since start, by who sent each and its verb: `<peer>/<verb>`.
static REQUESTS: crate::metrics::wakes::WakeTable = crate::metrics::wakes::WakeTable::new();

/// The ledger's word for a request line that names no verb of the table.
const UNKNOWN_VERB: &str = "unknown";

/// Count one request line from `peer` ([`peer_label`]). `verb` is the verb table's own name for
/// the line's verb, so what a client types never names a counter.
pub(crate) fn note_request(peer: &str, verb: Option<&'static str>) {
    REQUESTS.note(&format!("{peer}/{}", verb.unwrap_or(UNKNOWN_VERB)));
}

/// The subcommand words the ledger keeps of an `aterm` peer's argv, at most.
const PEER_WORDS: usize = 2;

/// The ledger's name for the peer of one connection. `lane`: the name of a connection that is
/// no local process's own (`fleet`, `bridge`), which wins. `pid`: the kernel's peer of a direct
/// local connection; none is `indirect`, this process is `self`, and one whose name cannot be
/// read is `unreadable`. Any other peer is its process name, and one named as this process is
/// (an `aterm` command) is told apart by the subcommand words its argv starts with (`aterm
/// harness tap`, `aterm ctl`): up to [`PEER_WORDS`] of them, ending at the first word that is
/// not a lowercase letter followed by lowercase letters, digits and `-`.
pub(crate) fn peer_label(
    lane: Option<&'static str>,
    pid: Option<u32>,
    own_pid: u32,
    comm_of: &dyn Fn(u32) -> Option<String>,
    argv_of: &dyn Fn(u32) -> Option<Vec<Vec<u8>>>,
) -> String {
    if let Some(lane) = lane {
        return lane.to_string();
    }
    let Some(pid) = pid else {
        return "indirect".to_string();
    };
    if pid == own_pid {
        return "self".to_string();
    }
    let Some(mut label) = comm_of(pid) else {
        return "unreadable".to_string();
    };
    if comm_of(own_pid).as_deref() == Some(label.as_str()) {
        let argv = argv_of(pid).unwrap_or_default();
        let words = argv
            .iter()
            .skip(1)
            .map_while(|word| {
                std::str::from_utf8(word)
                    .ok()
                    .filter(|w| subcommand_word(w))
            })
            .take(PEER_WORDS);
        for word in words {
            label.push(' ');
            label.push_str(word);
        }
    }
    label
}

/// Whether `word` reads as a subcommand: at most 16 bytes, a lowercase letter, then lowercase
/// letters, digits and `-`.
fn subcommand_word(word: &str) -> bool {
    word.len() <= 16
        && word.starts_with(|c: char| c.is_ascii_lowercase())
        && word
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Emit listener health metrics.
pub(crate) fn emit_metrics(
    ctx: &crate::metrics::wire::ReadCtx,
    s: &mut dyn crate::metrics::wire::FieldSink,
) {
    let h = snapshot(ctx.now_ns / 1000);
    s.count("control_accepts", h.accepts);
    s.count_pairs("control_requests", &REQUESTS.attribution(), "none");
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
    /// Bumped after every turn of the accept loop and every accepted peer, served or
    /// turned away: a value that stands still means the thread is stuck in some call.
    progress: AtomicU64,
    /// Connections this generation's thread accepted.
    accepts: AtomicU64,
    /// The queue length its last turn read; [`NO_READING`] for none.
    pending: AtomicU64,
    /// The thread is in its park: past a turn that left the queue empty, waiting for its
    /// listener or its quit. It turns nothing there, so a beat standing still while parked is
    /// rest, not a wedge, unless a connection is queued. `false` from the start, so a thread that
    /// never ran reads busy and is judged.
    parked: AtomicBool,
    /// The watchdog rests (disarmed) over this beat, so the thread rings its doorbell as it leaves
    /// its park; an armed watchdog wants no ring, so a thread that turns at once (a listener that
    /// reads ready and accepts nothing) never drags it along turn for turn. Set and read against
    /// `parked` in sequentially consistent order ([`Beat::rest_watched`]), so either the thread
    /// sees it set or the watchdog sees the thread out of its park.
    watch_resting: AtomicBool,
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

    fn parked(&self) -> bool {
        self.parked.load(Ordering::SeqCst)
    }

    /// The watchdog's claim to rest over this beat: true when the thread is still parked once the
    /// watchdog has said it rests, so the thread's next leaving rings it; false (and the claim
    /// withdrawn) when the thread has left its park, which the watchdog then judges armed.
    fn rest_watched(&self) -> bool {
        self.watch_resting.store(true, Ordering::SeqCst);
        if self.parked() {
            return true;
        }
        self.watch_resting.store(false, Ordering::SeqCst);
        false
    }

    /// The thread leaves its park: whether the watchdog rests and must be rung.
    fn leave_park(&self) -> bool {
        self.parked.store(false, Ordering::SeqCst);
        self.watch_resting.load(Ordering::SeqCst)
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
/// (a sleeping laptop, `SIGSTOP`) wakes both threads together, and an accept
/// thread in a turn, or parked over a queued connection, turns within a [`TICK`]
/// of waking — long before this many ticks.
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

    /// Start judging again from `reading` after a rest the watchdog did not judge (the thread
    /// parked over an empty queue): what stood still while parked is not a stand-still, so the
    /// beat and the queue are counted from now. The last rebind's backoff stands.
    const fn resume(&mut self, reading: Reading, now: Instant) {
        self.last = reading;
        self.progress_at = now;
        self.unchanged_ticks = 0;
        self.stalled_since = None;
    }

    const fn attempted(&mut self, now: Instant) {
        self.last_rebind = Some(now);
    }

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

/// What every accept thread says to a connection [`Reserve::accept`] turns away. It borrows the
/// connection: the listener closes it after, which frees the descriptor the reserve is taken
/// again from.
#[cfg(unix)]
type TurnAway = Arc<dyn Fn(&CtlStream) + Send + Sync>;

/// What an accept thread does with the connections its listener queues, shared by every
/// generation's thread.
#[cfg(unix)]
struct Admission {
    serve: Serve,
    turn_away: TurnAway,
    reserve: Arc<Reserve>,
}

/// One descriptor the listener holds in reserve (`/dev/null`, opened close-on-exec and kept out
/// of children by [`aterm_uds::kept_out`]), so that a process out of descriptors still answers a
/// client rather than hanging it up. Every accept made while the reserve is held gives it up
/// first, so a queued connection has a descriptor to be accepted into: an `accept(2)` that finds
/// none fails with `EMFILE` (or `ENFILE`), and macOS has then already taken the connection off the
/// queue and closed it, so its client reads EOF. The reserve is then opened again before the
/// connection is served. Where that open succeeds the reserve is held again. Where it fails with
/// `EMFILE` or `ENFILE` (no descriptor left), the connection is handed to the [`TurnAway`] (the
/// control server tells it busy) and closed, and the descriptor it freed is taken as the reserve
/// again. Where it fails otherwise the reserve is lost and the connection is served. One reserve
/// serves every generation's accept thread, and its lock makes their accepts one at a time.
///
/// The reserve's close, the accept and the open after it are made inside one
/// [`aterm_uds::kept_out`], as are a turned-away connection's close and the take after it. On
/// Apple platforms that is one exclusive hold of the spawn lock: no spawn in flight keeps the
/// reserve given up while the accept waits for the lock, and no descriptor another thread makes
/// through `kept_out` comes between (elsewhere no spawn takes the lock and nothing waits). A
/// descriptor another thread makes otherwise (a plain `open(2)`, and off Apple any), or at the
/// system's limit one another process opens, can still take the freed one in between: the accept
/// then fails (macOS drops that connection), or the reserve is lost. A lost reserve is taken again
/// before the next accept; while no descriptor is free to take it, no accept is made
/// ([`Accepted::Deferred`]), so every queued connection waits, with no answer, until one is free,
/// rather than being closed unanswered, and the caller backs off. Where the reserve cannot be
/// opened for another reason than a want of descriptors, the accept is made without it, and a
/// connection that accept finds no descriptor for is dropped as with no reserve.
#[cfg(unix)]
struct Reserve {
    held: Mutex<Held>,
}

#[cfg(unix)]
struct Held {
    /// The reserve, while held.
    file: Option<std::fs::File>,
    /// The reserve was lost, the loss logged, and it has not been taken again since.
    lost: bool,
}

/// What one [`Reserve::accept`] did, short of an accept that failed.
#[cfg(unix)]
#[derive(Debug)]
enum Accepted {
    /// A connection to serve, with the reserve taken again (where it could be opened at all).
    Serve(CtlStream),
    /// A connection accepted with no descriptor left for the reserve: told what the
    /// [`TurnAway`] says, then closed.
    TurnedAway,
    /// No accept was made: the reserve was lost and no descriptor is free to take it again
    /// (the open's error).
    Deferred(std::io::Error),
}

/// Whether `error` says the process (`EMFILE`) or the system (`ENFILE`) has no descriptor free.
#[cfg(unix)]
fn out_of_descriptors(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::EMFILE | libc::ENFILE))
}

#[cfg(unix)]
impl Reserve {
    /// A reserve taken now (its loss logged where it cannot be opened).
    fn take() -> Self {
        let mut held = Held {
            file: None,
            lost: false,
        };
        let _ = held.retake();
        Self {
            held: Mutex::new(held),
        }
    }

    /// Accept one connection from `listener`, made with the reserve given up, and open the
    /// reserve again before the connection is served, the three steps inside one
    /// [`aterm_uds::kept_out`]; one accepted with no descriptor left for the reserve is handed to
    /// `turn_away`, closed, and the reserve taken from the descriptor it freed. Where the reserve
    /// cannot be opened for another reason than a want of descriptors, the connection is served
    /// with the reserve lost, and later accepts go on without it.
    ///
    /// # Errors
    /// The accept's own error (`WouldBlock` once the queue is empty).
    fn accept(
        &self,
        listener: &CtlListener,
        turn_away: &dyn Fn(&CtlStream),
    ) -> std::io::Result<Accepted> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        if held.file.is_none()
            && let Err(error) = held.retake()
            && out_of_descriptors(&error)
        {
            return Ok(Accepted::Deferred(error));
        }
        // Inside one `kept_out` (on Apple one hold of the spawn lock): the reserve given up (the
        // descriptor the connection is accepted into), the accept, and the reserve opened again
        // where it was given up.
        let mut reopened = None;
        let accepted = aterm_uds::kept_out(|| {
            let reserved = held.file.take().is_some();
            let accepted = aterm_uds::accept(
                listener,
                aterm_uds::AcceptWait::Ready,
                aterm_uds::AcceptedMode::Blocking,
            );
            if reserved {
                reopened = Some(aterm_uds::kept_out(open_reserve));
            }
            accepted
        });
        let (stream, _) = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                if let Some(opened) = reopened {
                    let _ = held.hold(opened);
                }
                return Err(error);
            }
        };
        match reopened {
            None => Ok(Accepted::Serve(stream)),
            Some(Ok(file)) => {
                held.file = Some(file);
                Ok(Accepted::Serve(stream))
            }
            Some(Err(error)) if out_of_descriptors(&error) => {
                turn_away(&stream);
                // Its close and the reserve's take inside one `kept_out` too.
                let _ = held.hold(aterm_uds::kept_out(|| {
                    drop(stream);
                    open_reserve()
                }));
                Ok(Accepted::TurnedAway)
            }
            Some(Err(error)) => {
                held.lose(&error);
                Ok(Accepted::Serve(stream))
            }
        }
    }
}

#[cfg(unix)]
impl Held {
    /// Take the reserve again ([`Held::hold`] of an open inside [`aterm_uds::kept_out`]).
    fn retake(&mut self) -> std::io::Result<()> {
        self.hold(aterm_uds::kept_out(open_reserve))
    }

    /// Hold the reserve `opened` gave, logging the take that ends a loss; or, where its open
    /// failed, record the reserve lost ([`Held::lose`]) and return the open's error.
    fn hold(&mut self, opened: std::io::Result<std::fs::File>) -> std::io::Result<()> {
        match opened {
            Ok(file) => {
                self.file = Some(file);
                if std::mem::take(&mut self.lost) {
                    aterm_log::info!("control listener: its descriptor reserve is held again");
                }
                Ok(())
            }
            Err(error) => {
                self.lose(&error);
                Err(error)
            }
        }
    }

    /// Record the reserve lost to `error`, logging it the first time since it was last held.
    fn lose(&mut self, error: &std::io::Error) {
        if !std::mem::replace(&mut self.lost, true) {
            aterm_log::warn!(
                "control listener: its descriptor reserve could not be taken ({error}); until \
                 it is, a client arriving while the process has no descriptor free is not told \
                 busy"
            );
        }
    }
}

/// The reserve's descriptor: `/dev/null`, opened close-on-exec, so no child inherits it past its
/// `exec`. Every caller opens it inside [`aterm_uds::kept_out`], which keeps it out of forked
/// children too.
#[cfg(unix)]
fn open_reserve() -> std::io::Result<std::fs::File> {
    std::fs::File::open("/dev/null")
}

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
    /// The watchdog's doorbell, made by the first [`Endpoint::watch_bell`]: rung when
    /// [`Endpoint::reopen`] hands it a listener to serve, when [`Endpoint::retire`] closes the
    /// serving one, and by an accept thread leaving its park while the watchdog rests. `None`
    /// where no pipe could be made, and the watchdog then looks once a tick instead.
    doorbell: OnceLock<Option<Doorbell>>,
}

#[cfg(unix)]
#[derive(Default)]
struct EndpointState {
    /// The serving generation's listening descriptor. Its accept thread owns
    /// it and closes it only once retired, and a generation is retired only
    /// under this lock ([`Endpoint::install`]), so while it is here it names
    /// that listening socket.
    serving: Option<RawFd>,
    /// The serving generation's quit, which [`Endpoint::retire`] sets so its parked accept
    /// thread wakes and ends.
    serving_quit: Option<Arc<Stop>>,
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
            if let Some(quit) = endpoint.serving_quit.take() {
                quit.set();
            }
            (listener, endpoint.names.clone())
        };
        self.ring();
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
        drop(endpoint);
        self.ring();
        true
    }

    /// The watchdog's doorbell, made on the first call; `None` where no pipe could be made.
    fn watch_bell(&self) -> Option<&Doorbell> {
        self.shared.doorbell.get_or_init(make_doorbell).as_ref()
    }

    /// Wake the watchdog to look again.
    fn ring(&self) {
        if let Some(doorbell) = self.watch_bell() {
            doorbell.ring();
        }
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

    fn install(
        &self,
        listener: CtlListener,
        generation: u64,
        current: &AtomicU64,
        quit: &Arc<Stop>,
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
        endpoint.serving_quit = Some(quit.clone());
        if let Some(name) = bound_name(&listener)
            && !endpoint.names.contains(&name)
        {
            endpoint.names.push(name);
        }
        Some(listener)
    }

    fn forget(&self, listener: &CtlListener) {
        let mut endpoint = self
            .shared
            .endpoint_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if endpoint.serving == Some(listener.as_raw_fd()) {
            endpoint.serving = None;
            endpoint.serving_quit = None;
        }
    }

    /// The listener [`Endpoint::reopen`] handed over, if one waits.
    fn take_handed(&self) -> Option<CtlListener> {
        self.shared
            .endpoint_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .handed
            .take()
    }

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

    fn stop(&self, current: &AtomicU64) {
        let mut endpoint = self
            .shared
            .endpoint_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        endpoint.serving = None;
        current.store(u64::MAX, Ordering::Release);
        if let Some(quit) = endpoint.serving_quit.take() {
            quit.set();
        }
    }
}

/// A fresh doorbell for the watchdog, or `None` (warned) where no pipe could be made.
#[cfg(unix)]
fn make_doorbell() -> Option<Doorbell> {
    Doorbell::new()
        .inspect_err(|error| {
            aterm_log::warn!(
                "control listener: no doorbell ({error}); the watchdog looks once a tick"
            );
        })
        .ok()
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
    // SAFETY: `fd` is an open listening socket owned by the serving generation, for the reason the
    // `dup2` below states, and `shutdown` reads and writes no memory.
    let _ = unsafe { libc::shutdown(fd, libc::SHUT_RDWR) };
    loop {
        // SAFETY: both descriptors are open for the whole call: `null` is borrowed across it, and
        // `fd` is the serving generation's listener, which its accept thread closes only once
        // retired — never while `EndpointState::serving` names it, under the lock the caller holds.
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
        return false;
    }
    // SAFETY: `copy` was just created by this call, nothing else owns it, and it names a socket, so
    // closing it releases no lock.
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
    // SAFETY: `info` is a valid `MaybeUninit<libc::stat>` pointer; `fstat` writes exactly one
    // `struct stat` through the pointer and reads nothing else; the descriptor is inspected, never
    // consumed.
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
    /// The disarmed watchdog's longest rest ([`REST_LOOK`]); `None` rests with no deadline.
    rest_look: Option<Duration>,
}

#[cfg(unix)]
impl Pace {
    const PRODUCTION: Self = Self {
        tick: TICK,
        wedge_after: WEDGE_AFTER,
        rest_look: REST_LOOK,
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
    /// Generation whose park waits on its quit alone, deaf to its listener: the wake a lost
    /// `poll(2)` wakeup would miss.
    deaf: Arc<AtomicU64>,
    /// Generation whose listener's readiness the resting watchdog never hears either: with
    /// `deaf`, a queue whose readiness never fires at all, which only the kernel's count of the
    /// queue (read at the rest look) reveals.
    blind: Arc<AtomicU64>,
    /// Generation whose accept thread is in its park with no timeout (`u64::MAX` for none).
    accept_resting: Arc<AtomicU64>,
    /// The watchdog is disarmed, in a wait bounded by nothing but its rest look.
    watch_resting: Arc<AtomicBool>,
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
    /// Create a supervisor for `listener` with replacement factory `rebind`.
    pub(crate) fn new(listener: CtlListener, rebind: R, endpoint: Endpoint) -> Self {
        stamp_accept();
        Self {
            listener,
            rebind,
            endpoint,
            #[cfg(test)]
            seams: None,
        }
    }

    /// Run the watchdog loop until `stop` is set, serving each accepted connection via `serve`.
    ///
    /// The watchdog judges the accept thread's beat once a [`Pace::tick`] only while it is armed:
    /// the thread is out of its park (in a turn, or pacing a stalled queue), or the kernel counts
    /// a connection queued on the listener. Otherwise it rests in one `poll(2)` on its doorbell,
    /// the stop's bell and the listener, bounded only by [`Pace::rest_look`], so a resting window
    /// spends at most a wakeup a minute here: an arriving connection, an accept thread leaving its
    /// park, a cold exec's retire or reopen and the stop each wake it, and the rest look reads the
    /// queue's count where its readiness never fired. A thread parked over a queued connection
    /// (one that missed its wake) stands still while armed, which [`Watch`] judges a wedge as it
    /// judges one stuck in a call.
    ///
    /// Every accept thread accepts through one [`Reserve`], taken here: a connection accepted
    /// with no descriptor left for the reserve (`EMFILE` or `ENFILE`) is handed to `turn_away`
    /// and closed, never to `serve`.
    pub(crate) fn run(
        self,
        serve: impl Fn(CtlStream) + Send + Sync + 'static,
        turn_away: impl Fn(&CtlStream) + Send + Sync + 'static,
        stop: &Stop,
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
        let turn_away: TurnAway = Arc::new(turn_away);
        let reserve = Arc::new(Reserve::take());
        let current = Arc::new(AtomicU64::new(0));
        let mut generation = 0;
        let mut quit = Arc::new(Stop::new());
        let mut serving: Option<Arc<CtlListener>> = None;
        let mut beat = match endpoint.install(listener, generation, &current, &quit) {
            None => Arc::new(Beat::default()),
            Some(listener) => {
                let listener = Arc::new(listener);
                match AcceptThread::spawn(
                    listener.clone(),
                    generation,
                    &current,
                    Admission {
                        serve: serve.clone(),
                        turn_away: turn_away.clone(),
                        reserve: reserve.clone(),
                    },
                    &quit,
                    &endpoint,
                    #[cfg(test)]
                    seams.clone(),
                ) {
                    Ok(beat) => {
                        serving = Some(listener);
                        beat
                    }
                    Err(accept) => return (*accept).run(),
                }
            }
        };
        #[cfg(test)]
        let pace = seams.as_ref().map_or(Pace::PRODUCTION, |s| s.pace);
        #[cfg(not(test))]
        let pace = Pace::PRODUCTION;
        let mut watch = Watch::paced(Instant::now(), pace.wedge_after);
        // Armed from the start: the first accept thread has not parked yet.
        let mut next_look = Some(Instant::now() + pace.tick);
        loop {
            #[cfg(test)]
            let blind = seams
                .as_ref()
                .is_some_and(|s| s.blind.load(Ordering::Acquire) == generation);
            #[cfg(not(test))]
            let blind = false;
            let disarmed = next_look.is_none();
            let listening = disarmed
                .then_some(serving.as_deref())
                .flatten()
                .filter(|_| !blind && !endpoint.is_retired());
            let until = next_look.or_else(|| pace.rest_look.map(|look| Instant::now() + look));
            #[cfg(test)]
            if let Some(seams) = &seams {
                seams.watch_resting.store(disarmed, Ordering::Release);
            }
            rest(&endpoint, stop, listening, until, pace.tick);
            #[cfg(test)]
            if let Some(seams) = &seams {
                seams.watch_resting.store(false, Ordering::Release);
            }
            if stop.is_set() {
                break;
            }
            let now = Instant::now();
            let handed = endpoint.take_handed();
            let reopened = handed.is_some();
            let fresh = match handed {
                Some(fresh) => fresh,
                None => {
                    if endpoint.is_retired() {
                        next_look = None;
                        continue;
                    }
                    let queued = serving.as_deref().is_some_and(queued);
                    if beat.parked() && !queued && beat.rest_watched() {
                        next_look = None;
                        continue;
                    }
                    beat.watch_resting.store(false, Ordering::SeqCst);
                    let Some(due) = next_look else {
                        watch.resume(beat.read(), now);
                        next_look = Some(now + pace.tick);
                        continue;
                    };
                    if now < due {
                        continue;
                    }
                    next_look = Some(now + pace.tick);
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
            generation += 1;
            let previous = std::mem::replace(&mut quit, Arc::new(Stop::new()));
            let installed = endpoint.install(fresh, generation, &current, &quit);
            // The generation this one replaces ends at its next turn, or now from its park; a
            // retired one was told already.
            previous.set();
            let Some(fresh) = installed else {
                continue;
            };
            let fresh = Arc::new(fresh);
            // Counted before the fresh accept thread starts, which may serve a queued client at
            // once: whoever that client tells of the rebind then reads it in `control_rebinds`.
            if !reopened {
                HEALTH.rebinds.fetch_add(1, Ordering::Relaxed);
            }
            beat = match AcceptThread::spawn(
                fresh.clone(),
                generation,
                &current,
                Admission {
                    serve: serve.clone(),
                    turn_away: turn_away.clone(),
                    reserve: reserve.clone(),
                },
                &quit,
                &endpoint,
                #[cfg(test)]
                seams.clone(),
            ) {
                Ok(beat) => {
                    serving = Some(fresh);
                    beat
                }
                Err(unstarted) => {
                    endpoint.forget(&unstarted.listener);
                    drop(unstarted);
                    serving = None;
                    HEALTH.rebind_failures.fetch_add(1, Ordering::Relaxed);
                    aterm_log::warn!("control listener rebind: no accept thread could start");
                    Arc::new(Beat::default())
                }
            };
            watch.rebound(now);
            next_look = Some(now + pace.tick);
            if reopened {
                aterm_log::info!(
                    "control listener reopened (generation {generation}) after a cold exec \
                     that did not happen"
                );
            } else {
                aterm_log::info!(
                    "control listener rebound (generation {generation}); queued clients reconnect"
                );
            }
        }
        endpoint.stop(&current);
        quit.set();
    }
}

/// The watchdog's wait: until `until` (`None`: no deadline), a ring of the endpoint's doorbell,
/// the stop, or a connection queued on `listening` (passed only while resting, since a queue the
/// armed watchdog is judging would end every wait at once). Without the doorbell or the stop's
/// bell a ring or the stop is heard only by looking, so the wait then ends within a `tick`. The
/// doorbell is cleared after the wait and before the caller reads what a ring announces, so a
/// ring during that read ends the next wait.
#[cfg(unix)]
fn rest(
    endpoint: &Endpoint,
    stop: &Stop,
    listening: Option<&CtlListener>,
    until: Option<Instant>,
    tick: Duration,
) {
    let doorbell = endpoint.watch_bell();
    let bell = stop.bell().ok();
    let until = if doorbell.is_some() && bell.is_some() {
        until
    } else {
        let looks = Instant::now() + tick;
        Some(until.map_or(looks, |until| until.min(looks)))
    };
    let mut fds = Vec::with_capacity(3);
    fds.extend(doorbell.map(Doorbell::fd));
    fds.extend(bell);
    fds.extend(listening.map(|listener| listener.as_fd()));
    if aterm_uds::ready::any(&fds, until).is_err() {
        // An empty set (neither bell could be made, nothing to listen to) or a failing `poll(2)`:
        // a look a tick later, on the stop, never a spin.
        let _ = stop.sleep(tick);
    }
    if let Some(doorbell) = doorbell {
        doorbell.clear();
    }
}

/// Whether a connection is queued on `listener` now, by [`queue_pending`]: on macOS the kernel's
/// own count of its completed-connection queue, which stands whether or not its read readiness
/// fired. The reading is published as `control_queue_pending`, so a queue nobody accepts from
/// reads what waits in it, not the 0 the last turn left.
#[cfg(unix)]
fn queued(listener: &CtlListener) -> bool {
    let pending = queue_pending(listener);
    if let Some(pending) = pending {
        HEALTH
            .queue_pending
            .store(u64::from(pending), Ordering::Relaxed);
    }
    pending.is_some_and(|n| n > 0)
}

/// One generation's accept loop: the listening socket, what it answers to,
/// and the beat it publishes.
#[cfg(unix)]
struct AcceptThread {
    /// Shared with the watchdog, which polls it while it rests: its descriptor closes once both
    /// let go, so the number the watchdog polls names this socket (or, retired for a cold exec,
    /// `/dev/null`) and never a descriptor opened since.
    listener: Arc<CtlListener>,
    generation: u64,
    current: Arc<AtomicU64>,
    admission: Admission,
    beat: Arc<Beat>,
    /// Set when this generation is replaced or retired: it ends the park.
    quit: Arc<Stop>,
    /// Whose doorbell the thread rings as it leaves its park, which arms the watchdog.
    endpoint: Endpoint,
    #[cfg(test)]
    seams: Option<Seams>,
}

#[cfg(unix)]
impl AcceptThread {
    fn spawn(
        listener: Arc<CtlListener>,
        generation: u64,
        current: &Arc<AtomicU64>,
        admission: Admission,
        quit: &Arc<Stop>,
        endpoint: &Endpoint,
        #[cfg(test)] seams: Option<Seams>,
    ) -> Result<Arc<Beat>, Box<Self>> {
        let beat = Arc::new(Beat::default());
        let accept = Self {
            listener,
            generation,
            current: current.clone(),
            admission,
            beat: beat.clone(),
            quit: quit.clone(),
            endpoint: endpoint.clone(),
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
                    .map_or(Ok(beat), |accept| Err(Box::new(accept)))
            }
        }
    }

    fn retired(&self) -> bool {
        self.quit.is_set() || self.current.load(Ordering::Acquire) != self.generation
    }

    /// Wait in one `poll(2)` with no timeout for the listener or the quit, published as parked
    /// for the watchdog. On the way out the thread rings the watchdog's doorbell only while the
    /// watchdog rests ([`Beat::leave_park`]), so it judges the turn that follows; an armed
    /// watchdog sees the thread parked again at its next tick and disarms. Where the quit's bell
    /// cannot be made the wait is bounded by `tick` instead.
    fn park(&self, tick: Duration) {
        self.beat.parked.store(true, Ordering::SeqCst);
        let listener = [self.listener.as_fd()];
        #[cfg(test)]
        let deaf = self
            .seams
            .as_ref()
            .is_some_and(|s| s.deaf.load(Ordering::Acquire) == self.generation);
        #[cfg(not(test))]
        let deaf = false;
        let fds: &[_] = if deaf { &[] } else { &listener };
        #[cfg(test)]
        let resting = self
            .seams
            .as_ref()
            .filter(|_| self.quit.bell().is_ok())
            .inspect(|s| s.accept_resting.store(self.generation, Ordering::Release));
        if self.quit.wait_ready(fds, None).is_err() {
            wait_readable(&self.listener, tick);
        }
        #[cfg(test)]
        if let Some(seams) = resting {
            seams.accept_resting.store(u64::MAX, Ordering::Release);
        }
        if self.beat.leave_park() {
            self.endpoint.ring();
        }
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

    fn run(self) {
        let serve = |stream: CtlStream| {
            (self.admission.serve)(stream);
            self.beat.progress.fetch_add(1, Ordering::Release);
        };
        let turn_away = |stream: &CtlStream| {
            (self.admission.turn_away)(stream);
            self.beat.progress.fetch_add(1, Ordering::Release);
        };
        let reserve = &*self.admission.reserve;
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
                self.park(tick);
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
                let Some(accepted) =
                    drain(&self.listener, reserve, &serve, &turn_away, &mut backoff)
                else {
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
            let _ = drain(&self.listener, reserve, &serve, &turn_away, &mut backoff);
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
    // SAFETY: `pfd` is a valid single-element `pollfd` array for a descriptor the caller keeps open
    // for the whole call.
    match unsafe { libc::poll(&mut pfd, 1, ms) } {
        n if n > 0 => Ok(pfd.revents != 0),
        0 => Ok(false),
        _ => Err(std::io::Error::last_os_error()),
    }
}

/// Accept through `reserve` ([`Reserve::accept`]) until the queue reports empty (or
/// [`DRAIN_MAX`]). Returns how many connections were taken off the queue, those handed to
/// `serve` and those turned away (to `turn_away`) alike, or `None` once the descriptor names no
/// socket at all (a retired endpoint's, [`close_listening`]). A failed accept other than
/// `EWOULDBLOCK`/`EINTR`, and an accept the reserve deferred, end the drain, after a backoff
/// where the error says resources are exhausted ([`crate::control::accept_error_backoff`]).
#[cfg(unix)]
fn drain(
    listener: &CtlListener,
    reserve: &Reserve,
    serve: &impl Fn(CtlStream),
    turn_away: &dyn Fn(&CtlStream),
    backoff: &mut Option<Duration>,
) -> Option<usize> {
    let mut taken = 0;
    while taken < DRAIN_MAX {
        match reserve.accept(listener, turn_away) {
            Ok(Accepted::Serve(stream)) => {
                taken += 1;
                *backoff = None;
                HEALTH.accepts.fetch_add(1, Ordering::Relaxed);
                stamp_accept();
                serve(stream);
            }
            Ok(Accepted::TurnedAway) => {
                taken += 1;
                HEALTH.accepts.fetch_add(1, Ordering::Relaxed);
                stamp_accept();
            }
            Ok(Accepted::Deferred(error)) => {
                back_off(
                    "accept deferred, no descriptor free for its reserve",
                    &error,
                    backoff,
                );
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.raw_os_error() == Some(libc::ENOTSOCK) => return None,
            Err(error) => {
                let n = HEALTH.accept_errors.fetch_add(1, Ordering::Relaxed);
                if n.is_power_of_two() || n == 0 {
                    aterm_log::warn!("control listener accept failed ({} so far): {error}", n + 1);
                }
                back_off("accept failing", &error, backoff);
                break;
            }
        }
    }
    Some(taken)
}

/// Sleep the next step of the backoff `error` calls for, if it calls for one, saying on stderr at
/// the first step of a streak `what` the drain met and `error`.
#[cfg(unix)]
fn back_off(what: &str, error: &std::io::Error, backoff: &mut Option<Duration>) {
    if let Some(wait) = crate::control::accept_error_backoff(error, *backoff) {
        if backoff.is_none() {
            crate::logging::stderr_line!("aterm-gui: control socket {what} ({error}); backing off");
        }
        *backoff = Some(wait);
        std::thread::sleep(wait);
    }
}

/// Read the listening socket's completed-connection queue length.
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
    // SAFETY: `info` is a writable, 8-aligned buffer of exactly the size passed, which is the size
    // the kernel requires for this flavor.
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

/// Bind a replacement socket at a temp path and atomically rename it over `path` if owned.
#[cfg(unix)]
pub(crate) fn rebind_socket(path: &str, expected: &mut Option<(u64, u64)>) -> Option<CtlListener> {
    let target = std::path::Path::new(path);
    let dir = crate::control_auth::dir_of_socket(path);
    let temp = dir.join(format!(".r{}", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    let fresh = match aterm_uds::kept_out(|| CtlListener::bind(&temp)) {
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
#[path = "control_listener_tests.rs"]
mod tests;
