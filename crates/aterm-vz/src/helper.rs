// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The helper's core and its serve loop (`docs/DESIGN-vz-guest-2026-10-23.md` §5.1, §10).
//!
//! [`Core`] is the whole of the helper's behaviour: its sessions and `keep`, the release
//! rule, the connection objects it holds, the boot it watches and every detector. It is one
//! thread's state, fed [`Input`]s with the time they arrived (a `Duration` since the helper
//! started), so the tests drive it with a stepped clock and the Fake [`Machine`]. The serve
//! loop ([`serve`]) is what makes inputs of the world: the control socket's sessions, the
//! machine's events, the agent's watch connection and a one-second tick.
//!
//! `aterm-vz-host` runs exactly this with the VZ arm; the Fake `Helper`
//! ([`crate::client::fake`]) runs it on a thread with the Fake arm.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::fd::BorrowedFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use aterm_vz_agent::wire::{self, Frame, Hello, Job};

use crate::console::{Console, Seen};
use crate::detect::{Fired, SPAN, Space, TICK, Watch};
use crate::key::Key;
use crate::machine::{Conn, Event, Machine, Sink};
use crate::plan::{Disk, Plan, ShareEntry};
use crate::proto::{Attach, PROTO, Phase, Reply, Request, StateReport};
use crate::record::{DiskLock, Holder, Kernel, Paths, Pids, Record};
use crate::space::Free;
use crate::transport::{self, Channel};

/// Whether the helper keeps each connection object until its `Release` or its session's
/// close, rather than dropping it once its descriptor is passed (§5.1). V2 measured that a
/// passed descriptor stays live after the object is dropped (`docs/measured/vz-v2-2026-10-23.md`),
/// so the helper drops it; the held mode stays, behind this switch, for the measure.
pub const HOLD_CONNS: bool = false;

/// How long a `Stop` from `down` waits for the guest to power off before the hard stop.
pub const STOP_GRACE: Duration = SPAN;

/// The `Lost` words of a VM whose guest powered itself off, with no stop of the helper's
/// under way and no detector fired: the one end a bake seals after.
pub const POWERED_OFF: &str = "the guest stopped: it powered itself off";

/// The dials of the watch port that may wait for their hello at once. A later one is
/// refused unread: a guest process that holds this many silent dials when the agent dials
/// makes the boot lost (the agent's dial is refused, and it dials once), never ready.
pub const MAX_DIALS: usize = 8;

/// Who the helper is, as its record and `State` name it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ident {
    /// Its pid.
    pub pid: u32,
    /// Its unique id.
    pub uniq: u64,
    /// Its kernel start time ([`crate::record::Pids::birth_us`]).
    pub birth_us: u64,
    /// Its executable's content address.
    pub helper_sha256: String,
    /// The live disk it holds.
    pub live_disk: PathBuf,
}

/// What the core is built with besides its machine.
pub struct Config {
    /// Who it is.
    pub ident: Ident,
    /// The record it rewrites once the agent says hello (`None` in the core's tests).
    pub record: Option<(PathBuf, Record)>,
    /// [`HOLD_CONNS`], or the measure's other arm.
    pub hold_conns: bool,
    /// Where each `Boot` writes its key disk ([`crate::key`]), which the plan's disks gain,
    /// read-only, last.
    pub key_disk: PathBuf,
    /// The live disk's volume's free space, read every [`TICK`] while the VM runs.
    pub free: Box<dyn FnMut() -> Result<Free, String> + Send>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("ident", &self.ident)
            .field("record", &self.record)
            .field("hold_conns", &self.hold_conns)
            .field("key_disk", &self.key_disk)
            .finish_non_exhaustive()
    }
}

/// One thing that happened, for the core.
#[derive(Debug)]
pub enum Input {
    /// A control connection opened: its id, and the core's writer on it.
    Opened(u64, UnixStream),
    /// It sent a request.
    Request(u64, Request),
    /// It closed (EOF, or an unreadable message).
    Closed(u64),
    /// The machine did something.
    Machine(Event),
    /// A frame of the watch port's dial `u64`. Its reader posts none until the dial's first
    /// frame was a `Hello` carrying this boot's key, and that hello comes first, its key
    /// zeroed.
    Watch(u64, Frame),
    /// The dial's first frame was no hello with this boot's key, and why: no frame of it is
    /// believed, and the helper keeps listening.
    WatchRefused(u64, String),
    /// The dial's connection ended after its hello, and why.
    WatchClosed(u64, String),
    /// A second passed.
    Tick,
}

/// Whether the helper goes on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Flow {
    /// Go on.
    Go,
    /// Exit, for this reason, which the helper's log ends with.
    Exit(String),
}

/// One control connection.
#[derive(Debug)]
struct Session {
    out: UnixStream,
    attached: bool,
}

impl Drop for Session {
    fn drop(&mut self) {
        // The socket's reader thread holds a duplicate: a shutdown ends both, so the lab
        // sees EOF whether or not the helper's process ends.
        let _ = self.out.shutdown(std::net::Shutdown::Both);
    }
}

/// One dial of the watch port: the connection object and the core's writer on it.
#[derive(Debug)]
struct Dial {
    /// Held for the dial's life: the object owns the descriptor (fact 31).
    _conn: Conn,
    out: UnixStream,
}

impl Drop for Dial {
    fn drop(&mut self) {
        // Its reader thread holds a duplicate: a shutdown ends it, and the guest's end sees
        // EOF, whether or not the object's close comes later.
        let _ = self.out.shutdown(std::net::Shutdown::Both);
    }
}

/// A dial whose hello has not come yet, since when.
#[derive(Debug)]
struct Pending {
    dial: Dial,
    since: Duration,
}

