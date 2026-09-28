// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CONTROL SOCKET'S LANES: a REQUEST holds a thread, a connection does not.
//!
//! **The defect this shape ends** (the live end-to-end run of 2026-09-26). The
//! socket used to serve each accepted connection on one of eight fixed lanes for
//! as long as the connection stayed open — "accepted connections may remain
//! persistent indefinitely" — and every supervised agent tab holds one (its
//! loop's persistent `RelayCtl`, parked most of the time in a 20 s `await`). Six
//! supervised tabs plus two held connections refused every new `aterm ctl` for a
//! minute; eight supervised tabs would have refused every other client outright.
//!
//! **Where a connection is, at any moment — exactly one of:**
//!
//! * on a REQUEST lane (`CONTROL_WORKERS` fixed threads), authenticating or
//!   serving a request. Admission counts these, queued plus running.
//! * on a WAIT lane ([`WaitLanes`], at most `CONTROL_WAIT_WORKERS` threads, grown
//!   on demand and kept): serving a request whose time is spent WAITING on an
//!   event — `await`, `ready`, `wait`, `turn`, a `post` or `inbox get @…` that
//!   parks on the bridge — or relaying the connection somewhere else (`dial
//!   <name>`, a session this instance does not host). A request lane that reads
//!   such a line hands the connection over and is free again at once. With every
//!   wait lane taken, the request is served where it is, as before.
//! * PARKED ([`Parker`], one thread): authenticated, idle between requests,
//!   nothing buffered. One `poll(2)` over every parked descriptor; a connection
//!   that turns readable goes back to the request lanes' queue, and one whose
//!   peer hung up with nothing to read is closed right there.
//! * on the subscription pool (unchanged: a `subscribe` flips it to push).
//!
//! **Admission.** A fresh connection gets the prompt `ERR control server busy;
//! retry` when every request lane is taken by work, or when the open-connection
//! cap is reached (`control_connections_cap`: a quarter of the process's soft
//! descriptor limit, at most `CONTROL_CONNECTIONS_MAX` — 64 in a window launchd
//! started, whose limit is 256). That cap is the parker's capacity, so an idle
//! connection ALWAYS has a place to park and never keeps a lane (a lane kept by
//! an idle connection would also starve the resumes queued behind it). Open
//! connections, not requests, are what that second bound caps: it is the memory
//! and descriptor bound the fixed lanes used to give, and it leaves most of the
//! descriptors to tabs and files, so the connection past it still gets the busy
//! line rather than a process with none left to accept it.
//!
//! **What is kept.** Per-connection request ORDER: one owner at a time, and a
//! connection moves only between requests with nothing left in its read buffer
//! (a pipelined next request already buffered is served where it is). The auth
//! and uid checks (the listener's, and the handshake on the first lane). Claims
//! bound to a connection (`meta set supervisor` without `ttl=`) stay bound across
//! every move and are released when the connection ends, however it ends — the
//! release is a drop guard ([`ConnTenure`]), so a lane's panic or the parker's
//! close releases them too. The cut `RelayCtl`'s interrupter makes (a client-side
//! `shutdown`) reads, wherever the connection is, as the hang-up it always was.
//! Every queue is bounded and every thread count is capped. Without a parker (off
//! unix, or one that could not start) an idle connection stays on its lane and
//! the connection bound is the lanes', exactly as before.
//!
//! Derived model: `aterm_spec::derive::control_lane_tenure_model`; its Tier-1
//! bind (`control_lanes_conformance.rs`) drives these lanes with real sockets.

use std::collections::VecDeque;
use std::io::BufReader;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};

use aterm_uds::CtlStream;

use super::{BoundedDispatch, Scope, ServeDisposition, read_authenticated_request_line};