/// A stop under way.
#[derive(Debug)]
enum Stopping {
    /// The guest was asked to power off at `since` (`Poweroff` through the agent, or
    /// `requestStopWithError:`); it is stopped hard after `grace` with no new console line.
    Soft {
        since: Duration,
        grace: Duration,
        why: String,
        /// The command connection the `Poweroff` went on, kept until the VM stops.
        _conn: Option<Conn>,
    },
    /// `stopWithCompletionHandler:` was called at `since`.
    Hard { since: Duration, why: String },
}

impl Stopping {
    fn why(&self) -> &str {
        match self {
            Self::Soft { why, .. } | Self::Hard { why, .. } => why,
        }
    }
}

/// The helper's state.
pub struct Core<M: Machine> {
    machine: M,
    cfg: Config,
    inputs: Sender<Input>,
    sessions: BTreeMap<u64, Session>,
    ever_attached: bool,
    keep: bool,
    phase: Phase,
    started: bool,
    plan: Option<Plan>,
    shares: Vec<ShareEntry>,
    console: Console,
    console_file: Option<std::fs::File>,
    watch: Option<Watch>,
    space: Space,
    hello: Option<Hello>,
    jobs: Vec<Job>,
    failed_units: Vec<String>,
    /// This boot's key: a dial whose first frame is a `Hello` carrying it is the agent's.
    key: Option<Key>,
    /// The dials whose hello has not come yet.
    dials: BTreeMap<u64, Pending>,
    next_dial: u64,
    /// The agent's watch connection: the first dial whose hello carried the key.
    watch_conn: Option<(u64, Dial)>,
    /// Whether this boot's watch connection was taken: the agent dials once per boot (§6),
    /// so every later dial of port 1025 is refused and its frames never reach the core.
    watch_taken: bool,
    /// This boot's refused dials before the agent's: how many, and the last one's words,
    /// which the boot detector's words carry (an agent of another protocol is refused
    /// here, its hello unread).
    refused: (usize, Option<String>),
    held: BTreeMap<u64, (u64, Conn)>,
    next_conn: u64,
    stopping: Option<Stopping>,
    stop_waiters: Vec<u64>,
    lost_sent: bool,
    last_note: Duration,
}

impl<M: Machine> std::fmt::Debug for Core<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Core")
            .field("phase", &self.phase)
            .field("keep", &self.keep)
            .field("sessions", &self.sessions.len())
            .field("held", &self.held.len())
            .finish_non_exhaustive()
    }
}

/// The helper's log line, on its stderr (`run/helper.log`).
macro_rules! note {
    ($($arg:tt)*) => {
        eprintln!("aterm-vz-host {}: {}", stamp(), format_args!($($arg)*))
    };
}

/// Now, as `helper.log` stamps each line: Unix seconds and milliseconds.
#[must_use]
pub fn stamp() -> String {
    aterm_time::since_epoch().map_or_else(
        || "?".to_string(),
        |t| format!("{}.{:03}", t.as_secs(), t.subsec_millis()),
    )
}

/// A duration in whole milliseconds.
fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

impl<M: Machine> Core<M> {
    /// A helper with no session and no VM; the watch reader and the machine's events post to
    /// `inputs`.
    pub fn new(machine: M, cfg: Config, inputs: Sender<Input>) -> Self {
        Self {
            machine,
            cfg,
            inputs,
            sessions: BTreeMap::new(),
            ever_attached: false,
            keep: false,
            phase: Phase::Absent,
            started: false,
            plan: None,
            shares: Vec::new(),
            console: Console::new(),
            console_file: None,
            watch: None,
            space: Space::new(Duration::ZERO),
            hello: None,
            jobs: Vec::new(),
            failed_units: Vec::new(),
            key: None,
            dials: BTreeMap::new(),
            next_dial: 1,
            watch_conn: None,
            watch_taken: false,
            refused: (0, None),
            held: BTreeMap::new(),
            next_conn: 1,
            stopping: None,
            stop_waiters: Vec::new(),
            lost_sent: false,
            last_note: Duration::ZERO,
        }
    }

    /// Where the VM is.
    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// Whether `keep` is set.
    #[must_use]
    pub const fn keep(&self) -> bool {
        self.keep
    }

    /// The sessions attached now.
    #[must_use]
    pub fn attached(&self) -> usize {
        self.sessions.values().filter(|s| s.attached).count()
    }

    /// The connection objects held now (the watch connection's aside).
    #[must_use]
    pub fn held(&self) -> usize {
        self.held.len()
    }

    /// The machine, for the tests' reads of the Fake.
    #[must_use]
    pub const fn machine(&self) -> &M {
        &self.machine
    }

    /// Take `input`, which arrived at `now`.
    pub fn handle(&mut self, input: Input, now: Duration) -> Flow {
        match input {
            Input::Opened(sid, out) => {
                // A lab that stops reading for a whole span is gone: its writes fail and its
                // session closes, rather than the helper waiting on it.
                let _ = out.set_write_timeout(Some(SPAN));
                self.sessions.insert(
                    sid,
                    Session {
                        out,
                        attached: false,
                    },
                );
                Flow::Go
            }
            Input::Request(sid, request) => self.request(sid, request, now),
            Input::Closed(sid) => self.close(sid, now),
            Input::Machine(event) => self.event(event, now),
            Input::Watch(dial, frame) => self.watch_frame(dial, frame, now),
            Input::WatchRefused(dial, why) => {
                if self.dials.remove(&dial).is_some() {
                    self.refuse(why);
                }
                Flow::Go
            }
            Input::WatchClosed(dial, why) => {
                if self.watch_conn.as_ref().is_some_and(|(d, _)| *d == dial) {
                    note!("the watch connection ended: {why}");
                    self.watch_conn = None;
                }
                self.dials.remove(&dial);
                Flow::Go
            }
            Input::Tick => self.tick(now),
        }
    }

    fn running(&self) -> bool {
        matches!(self.phase, Phase::Booting | Phase::Ready)
    }

    fn reply(&self, sid: u64, reply: &Reply, fd: Option<BorrowedFd<'_>>) {
        let Some(session) = self.sessions.get(&sid) else {
            return;
        };
        let sent = reply
            .encode()
            .map_err(|e| e.to_string())
            .and_then(|bytes| transport::send(&session.out, &bytes, fd).map_err(|e| e.to_string()));
        if let Err(e) = sent {
            note!("session {sid}: {e}; closing it");
            let _ = session.out.shutdown(std::net::Shutdown::Both);
        }
    }

    fn broadcast(&self, reply: &Reply) {
        for (sid, s) in &self.sessions {
            if s.attached {
                self.reply(*sid, reply, None);
            }
        }
    }

    fn lost(&mut self, why: &str) {
        self.lost_sent = true;
        self.broadcast(&Reply::Lost {
            why: why.to_string(),
            tail: self.console.tail(),
        });
    }

    fn request(&mut self, sid: u64, request: Request, now: Duration) -> Flow {
        let attached = self.sessions.get(&sid).is_some_and(|s| s.attached);
        match request {
            Request::State => {
                let state = self.state(now);
                self.reply(sid, &Reply::State(Box::new(state)), None);
                Flow::Go
            }
            Request::Stop { grace_ms } => {
                self.stop_request(sid, Duration::from_millis(grace_ms), now)
            }
            Request::Detach => self.close(sid, now),
            Request::Attach(attach) => self.attach(sid, attach),
            _ if !attached => {
                self.reply(
                    sid,
                    &Reply::Error("this request needs a session: Attach first".into()),
                    None,
                );
                Flow::Go
            }
            Request::Boot(plan) => self.boot(sid, *plan, now),
            Request::Connect { port } => self.connect(sid, port),
            Request::Release { id } => {
                if self.held.get(&id).is_some_and(|(owner, _)| *owner == sid) {
                    self.held.remove(&id);
                }
                self.reply(sid, &Reply::Done, None);
                Flow::Go
            }
            Request::Share(entries) => {
                let reply = if self.running() {
                    match self.machine.share(&entries) {
                        Ok(()) => {
                            self.shares = entries;
                            Reply::Done
                        }
                        Err(e) => Reply::Error(format!("the aterm share was not swapped: {e}")),
                    }
                } else {
                    Reply::Error(format!("the VM is {}: no share to swap", self.phase.name()))
                };
                self.reply(sid, &reply, None);
                Flow::Go
            }
            Request::Keep(keep) => {
                self.keep = keep;
                self.reply(sid, &Reply::Done, None);
                Flow::Go
            }
        }
    }

    /// The agent the running VM has or will have: the guest's, once it said hello, else the
    /// plan's.
    fn agent_now(&self) -> Option<String> {
        if let Some(h) = &self.hello {
            return Some(crate::hex(&h.agent_sha256));
        }
        self.running()
            .then(|| self.plan.as_ref().map(|p| p.agent_sha256.clone()))
            .flatten()
    }

    fn attach(&mut self, sid: u64, a: Attach) -> Flow {
        let refusal = if a.proto != PROTO {
            Some(format!(
                "this helper speaks protocol {PROTO}; the session's checkout speaks {}",
                a.proto
            ))
        } else if a.helper_sha256 != self.cfg.ident.helper_sha256 {
            Some(format!(
                "this helper is {}; the session's checkout signed {}",
                self.cfg.ident.helper_sha256, a.helper_sha256
            ))
        } else if self.stopping.is_some()
            || !matches!(self.phase, Phase::Absent | Phase::Booting | Phase::Ready)
        {
            Some(format!(
                "the VM is {}: this helper exits once its sessions close",
                self.phase.name()
            ))
        } else {
            self.agent_now()
                .filter(|agent| *agent != a.agent_sha256)
                .map(|agent| {
                    format!(
                        "the guest runs agent {agent}; the session's checkout built {}",
                        a.agent_sha256
                    )
                })
        };
        if let Some(why) = refusal {
            self.reply(sid, &Reply::Refused(why), None);
            return Flow::Go;
        }
        let mut swapped = false;
        if self.running() && a.shares != self.shares {
            match self.machine.share(&a.shares) {
                Ok(()) => {
                    self.shares = a.shares;
                    swapped = true;
                }
                Err(e) => {
                    self.reply(
                        sid,
                        &Reply::Refused(format!("the aterm share was not swapped: {e}")),
                        None,
                    );
                    return Flow::Go;
                }
            }
        }
        if let Some(s) = self.sessions.get_mut(&sid) {
            s.attached = true;
        }
        self.ever_attached = true;
        if a.keep {
            self.keep = true;
        }
        self.reply(
            sid,
            &Reply::Attached {
                phase: self.phase,
                keep: self.keep,
                swapped,
            },
            None,
        );
        if self.phase == Phase::Ready
            && let Some(h) = &self.hello
        {
            self.reply(sid, &Reply::Ready(h.clone()), None);
        }
        Flow::Go
    }

    fn fail(&mut self, error: String) {
        self.phase = Phase::Failed;
        note!("{error}");
        self.broadcast(&Reply::Failed {
            error,
            tail: self.console.tail(),
        });
    }