/// What a lane serves a connection's requests WITH: the control server in the
/// window ([`super::ControlWorkerContext`]), a stub in the scheduling tests.
pub(super) trait LaneService: Send + Sync {
    /// Read the handshake and resolve its scope, with the verb a `TOKEN` line
    /// folded in. `None` closes the connection (the service has answered).
    fn authenticate(
        &self,
        stream: &CtlStream,
        reader: &mut BufReader<&CtlStream>,
    ) -> Option<(Scope, Option<String>)>;
    /// Serve one authenticated request. `None` keeps the connection.
    fn serve_line(
        &self,
        line: String,
        scope: Scope,
        stream: &CtlStream,
        reader: &mut BufReader<&CtlStream>,
    ) -> Option<ServeDisposition>;
    /// Whether `line` belongs on a wait lane (see the module doc).
    fn is_wait(&self, line: &str, scope: Scope) -> bool;
    /// Take over a connection that flipped to push.
    fn subscribe(&self, line: String, scope: Scope, stream: CtlStream);
    /// Release every claim bound to connection `id`: it ended.
    fn release(&self, id: u64);
}

/// The caps. Production: `CONTROL_WORKERS`, `CONTROL_WAIT_WORKERS`,
/// `control_connections_cap()`; the scheduling tests use small ones.
#[derive(Clone, Copy, Debug)]
pub(super) struct LaneLimits {
    /// Request lanes.
    pub(super) rpc: usize,
    /// Wait lanes, at most.
    pub(super) wait: usize,
    /// Connections open at once while the parker runs — its capacity.
    pub(super) open: usize,
}

/// A connection's claim to its own serial, and the release of whatever was
/// bound to it, as a drop guard: when the connection ends — closed, flipped to
/// push, dropped by a lane's unwind or by the parker — its claims go with it.
struct ConnTenure {
    id: u64,
    claimed: bool,
    service: Arc<dyn LaneService>,
}

impl Drop for ConnTenure {
    fn drop(&mut self) {
        if self.claimed {
            self.service.release(self.id);
        }
    }
}

/// One slot of a counted capacity, given back on drop: an OPEN connection
/// (taken at admission) or a PARKED one (taken when it parks, given back when a
/// request lane takes it again or it closes).
struct Slot(Arc<AtomicUsize>);

impl Slot {
    /// Take one slot of `count` below `cap`, or none.
    fn take(count: &Arc<AtomicUsize>, cap: usize) -> Option<Self> {
        count
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < cap).then_some(n + 1)
            })
            .ok()?;
        Some(Self(Arc::clone(count)))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let previous = self.0.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "a capacity slot was given back twice");
    }
}

/// One connection between placements.
pub(super) struct Conn {
    stream: CtlStream,
    /// `None` until the handshake on its first lane.
    scope: Option<Scope>,
    tenure: ConnTenure,
    /// Held from parking until a request lane takes it back.
    parked: Option<Slot>,
    /// Held from admission until the connection ends (or flips to push). Last,
    /// so a new peer is admitted only after this one's claims are released.
    _open: Slot,
}

impl Conn {
    /// End the connection: an explicit shutdown first (a vanished macOS
    /// AF_UNIX peer can otherwise linger in the kernel), then the tenure's
    /// release and the open slot as the value drops.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ControlLaneTenure",
            action = "Close",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ControlLaneTenure",
            action = "WaitClose",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    fn close(self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }

    /// A request lane takes a resumed connection back: its parking slot is
    /// free again.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ControlLaneTenure",
            action = "Pick",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    fn take_back(&mut self) {
        self.parked = None;
    }
}