    fn boot(&mut self, sid: u64, plan: Plan, now: Duration) -> Flow {
        match self.phase {
            Phase::Absent => {}
            Phase::Booting => {
                if self.started {
                    self.reply(sid, &Reply::Started, None);
                }
                return Flow::Go;
            }
            Phase::Ready => {
                self.reply(sid, &Reply::Started, None);
                if let Some(h) = &self.hello {
                    self.reply(sid, &Reply::Ready(h.clone()), None);
                }
                return Flow::Go;
            }
            other => {
                self.reply(
                    sid,
                    &Reply::Error(format!(
                        "the VM is {}: this helper exits once its sessions close",
                        other.name()
                    )),
                    None,
                );
                return Flow::Go;
            }
        }
        let limits = match self.machine.limits() {
            Ok(limits) => limits,
            Err(e) => {
                self.fail(e);
                return self.settle(now);
            }
        };
        if let Some(words) = limits.memory_refusal(plan.memory_bytes) {
            self.fail(words);
            return self.settle(now);
        }
        let mut plan = plan;
        let key = match crate::key::fresh().and_then(|key| {
            crate::key::write_disk(&self.cfg.key_disk, &key)?;
            Ok(key)
        }) {
            Ok(key) => key,
            Err(e) => {
                self.fail(format!(
                    "the boot's key disk {}: {e}",
                    self.cfg.key_disk.display()
                ));
                return self.settle(now);
            }
        };
        plan.disks.push(Disk {
            path: self.cfg.key_disk.clone(),
            read_only: true,
        });
        self.key = Some(key);
        self.dials.clear();
        note!("booting:\n{}", plan.config_text(&limits));
        // The last boot's console is kept beside this one's (a bake's, once its clone boots).
        let _ = std::fs::rename(&plan.console_log, previous_console(&plan.console_log));
        self.console_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&plan.console_log)
            .map_err(|e| note!("console.log {}: {e}", plan.console_log.display()))
            .ok();
        self.console = Console::new();
        self.watch = Some(Watch::new(now));
        self.watch_taken = false;
        self.refused = (0, None);
        self.space = Space::new(now);
        self.last_note = now;
        self.shares = plan.shares.clone();
        let inputs = self.inputs.clone();
        let sink = Sink::new(move |event| {
            let _ = inputs.send(Input::Machine(event));
        });
        let booted = self.machine.boot(&plan, &limits, sink);
        self.plan = Some(plan);
        match booted {
            Ok(()) => {
                self.phase = Phase::Booting;
                Flow::Go
            }
            Err(e) => {
                self.fail(e);
                self.settle(now)
            }
        }
    }

    fn connect(&mut self, sid: u64, port: u32) -> Flow {
        if !self.running() {
            self.reply(
                sid,
                &Reply::Error(format!("the VM is {}: no connection", self.phase.name())),
                None,
            );
            return Flow::Go;
        }
        match self.machine.connect(port) {
            Ok(conn) => {
                let id = self.next_conn;
                self.next_conn += 1;
                self.reply(sid, &Reply::Connected { id, port }, Some(conn.fd()));
                if self.cfg.hold_conns {
                    self.held.insert(id, (sid, conn));
                } else {
                    drop(conn);
                    note!(
                        "connection {id} to guest port {port}: descriptor passed, object dropped"
                    );
                }
            }
            Err(e) => self.reply(
                sid,
                &Reply::Error(format!("no connection to guest port {port}: {e}")),
                None,
            ),
        }
        Flow::Go
    }

    fn stop_request(&mut self, sid: u64, grace: Duration, now: Duration) -> Flow {
        // The session hears `Stopping` now and again every TICK until the helper exits, so
        // its no-progress detector reads a helper at work, never a hung one.
        self.reply(sid, &Reply::Stopping, None);
        self.stop_waiters.push(sid);
        self.last_note = now;
        self.keep = false;
        if !self.running() && self.stopping.is_none() {
            return Flow::Exit(format!(
                "Stop with the VM {}: nothing to power off",
                self.phase.name()
            ));
        }
        if self.stopping.is_none() {
            return self.soft_stop(
                now,
                grace,
                "the VM was stopped by a Stop (down)".into(),
                true,
            );
        }
        Flow::Go
    }

    fn close(&mut self, sid: u64, now: Duration) -> Flow {
        if self.sessions.remove(&sid).is_none() {
            return Flow::Go;
        }
        // No connection object outlives its session.
        self.held.retain(|_, (owner, _)| *owner != sid);
        self.settle(now)
    }

    /// The release rule (§5.1).
    fn settle(&mut self, now: Duration) -> Flow {
        if self.attached() > 0 {
            return Flow::Go;
        }
        match self.phase {
            Phase::Absent | Phase::Failed | Phase::Stopped if self.ever_attached => {
                Flow::Exit("no session is left, and no VM runs".into())
            }
            Phase::Booting | Phase::Ready if !self.keep && self.stopping.is_none() => self
                .soft_stop(
                    now,
                    SPAN,
                    "the last session closed with keep unset".into(),
                    true,
                ),
            _ => Flow::Go,
        }
    }

    /// Ask the guest to power off: `Poweroff` through the agent once it is `Ready` and
    /// `agent` allows, else `requestStopWithError:`; a hard stop follows `grace` with no new
    /// console line.
    fn soft_stop(&mut self, now: Duration, grace: Duration, why: String, agent: bool) -> Flow {
        let conn = (agent && self.phase == Phase::Ready)
            .then(|| self.poweroff_through_agent())
            .flatten();
        let how = if conn.is_some() {
            "Poweroff through the agent"
        } else if let Err(e) = self.machine.request_stop() {
            note!("requestStopWithError: {e}");
            return self.hard_stop(now, why);
        } else {
            "requestStopWithError:"
        };
        note!("stopping ({why}): {how}");
        self.phase = Phase::Stopping;
        self.stopping = Some(Stopping::Soft {
            since: now,
            grace,
            why,
            _conn: conn,
        });
        Flow::Go
    }

    fn poweroff_through_agent(&mut self) -> Option<Conn> {
        let conn = self
            .machine
            .connect(aterm_vz_agent::COMMAND_PORT)
            .map_err(|e| note!("no command connection for Poweroff: {e}"))
            .ok()?;
        let mut stream = conn.stream().ok()?;
        wire::write_frame(&mut stream, &Frame::Poweroff)
            .map_err(|e| note!("Poweroff: {e}"))
            .ok()?;
        Some(conn)
    }

    fn hard_stop(&mut self, now: Duration, why: String) -> Flow {
        note!("stopping hard: {why}");
        self.phase = Phase::Stopping;
        let called = self.machine.hard_stop();
        self.stopping = Some(Stopping::Hard { since: now, why });
        match called {
            Ok(()) => Flow::Go,
            Err(e) => Flow::Exit(format!(
                "stopWithCompletionHandler: {e}; the helper exits, which ends its VM"
            )),
        }
    }

    /// A detector fired: every session hears `why` now, and the VM stops hard.
    fn fire(&mut self, now: Duration, why: String) -> Flow {
        self.lost(&why);
        self.hard_stop(now, why)
    }

    fn event(&mut self, event: Event, now: Duration) -> Flow {
        match event {
            Event::Started => {
                self.started = true;
                note!("started");
                self.broadcast(&Reply::Started);
                Flow::Go
            }
            Event::StartFailed(e) => {
                self.fail(e);
                self.settle(now)
            }
            Event::Console(bytes) => {
                if let Some(f) = &mut self.console_file
                    && let Err(e) = f.write_all(&bytes)
                {
                    note!("console.log: {e}; no longer written");
                    self.console_file = None;
                }
                let seen = self.console.feed(&bytes, now);
                for Seen::Panic(line) in seen {
                    if !matches!(self.stopping, Some(Stopping::Hard { .. })) {
                        return self.fire(
                            now,
                            format!("NOT RUN — the guest's kernel panicked: {line}; VM stopped"),
                        );
                    }
                }
                Flow::Go
            }
            Event::Watch(conn) => {
                self.watch_opened(conn, now);
                Flow::Go
            }
            Event::GuestStopped => self.stopped(None),
            Event::StoppedWithError(e) => self.stopped(Some(e)),
            Event::HardStopped(done) => {
                if let Err(e) = done {
                    note!("the hard stop answered: {e}");
                }
                self.stopped(None)
            }
        }
    }

    fn stopped(&mut self, error: Option<String>) -> Flow {
        let why = match (&error, &self.stopping) {
            (Some(e), _) => format!("the guest stopped with an error: {e}"),
            (None, Some(s)) => s.why().to_string(),
            (None, None) => POWERED_OFF.into(),
        };
        match error {
            Some(_) => self.fail(why.clone()),
            None => {
                self.phase = Phase::Stopped;
                if !self.lost_sent {
                    self.lost(&why);
                }
            }
        }
        self.held.clear();
        self.dials.clear();
        self.watch_conn = None;
        Flow::Exit(format!("the VM stopped: {why}"))
    }

    /// Something in the guest dialed port 1025. Any guest process may (§6), so no frame of a
    /// dial is believed until its first is a `Hello` carrying this boot's key, which only
    /// guest root reads: its reader thread ([`read_dial`]) checks that, and posts nothing
    /// else first. The first dial whose hello carried the key is the watch connection; the
    /// others still waiting are closed then, and every later dial is closed unread (the
    /// agent dials once per boot). A dial that says no hello within [`SPAN`] is closed at a
    /// tick.
    fn watch_opened(&mut self, conn: Conn, now: Duration) {
        let port = conn.port();
        if self.watch_taken {
            note!(
                "a dial of the watch port {port} after the agent's refused unread: the agent \
                 dials once per boot"
            );
            return;
        }
        let Some(key) = self.key else {
            note!("a dial of the watch port {port} with no boot under way refused unread");
            return;
        };
        if self.dials.len() >= MAX_DIALS {
            self.refuse(format!(
                "unread, since {MAX_DIALS} dials already wait for their hello"
            ));
            return;
        }
        let (reader, out) = match (conn.stream(), conn.stream()) {
            (Ok(r), Ok(w)) => (r, w),
            (Err(e), _) | (_, Err(e)) => {
                note!("a watch dial's descriptor: {e}");
                return;
            }
        };
        let dial = self.next_dial;
        self.next_dial += 1;
        let inputs = self.inputs.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("vz-watch-{dial}"))
            .spawn(move || read_dial(dial, reader, &key, &inputs));
        if let Err(e) = spawned {
            note!("the watch reader: {e}");
            return;
        }
        self.dials.insert(
            dial,
            Pending {
                dial: Dial { _conn: conn, out },
                since: now,
            },
        );
    }

    /// A dial of the watch port was refused, for `why`; the helper keeps listening.
    fn refuse(&mut self, why: String) {
        note!("a dial of the watch port refused: {why}; the helper keeps listening");
        if self.hello.is_none() {
            self.refused = (self.refused.0 + 1, Some(why));
        }
    }

    /// A frame of dial `dial`, which its reader posts only after that dial's keyed hello.
    fn watch_frame(&mut self, dial: u64, frame: Frame, now: Duration) -> Flow {
        if self.watch_conn.as_ref().is_some_and(|(d, _)| *d == dial) {
            return self.frame(frame, now);
        }
        let Some(pending) = self.dials.remove(&dial) else {
            // A dial already closed: nothing of it is believed.
            return Flow::Go;
        };
        if self.watch_taken || !matches!(frame, Frame::Hello(_)) {
            return Flow::Go;
        }
        self.watch_taken = true;
        let others = std::mem::take(&mut self.dials);
        if !others.is_empty() {
            note!(
                "{} other dial(s) of the watch port closed unread: the agent's carried the key",
                others.len()
            );
        }
        drop(others);
        self.watch_conn = Some((dial, pending.dial));
        self.frame(frame, now)
    }

    fn frame(&mut self, frame: Frame, now: Duration) -> Flow {
        if !self.running() {
            return Flow::Go;
        }
        match frame {
            Frame::Hello(h) => self.hello(h, now),
            Frame::Booting(jobs) => {
                if let Some(w) = &mut self.watch {
                    w.booting(&jobs, now);
                }
                self.jobs = jobs;
                Flow::Go
            }
            Frame::Ready(ready) => {
                let Some(hello) = self.hello.clone() else {
                    note!("a Ready before the agent's hello: ignored");
                    return Flow::Go;
                };
                if let Some(w) = &mut self.watch {
                    w.ready(now);
                }
                self.phase = Phase::Ready;
                note!(
                    "ready: {}{}",
                    ready.state,
                    if ready.failed_units.is_empty() {
                        String::new()
                    } else {
                        format!(", failed: {}", ready.failed_units.join(" "))
                    }
                );
                self.failed_units = ready.failed_units;
                self.broadcast(&Reply::Ready(hello));
                Flow::Go
            }
            Frame::Beat { .. } => {
                if let Some(w) = &mut self.watch {
                    w.beat(now);
                }
                Flow::Go
            }
            other => {
                note!(
                    "an unexpected frame on the watch connection (kind {:#04x})",
                    other.kind()
                );
                Flow::Go
            }
        }
    }

    fn hello(&mut self, h: Hello, now: Duration) -> Flow {
        let got = crate::hex(&h.agent_sha256);
        let want = self
            .plan
            .as_ref()
            .map(|p| p.agent_sha256.clone())
            .unwrap_or_default();
        if got != want || h.proto != wire::PROTO {
            let why = format!(
                "NOT RUN — the guest runs agent {got} (protocol {}); this checkout built {want} \
                 (protocol {})",
                h.proto,
                wire::PROTO
            );
            self.lost(&why);
            // §10: `Stop`, never through the agent that is not the plan's.
            return if self.stopping.is_none() {
                self.soft_stop(now, SPAN, why, false)
            } else {
                Flow::Go
            };
        }
        if let Some(w) = &mut self.watch {
            w.hello(now);
        }
        note!(
            "hello: agent {got}, boot {}, kernel {}, uptime {} ms",
            h.boot_id,
            h.kernel,
            h.uptime_ms
        );
        if let Some((path, record)) = &mut self.cfg.record {
            record.boot_id = Some(h.boot_id.clone());
            record.agent_sha256 = Some(got);
            if let Err(e) = record.write(path) {
                note!("the record {}: {e}", path.display());
            }
        }
        if let Some((_, dial)) = &mut self.watch_conn
            && let Some(t) = aterm_time::since_epoch()
        {
            let unix_ns = u64::try_from(t.as_nanos()).unwrap_or(u64::MAX);
            if let Err(e) = wire::write_frame(&mut dial.out, &Frame::Clock { unix_ns }) {
                note!("Clock: {e}");
            }
        }
        self.broadcast(&Reply::Booting(format!(
            "the agent said hello: boot {}, kernel {}",
            h.boot_id, h.kernel
        )));
        self.hello = Some(h);
        Flow::Go
    }

    /// What the boot waits on, for the sessions' no-progress detectors.
    fn boot_note(&self) -> String {
        if self.hello.is_none() {
            let (lines, _) = self.console.counts();
            let refused = match &self.refused {
                (n, Some(last)) => {
                    format!("{n} dial(s) of the watch port refused, the last: {last}; ")
                }
                _ => String::new(),
            };
            return match self.console.last_line() {
                Some(line) => {
                    format!("no hello yet; {refused}{lines} console lines, the last: {line}")
                }
                None => format!("no hello yet; {refused}no console line yet"),
            };
        }
        if self.jobs.is_empty() {
            return "the agent said hello; no start job runs".into();
        }
        let jobs = self
            .jobs
            .iter()
            .map(|j| format!("{} {} ({} s)", j.unit, j.state, j.elapsed_ms / 1000))
            .collect::<Vec<_>>()
            .join(", ");
        format!("start jobs: {jobs}")
    }

    fn tick(&mut self, now: Duration) -> Flow {
        let silent: Vec<u64> = self
            .dials
            .iter()
            .filter(|(_, p)| now.saturating_sub(p.since) >= SPAN)
            .map(|(d, _)| *d)
            .collect();
        for dial in silent {
            self.dials.remove(&dial);
            self.refuse(format!("no hello within {} s", SPAN.as_secs()));
        }
        if !self.ever_attached && now >= SPAN {
            return Flow::Exit(format!(
                "no session attached within {} s of the helper's start",
                SPAN.as_secs()
            ));
        }
        if self.stopping.is_some() && now.saturating_sub(self.last_note) >= TICK {
            self.last_note = now;
            for sid in &self.stop_waiters {
                self.reply(*sid, &Reply::Stopping, None);
            }
        }
        match &self.stopping {
            Some(Stopping::Soft {
                since, grace, why, ..
            }) => {
                let last = self
                    .console
                    .last_progress()
                    .map_or(*since, |c| c.max(*since));
                if now.saturating_sub(last) >= *grace {
                    let why = format!(
                        "{why}; the guest did not power off (no new console line for {} s), so \
                         it was stopped hard",
                        grace.as_secs()
                    );
                    return self.hard_stop(now, why);
                }
                return Flow::Go;
            }
            Some(Stopping::Hard { since, .. }) => {
                if now.saturating_sub(*since) >= SPAN {
                    return Flow::Exit(format!(
                        "the hard stop did not finish within {} s; the helper exits, which ends \
                         its VM",
                        SPAN.as_secs()
                    ));
                }
                return Flow::Go;
            }
            None => {}
        }
        if !self.running() {
            return Flow::Go;
        }
        if let Some(fired) = self
            .watch
            .as_ref()
            .and_then(|w| w.check(now, self.console.last_progress()))
        {
            let mut why = fired.why();
            if let (Fired::BootBeforeAgent, (n, Some(last))) = (&fired, &self.refused) {
                why = format!("{why}; {n} dial(s) of the watch port refused, the last: {last}");
            }
            return self.fire(now, why);
        }
        if self.space.due(now) {
            match (self.cfg.free)() {
                Ok(free) => {
                    if let Some(words) = self.space.read(free) {
                        return self.fire(now, words);
                    }
                }
                Err(e) => note!("free space: {e}"),
            }
        }
        if self.phase == Phase::Booting && now.saturating_sub(self.last_note) >= TICK {
            self.last_note = now;
            self.broadcast(&Reply::Booting(self.boot_note()));
        }
        Flow::Go
    }

    fn state(&self, now: Duration) -> StateReport {
        let id = &self.cfg.ident;
        StateReport {
            pid: id.pid,
            uniq: id.uniq,
            helper_sha256: id.helper_sha256.clone(),
            proto: PROTO,
            live_disk: id.live_disk.clone(),
            phase: self.phase,
            keep: self.keep,
            sessions: u32::try_from(self.attached()).unwrap_or(u32::MAX),
            boot_id: self
                .hello
                .as_ref()
                .map(|h| h.boot_id.clone())
                .unwrap_or_default(),
            agent_sha256: self
                .hello
                .as_ref()
                .map(|h| crate::hex(&h.agent_sha256))
                .unwrap_or_default(),
            failed_units: self.failed_units.clone(),
            beat_ms_ago: self
                .watch
                .as_ref()
                .and_then(Watch::last_beat)
                .map(|b| millis(now.saturating_sub(b))),
            free_bytes: self.space.last_bytes(),
            console_tail: self.console.tail(),
        }
    }
}