/// The request lanes' work: a freshly accepted connection (holding the open
/// slot its admission took), or a parked one that turned readable.
enum Job {
    Fresh(CtlStream, Slot),
    Resume(Conn),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lane {
    Rpc,
    Wait,
}

/// Why a lane let go of a connection.
enum Here {
    Closed,
    Subscribe(String, Scope),
    /// Idle, nothing buffered: park it.
    #[cfg_attr(not(unix), allow(dead_code))]
    Idle,
    /// A wait read on a request lane, nothing else buffered: move it.
    Wait(String),
}

/// The wait lanes: grown one thread at a time up to `max`, and kept.
struct WaitLanes {
    max: usize,
    state: Mutex<WaitState>,
    ready: Condvar,
}

#[derive(Default)]
struct WaitState {
    queue: VecDeque<(Conn, String)>,
    idle: usize,
    threads: usize,
    running: usize,
}

#[cfg(unix)]
struct Parker {
    parked: Arc<AtomicUsize>,
    inbox: Mutex<Vec<Conn>>,
    /// The write end of the parker's bell (non-blocking): a byte wakes its poll.
    bell: std::os::unix::net::UnixStream,
}

/// The whole lane set of one control socket.
pub(super) struct Lanes {
    limits: LaneLimits,
    rpc: BoundedDispatch<Job>,
    waits: WaitLanes,
    open: Arc<AtomicUsize>,
    #[cfg(unix)]
    parker: OnceLock<Arc<Parker>>,
    service: OnceLock<Arc<dyn LaneService>>,
}

/// What the lanes hold right now (the Tier-1 bind's projection).
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Census {
    /// Request-lane work, queued plus running (fresh admission reads this).
    pub(super) rpc: usize,
    /// Parked connections plus resumes no lane has taken yet.
    pub(super) parked: usize,
    /// Wait-lane work, queued plus running.
    pub(super) waits: usize,
    pub(super) wait_threads: usize,
    /// Connections open (admitted, not yet ended or flipped to push).
    pub(super) open: usize,
}

impl Lanes {
    pub(super) fn new(limits: LaneLimits) -> Arc<Self> {
        Arc::new(Self {
            limits,
            // Room for every request lane's fresh admission plus every resume a
            // parked connection can put in the queue.
            rpc: BoundedDispatch::with_resume_room(limits.rpc, limits.open),
            waits: WaitLanes {
                max: limits.wait,
                state: Mutex::new(WaitState::default()),
                ready: Condvar::new(),
            },
            open: Arc::default(),
            #[cfg(unix)]
            parker: OnceLock::new(),
            service: OnceLock::new(),
        })
    }

    /// Start the request lanes and the parker. Returns the number of request
    /// lanes running; admission is published for exactly those. A parker that
    /// cannot start leaves every idle connection on its lane, as before.
    pub(super) fn start(self: &Arc<Self>) -> usize {
        let mut started = 0;
        for index in 0..self.limits.rpc {
            let lanes = Arc::clone(self);
            match std::thread::Builder::new()
                .name(format!("aterm-control-{index}"))
                .spawn(move || {
                    // QoS FLOOR (qos.rs): this lane runs the verb dispatch, and a
                    // read verb HOLDS THE TERMINAL MUTEX while it formats — `text`
                    // walks every visible row under one `term_lock`. Undeclared,
                    // the thread ran at the inherited DEFAULT band, below the UI
                    // thread that takes the same mutex on the key path
                    // (`term_lock_ui`) and in the redraw, so an agent polling
                    // `text`/`screen` could leave a descheduled holder in front of
                    // the next keystroke. Deliberately NOT `Interactive`: a verb
                    // storm must never outrank the UI thread, only stop sitting
                    // descheduled underneath it.
                    crate::qos::set_self(crate::qos::Role::Responsive);
                    lanes.rpc_lane();
                }) {
                Ok(_) => started += 1,
                Err(error) => aterm_log::warn!("control worker {index} could not start: {error}"),
            }
        }
        self.rpc.set_capacity(started);
        #[cfg(unix)]
        if self.limits.open > 0 {
            self.start_parker();
        }
        started
    }

    /// Publish what the lanes serve with. No connection is admitted before.
    pub(super) fn publish(&self, service: Arc<dyn LaneService>) {
        let _ = self.service.set(service);
    }