/// Read one session's requests into `inputs` until it closes.
/// Whether `frame`, a dial's first, is a `Hello` carrying `key`: that hello with its key
/// zeroed, or why not.
///
/// # Errors
/// It is another frame, or a hello with another key.
pub fn keyed_hello(frame: Frame, key: &Key) -> Result<Hello, String> {
    match frame {
        Frame::Hello(mut h) => {
            let keyed = crate::key::same(&h.key, key);
            h.key = [0; crate::key::LEN];
            if keyed {
                Ok(h)
            } else {
                Err(format!(
                    "its hello (agent {}, boot {}) carried another key",
                    crate::hex(&h.agent_sha256),
                    h.boot_id
                ))
            }
        }
        other => Err(format!(
            "its first frame was kind {:#04x}, not the agent's hello",
            other.kind()
        )),
    }
}

/// A watch dial's reader: its first frame must be a keyed hello ([`keyed_hello`]), or the
/// dial is refused and nothing of it is posted; then every frame, until it ends.
fn read_dial(dial: u64, mut reader: UnixStream, key: &Key, inputs: &Sender<Input>) {
    let first = match wire::read_frame(&mut reader) {
        Ok(Some(frame)) => keyed_hello(frame, key),
        Ok(None) => Err("it closed before any hello".into()),
        Err(e) => Err(format!("its first frame: {e}")),
    };
    let hello = match first {
        Ok(hello) => hello,
        Err(why) => {
            let _ = inputs.send(Input::WatchRefused(dial, why));
            return;
        }
    };
    if inputs
        .send(Input::Watch(dial, Frame::Hello(hello)))
        .is_err()
    {
        return;
    }
    loop {
        match wire::read_frame(&mut reader) {
            Ok(Some(frame)) => {
                if inputs.send(Input::Watch(dial, frame)).is_err() {
                    return;
                }
            }
            Ok(None) => {
                let _ = inputs.send(Input::WatchClosed(
                    dial,
                    "the agent closed its watch connection".into(),
                ));
                return;
            }
            Err(e) => {
                let _ = inputs.send(Input::WatchClosed(dial, e.to_string()));
                return;
            }
        }
    }
}

fn read_session(sid: u64, stream: UnixStream, inputs: &Sender<Input>) {
    let mut channel = Channel::new(stream);
    loop {
        match channel.recv_request() {
            Ok(Some(request)) => {
                if inputs.send(Input::Request(sid, request)).is_err() {
                    return;
                }
            }
            Ok(None) => break,
            Err(e) => {
                note!("session {sid}: {e}; closing it");
                break;
            }
        }
    }
    let _ = inputs.send(Input::Closed(sid));
}

/// Serve `listener` (bound at `socket`) with `core` until it exits, and say why. `inputs`
/// and `rx` are the two ends of the channel `core` was built with. The core comes back with
/// its sessions still open, a connection accepted as it exited among them, so the caller
/// removes the record and the socket before any lab sees its session end.
pub fn serve<M: Machine>(
    listener: UnixListener,
    socket: &Path,
    mut core: Core<M>,
    inputs: &Sender<Input>,
    rx: &Receiver<Input>,
) -> (String, Core<M>) {
    let start = Instant::now();
    let done = Arc::new(AtomicBool::new(false));
    let accept = {
        let inputs = inputs.clone();
        let done = Arc::clone(&done);
        let accepted = std::thread::Builder::new()
            .name("vz-accept".into())
            .spawn(move || {
                for (sid, stream) in (1u64..).zip(listener.incoming()) {
                    if done.load(Ordering::SeqCst) {
                        return;
                    }
                    let stream = match stream {
                        Ok(s) => s,
                        Err(e) => {
                            note!("accept: {e}");
                            continue;
                        }
                    };
                    let Ok(out) = stream.try_clone() else {
                        continue;
                    };
                    if inputs.send(Input::Opened(sid, out)).is_err() {
                        return;
                    }
                    let inputs = inputs.clone();
                    let _ = std::thread::Builder::new()
                        .name(format!("vz-session-{sid}"))
                        .spawn(move || read_session(sid, stream, &inputs));
                }
            });
        match accepted {
            Ok(handle) => handle,
            Err(e) => return (format!("the accept thread: {e}"), core),
        }
    };
    {
        let inputs = inputs.clone();
        let done = Arc::clone(&done);
        let _ = std::thread::Builder::new()
            .name("vz-tick".into())
            .spawn(move || {
                while !done.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_secs(1));
                    if inputs.send(Input::Tick).is_err() {
                        return;
                    }
                }
            });
    }
    let why = loop {
        let Ok(input) = rx.recv() else {
            break "the helper's input channel closed".to_string();
        };
        if let Flow::Exit(why) = core.handle(input, start.elapsed()) {
            break why;
        }
    };
    done.store(true, Ordering::SeqCst);
    // Wake the accept thread so it sees `done` (a helper's process ends anyway; the Fake
    // helper's thread must not outlive its test). Once it has ended, a connection it opened
    // behind the input the core exited on (a `down`'s `Stop` as the last session closed)
    // becomes a session, so it ends with the others: its reader thread holds a duplicate,
    // and only a session's shutdown gives its lab EOF.
    if UnixStream::connect(socket).is_ok() {
        let _ = accept.join();
        while let Ok(input) = rx.try_recv() {
            if matches!(input, Input::Opened(..)) {
                let _ = core.handle(input, start.elapsed());
            }
        }
    }
    (why, core)
}