    /// Admit a freshly accepted connection onto a request lane, or hand it back
    /// (for the listener's busy reply) when every request lane is taken by work
    /// or the open-connection bound is reached.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ControlLaneTenure",
            action = "Admit",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ControlLaneTenure",
            action = "Refuse",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    pub(super) fn admit(&self, stream: CtlStream) -> Result<(), CtlStream> {
        // With no parker an idle connection keeps its lane, so the lanes bound
        // the connections, as they always did.
        let cap = if self.parker_runs() {
            self.limits.open
        } else {
            usize::MAX
        };
        let Some(open) = Slot::take(&self.open, cap) else {
            return Err(stream);
        };
        self.rpc
            .try_submit(Job::Fresh(stream, open))
            .map_err(|job| match job {
                Job::Fresh(stream, _open) => stream,
                Job::Resume(_) => unreachable!("admission submits only fresh connections"),
            })
    }

    #[cfg(test)]
    pub(super) fn census(&self) -> Census {
        let waits = self
            .waits
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Census {
            rpc: self.rpc.outstanding(),
            parked: self.parked(),
            waits: waits.queue.len() + waits.running,
            wait_threads: waits.threads,
            open: self.open.load(Ordering::Acquire),
        }
    }

    #[cfg(all(test, unix))]
    fn parked(&self) -> usize {
        self.parker
            .get()
            .map_or(0, |p| p.parked.load(Ordering::Acquire))
    }

    #[cfg(all(test, not(unix)))]
    fn parked(&self) -> usize {
        0
    }

    fn service(&self) -> &Arc<dyn LaneService> {
        self.service
            .get()
            .expect("the lane service is published before any connection is admitted")
    }

    fn rpc_lane(self: &Arc<Self>) {
        loop {
            if !self.rpc.serve_next(|job| match job {
                Job::Fresh(stream, open) => {
                    let conn = Conn {
                        stream,
                        scope: None,
                        tenure: ConnTenure {
                            id: super::NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed),
                            claimed: false,
                            service: Arc::clone(self.service()),
                        },
                        parked: None,
                        _open: open,
                    };
                    self.drive(conn, None, Lane::Rpc);
                }
                Job::Resume(mut conn) => {
                    conn.take_back();
                    self.drive(conn, None, Lane::Rpc);
                }
            }) {
                aterm_log::warn!("control worker recovered after a connection panic");
            }
        }
    }