/// The run files a helper holds for its whole life.
#[derive(Debug)]
pub struct Held {
    /// The live disk's lock.
    pub lock: DiskLock,
    /// The control socket.
    pub listener: UnixListener,
    /// Where it is bound.
    pub socket: PathBuf,
    /// The record written.
    pub record: Record,
}

/// Take the run files at `paths` for the helper `ident`, in §5.1's order: the live disk's
/// `flock` (a second helper for the disk fails here, naming the holder from the record,
/// before any framework sees the disk), then the control socket (a dead helper's is
/// removed first), then the record, by rename.
///
/// # Errors
/// The lock is held, or a run file could not be made.
pub fn take(paths: &Paths, ident: &Ident, pids: &dyn Pids) -> Result<Held, String> {
    let socket = paths.checked_socket()?;
    let run = paths.run_dir();
    std::fs::create_dir_all(&run).map_err(|e| format!("{}: {e}", run.display()))?;
    let lock_path = paths.disk_lock();
    if let Some(dir) = lock_path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let lock = match DiskLock::try_take(&lock_path) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            let holder = match crate::record::holder(paths, pids) {
                Holder::Live(r) => format!(
                    "helper {} (unique id {}, started {})",
                    r.pid, r.uniq, r.started
                ),
                Holder::Unproven(r) => format!(
                    "helper {} (unique id {}, started {}), which an earlier build started (its \
                     record names no start time; `down` asks it to stop)",
                    r.pid, r.uniq, r.started
                ),
                Holder::Dead(r) => format!(
                    "a process whose record names helper {}, which is gone",
                    r.pid
                ),
                Holder::None => "a process with no record yet".into(),
            };
            return Err(format!(
                "{} is held by {holder}: a second helper for the same disk does not start",
                paths.live_disk().display()
            ));
        }
        Err(e) => return Err(format!("{}: {e}", lock_path.display())),
    };
    match std::fs::remove_file(&socket) {
        Ok(()) => note!("removed a dead helper's socket {}", socket.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", socket.display())),
    }
    let listener =
        UnixListener::bind(&socket).map_err(|e| format!("bind {}: {e}", socket.display()))?;
    let record = Record {
        pid: ident.pid,
        uniq: ident.uniq,
        birth_us: ident.birth_us,
        helper_sha256: ident.helper_sha256.clone(),
        proto: PROTO,
        live_disk: ident.live_disk.clone(),
        started: aterm_time::unix_secs().unwrap_or(0),
        boot_id: None,
        agent_sha256: None,
    };
    record
        .write(&paths.record())
        .map_err(|e| format!("the record {}: {e}", paths.record().display()))?;
    Ok(Held {
        lock,
        listener,
        socket,
        record,
    })
}