    fn wait_lane(self: &Arc<Self>) {
        crate::qos::set_self(crate::qos::Role::Responsive);
        let mut state = self
            .waits
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some((conn, line)) = state.queue.pop_front() {
                state.running += 1;
                drop(state);
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.drive(conn, Some(line), Lane::Wait);
                }))
                .is_err()
                {
                    aterm_log::warn!("control wait lane recovered after a connection panic");
                }
                state = self
                    .waits
                    .state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                state.running -= 1;
                continue;
            }
            state.idle += 1;
            state = self
                .waits
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
            state.idle -= 1;
        }
    }

    /// Hand `conn` and the wait it just read to a wait lane: an idle one, or a
    /// new one while the cap allows. Never queues behind running waits — a
    /// wait no lane can take at once comes back, to be served where it is.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ControlLaneTenure",
            action = "Defer",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    fn defer(self: &Arc<Self>, conn: Conn, line: String) -> Result<(), (Conn, String)> {
        let mut state = self
            .waits
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if state.idle <= state.queue.len() {
            if state.threads >= self.waits.max {
                return Err((conn, line));
            }
            let lanes = Arc::clone(self);
            let name = format!("aterm-control-wait-{}", state.threads);
            match std::thread::Builder::new()
                .name(name)
                .spawn(move || lanes.wait_lane())
            {
                Ok(_) => state.threads += 1,
                Err(error) => {
                    aterm_log::warn!("control wait lane could not start: {error}");
                    return Err((conn, line));
                }
            }
        }
        state.queue.push_back((conn, line));
        self.waits.ready.notify_one();
        Ok(())
    }

    /// Carry one connection until it leaves this lane: closed, flipped to push,
    /// parked, or moved to a wait lane. Every fallback keeps it HERE, which is
    /// what every connection got before.
    fn drive(self: &Arc<Self>, mut conn: Conn, mut line: Option<String>, lane: Lane) {
        let mut may_park = true;
        let mut may_defer = true;
        loop {
            match self.serve_here(&mut conn, line.take(), lane, may_park, may_defer) {
                Here::Closed => {
                    conn.close();
                    return;
                }
                Here::Subscribe(line, scope) => {
                    let Conn { stream, tenure, .. } = conn;
                    // A push stream reads no more requests: what it bound ends here.
                    drop(tenure);
                    let service = Arc::clone(self.service());
                    service.subscribe(line, scope, stream);
                    return;
                }
                Here::Idle => match self.park(conn) {
                    Ok(()) => return,
                    Err(back) => {
                        // Its next request is read here, blocking; parking is
                        // tried again once that request is answered.
                        conn = back;
                        may_park = false;
                        may_defer = true;
                    }
                },
                Here::Wait(wait) => match self.defer(conn, wait) {
                    Ok(()) => return,
                    Err((back, wait)) => {
                        conn = back;
                        line = Some(wait);
                        may_defer = false;
                    }
                },
            }
        }
    }

    /// Serve `conn` on this lane until it must leave it (see [`Here`]).
    fn serve_here(
        &self,
        conn: &mut Conn,
        mut pending: Option<String>,
        lane: Lane,
        may_park: bool,
        mut may_defer: bool,
    ) -> Here {
        let service = Arc::clone(self.service());
        let Conn {
            stream,
            scope,
            tenure,
            ..
        } = conn;
        let stream: &CtlStream = stream;
        let _serving = Serving::enter(tenure.id, &mut tenure.claimed);
        let mut reader = BufReader::new(stream);
        let scope = match *scope {
            Some(scope) => scope,
            None => {
                let Some((authenticated, inline)) = service.authenticate(stream, &mut reader)
                else {
                    return Here::Closed;
                };
                *scope = Some(authenticated);
                // A bare `TOKEN` line is only an acknowledgement.
                pending = inline.filter(|verb| !verb.is_empty());
                authenticated
            }
        };
        let mut served = false;
        loop {
            let line = match pending.take() {
                Some(line) => line,
                None => {
                    if (may_park || served) && reader.buffer().is_empty() && self.parkable(stream) {
                        return Here::Idle;
                    }
                    match read_authenticated_request_line(&mut reader) {
                        Some(line) => line,
                        None => return Here::Closed,
                    }
                }
            };
            if lane == Lane::Rpc
                && may_defer
                && self.limits.wait > 0
                && reader.buffer().is_empty()
                && service.is_wait(&line, scope)
            {
                return Here::Wait(line);
            }
            may_defer = true;
            match service.serve_line(line, scope, stream, &mut reader) {
                None => served = true,
                Some(ServeDisposition::Close) => return Here::Closed,
                Some(ServeDisposition::Subscribe { line, scope }) => {
                    return Here::Subscribe(line, scope);
                }
            }
        }
    }

    #[cfg(unix)]
    fn parker_runs(&self) -> bool {
        self.parker.get().is_some()
    }

    #[cfg(not(unix))]
    fn parker_runs(&self) -> bool {
        false
    }

    /// Whether an idle connection could park now: a parker runs, and the
    /// connection has nothing to read yet (parking it would only bounce it
    /// straight back).
    #[cfg(unix)]
    fn parkable(&self, stream: &CtlStream) -> bool {
        self.parker_runs() && !readable_now(stream)
    }

    #[cfg(not(unix))]
    fn parkable(&self, _stream: &CtlStream) -> bool {
        false
    }

    #[cfg(not(unix))]
    fn park(&self, conn: Conn) -> Result<(), Conn> {
        Err(conn)
    }

    /// Park an idle connection. The open-connection bound is the parker's
    /// capacity, so a slot is always free here while the parker runs; the check
    /// stays, and a refusal keeps the connection on its lane, as before.
    #[cfg(unix)]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ControlLaneTenure",
            action = "Finish",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ControlLaneTenure",
            action = "WaitFinish",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    fn park(&self, mut conn: Conn) -> Result<(), Conn> {
        use std::io::Write as _;

        let Some(parker) = self.parker.get() else {
            return Err(conn);
        };
        let Some(slot) = Slot::take(&parker.parked, self.limits.open) else {
            return Err(conn);
        };
        conn.parked = Some(slot);
        parker
            .inbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(conn);
        // Non-blocking: a full bell already has a wake pending.
        let _ = (&parker.bell).write(&[1]);
        Ok(())
    }

    #[cfg(unix)]
    fn start_parker(self: &Arc<Self>) {
        let (bell, listen) = match std::os::unix::net::UnixStream::pair() {
            Ok(pair) => pair,
            Err(error) => {
                aterm_log::warn!("control parker has no bell: {error}");
                return;
            }
        };
        if bell.set_nonblocking(true).is_err() || listen.set_nonblocking(true).is_err() {
            aterm_log::warn!("control parker bell cannot be made non-blocking");
            return;
        }
        let parker = Arc::new(Parker {
            parked: Arc::default(),
            inbox: Mutex::new(Vec::new()),
            bell,
        });
        let lanes = Arc::clone(self);
        let polled = Arc::clone(&parker);
        match std::thread::Builder::new()
            .name("aterm-control-parker".to_string())
            .spawn(move || {
                // On the latency path of every parked driver's next request.
                crate::qos::set_self(crate::qos::Role::Responsive);
                lanes.parker_loop(&polled, &listen);
            }) {
            // Published only once something polls it: before that — and for
            // ever, if the thread could not start — idle connections stay on
            // their lanes, as they always did.
            Ok(_) => {
                let _ = self.parker.set(parker);
            }
            Err(error) => aterm_log::warn!("control parker could not start: {error}"),
        }
    }

    /// The parker: one `poll(2)` over every parked connection and the bell.
    #[cfg(unix)]
    fn parker_loop(&self, parker: &Parker, listen: &std::os::unix::net::UnixStream) {
        use std::io::Read as _;
        use std::os::fd::AsRawFd;

        let mut parked: Vec<Conn> = Vec::new();
        let mut fds: Vec<libc::pollfd> = Vec::new();
        loop {
            fds.clear();
            fds.push(libc::pollfd {
                fd: listen.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            fds.extend(parked.iter().map(|conn| libc::pollfd {
                fd: conn.stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            }));
            let Ok(count) = libc::nfds_t::try_from(fds.len()) else {
                aterm_log::warn!("control parker: too many descriptors to poll");
                return;
            };
            // SAFETY: `fds` is an initialized, exclusively borrowed array of
            // `count` pollfds, and every descriptor in it is owned by `listen`
            // or by a `Conn` in `parked`, both alive across the call. -1 waits
            // until a descriptor is ready or a signal interrupts.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), count, -1) };
            if ready < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    aterm_log::warn!("control parker poll failed: {error}");
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                continue;
            }
            // Readiness of the connections this poll watched. Downward, so a
            // `swap_remove` only moves an entry already looked at.
            for index in (0..parked.len()).rev() {
                let events = fds[index + 1].revents;
                if events == 0 {
                    continue;
                }
                let conn = parked.swap_remove(index);
                match wake_kind(&conn.stream, events) {
                    Wake::Request => self.resume(conn),
                    Wake::HungUp => close_parked(conn),
                    Wake::Spurious => parked.push(conn),
                }
            }
            if fds[0].revents != 0 {
                let mut drain = [0u8; 64];
                while matches!((&*listen).read(&mut drain), Ok(n) if n > 0) {}
                let arrivals = std::mem::take(
                    &mut *parker.inbox.lock().unwrap_or_else(PoisonError::into_inner),
                );
                parked.extend(arrivals);
            }
        }
    }

    /// A parked connection turned readable: back to the request lanes' queue,
    /// behind whatever runs, holding its parking slot until a lane takes it.
    #[cfg(unix)]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ControlLaneTenure",
            action = "Wake",
            project = "aterm_gui::control::control_lanes::conformance::project"
        )
    )]
    fn resume(&self, conn: Conn) {
        if let Err(job) = self.rpc.submit_resumed(Job::Resume(conn)) {
            // Unreachable while the open bound sizes the queue; never silently
            // strand a live connection if it regresses.
            aterm_log::warn!("control parker: resume refused, closing");
            if let Job::Resume(conn) = job {
                conn.close();
            }
        }
    }
}

/// A parked connection's peer hung up with nothing left to read: closed where
/// it is, its tenure (and any claim bound to it) ended with it.
#[cfg(unix)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "ControlLaneTenure",
        action = "ParkedClose",
        project = "aterm_gui::control::control_lanes::conformance::project"
    )
)]
fn close_parked(conn: Conn) {
    conn.close();
}

/// What woke a parked connection.
#[cfg(unix)]
enum Wake {
    /// Bytes to read (maybe with a hang-up behind them): serve them.
    Request,
    /// The peer is gone and there is nothing to read.
    HungUp,
    /// Nothing after all: stay parked.
    Spurious,
}

#[cfg(unix)]
fn wake_kind(stream: &CtlStream, events: libc::c_short) -> Wake {
    use std::os::fd::AsRawFd;

    if events & libc::POLLNVAL != 0 {
        return Wake::HungUp;
    }
    let mut byte = 0u8;
    // SAFETY: `stream` is borrowed across the call and `byte` is one writable
    // byte. MSG_PEEK leaves the byte for the lane that reads the request.
    let peeked = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            std::ptr::addr_of_mut!(byte).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    match peeked {
        n if n > 0 => Wake::Request,
        0 => Wake::HungUp,
        _ => match std::io::Error::last_os_error().kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted => Wake::Spurious,
            _ => Wake::HungUp,
        },
    }
}