/// Where the previous boot's console is kept: `console.log`'s `console.1.log`.
#[must_use]
pub fn previous_console(console_log: &Path) -> PathBuf {
    console_log.with_extension("1.log")
}

/// A helper's run files at its exit: the record and the socket only while the record is
/// still `record`'s (a lab that saw its session end reads no record of this helper), and
/// the key disk whoever's record is there, since this helper wrote it under the lock.
pub fn release_run_files(paths: &Paths, record: &Record, socket: &Path) {
    let ours = Record::read(&paths.record()).is_some_and(|r| {
        r.pid == record.pid && r.uniq == record.uniq && r.birth_us == record.birth_us
    });
    if ours {
        let _ = std::fs::remove_file(paths.record());
        let _ = std::fs::remove_file(socket);
    }
    let _ = std::fs::remove_file(paths.key_disk());
}

/// This process, as a helper's [`Ident`].
#[must_use]
pub fn this_process(helper_sha256: String, live_disk: PathBuf) -> Ident {
    let pid = std::process::id();
    Ident {
        pid,
        uniq: Kernel.uniq(pid).unwrap_or(0),
        birth_us: Kernel.birth_us(pid).unwrap_or(0),
        helper_sha256,
        live_disk,
    }
}

/// A helper's whole life over `paths` with `machine`: [`take`], [`serve`], then the record
/// and the socket removed (only while the record is still this helper's) and the lock let
/// go. Says why it ended.
///
/// # Errors
/// The run files could not be taken ([`take`]'s words).
pub fn run<M: Machine>(
    paths: &Paths,
    machine: M,
    ident: Ident,
    hold_conns: bool,
) -> Result<String, String> {
    let held = take(paths, &ident, &Kernel)?;
    note!(
        "pid {} (unique id {}), helper {}, serving {}",
        ident.pid,
        ident.uniq,
        ident.helper_sha256,
        held.socket.display()
    );
    let live_disk = ident.live_disk.clone();
    let (inputs, rx) = std::sync::mpsc::channel();
    let cfg = Config {
        ident,
        record: Some((paths.record(), held.record.clone())),
        hold_conns,
        key_disk: paths.key_disk(),
        free: Box::new(move || crate::space::read(&live_disk)),
    };
    let core = Core::new(machine, cfg, inputs.clone());
    let Held {
        lock,
        listener,
        socket,
        record,
    } = held;
    let (why, core) = serve(listener, &socket, core, &inputs, &rx);
    note!("exiting: {why}");
    // The run files go first, then the sessions (their EOF), then the lock: a lab that saw
    // its session end reads no record of this helper, and waits for its pid.
    release_run_files(paths, &record, &socket);
    drop(core);
    drop(lock);
    Ok(why)
}

#[cfg(test)]
#[path = "helper_tests.rs"]
mod tests;