/// Whether the connection already has something to read (or has hung up), so
/// parking it would only bounce it straight back.
#[cfg(unix)]
fn readable_now(stream: &CtlStream) -> bool {
    use std::os::fd::AsRawFd;

    let mut fd = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one initialized pollfd over a descriptor `stream` keeps open;
    // a zero timeout never blocks.
    let ready = unsafe { libc::poll(&mut fd, 1, 0) };
    ready != 0
}

/// The serving thread's view of the connection it carries — the binding a
/// connection-scoped claim is made to ([`super::serving_connection`]). Set
/// while a lane serves, and the claim flag carried back into the connection
/// when it leaves the lane (normally or by unwinding).
struct Serving<'a> {
    claimed: &'a mut bool,
}

impl<'a> Serving<'a> {
    fn enter(id: u64, claimed: &'a mut bool) -> Self {
        super::SERVING_CONNECTION.with(|serving| serving.set(Some(id)));
        super::CONNECTION_CLAIMED.with(|flag| flag.set(*claimed));
        Self { claimed }
    }
}

impl Drop for Serving<'_> {
    fn drop(&mut self) {
        *self.claimed = super::CONNECTION_CLAIMED.with(|flag| flag.replace(false));
        super::SERVING_CONNECTION.with(|serving| serving.set(None));
    }
}

#[cfg(all(test, unix))]
#[path = "control_lanes_conformance.rs"]
mod conformance;
