// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lab's side of the helper (`docs/DESIGN-vz-guest-2026-10-23.md` §5.1): a [`Session`]
//! on the control socket, and [`Lab`], which finds the live helper by its record or starts
//! one detached, replaces one from another checkout, removes a dead one's leftovers, and
//! escalates against one that does not answer: `SIGKILL` by the record's pid, only once its
//! unique id is read again and matches.
//!
//! Every wait on the helper is a one-minute no-progress detector ([`crate::detect::SPAN`]):
//! the helper sends `Booting` every 5 s while the boot runs and `Stopping` every 5 s while a
//! stop runs, so a wait that hears nothing for the span is a helper that does not answer.
//!
//! How a [`Lab`] starts a helper is its [`Start`]: [`Detached`] runs `aterm-vz-host` in a
//! session of its own; the Fake ([`fake::Threaded`]) runs the same core with the Fake
//! machine on a thread of the test.

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_vz_agent::wire::Hello;

use crate::detect::SPAN;
use crate::plan::{Plan, ShareEntry};
use crate::proto::{Attach, Phase, ProtoError, Reply, Request, StateReport};
use crate::record::{Holder, Kernel, Paths, Pids, Record};
use crate::transport::Channel;

/// Why a session's request did not get its answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientError {
    /// The control socket failed, or a message did not read.
    Io(String),
    /// The helper sent nothing for the span: what it did not answer, with the escalation's
    /// outcome once one ran.
    Hung(String),
    /// The helper refused the session (another protocol, helper or agent, or a VM that is
    /// stopping): the lab replaces it.
    Refused(String),
    /// The guest stopped, stopped answering, or the helper stopped it.
    Lost {
        /// Why.
        why: String,
        /// The console tail.
        tail: String,
    },
    /// A framework error.
    Failed {
        /// The error.
        error: String,
        /// The console tail.
        tail: String,
    },
    /// The helper answered the request with an error.
    Error(String),
    /// The helper ended the session (it exited).
    Closed,
    /// A helper could not be started.
    Start(String),
    /// No live helper runs the VM, for a verb that never starts one ([`Lab::join`]).
    NoHelper(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(why) | Self::Hung(why) | Self::Error(why) => f.write_str(why),
            Self::Refused(why) => write!(f, "the helper refused the session: {why}"),
            Self::Lost { why, .. } => f.write_str(why),
            Self::Failed { error, .. } => f.write_str(&not_run_words(error)),
            Self::Closed => f.write_str("the helper ended the session"),
            Self::Start(why) => write!(f, "the helper did not start: {why}"),
            Self::NoHelper(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for ClientError {}

/// How a run that did not run begins its words.
pub const NOT_RUN: &str = "NOT RUN — ";

/// `words` as the run reports them: NOT RUN once, whether or not the helper's own words
/// (a detector's, a refusal's) already begin with it.
#[must_use]
pub fn not_run_words(words: &str) -> String {
    if words.starts_with(NOT_RUN) {
        words.to_string()
    } else {
        format!("{NOT_RUN}{words}")
    }
}

fn read_error(e: ProtoError, what: &str, span: Duration) -> ClientError {
    match e {
        ProtoError::Io(io)
            if matches!(
                io.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            ClientError::Hung(format!(
                "the helper did not answer {what} for {} s",
                span.as_secs_f64()
            ))
        }
        ProtoError::Io(io) if helper_gone(&io) => ClientError::Closed,
        other => ClientError::Io(other.to_string()),
    }
}

/// Whether a send or read failed because the helper's side of the socket is gone: a closed
/// pipe, a reset, or a socket no longer connected (`down` racing a helper that had just found
/// no session left read ENOTCONN).
fn helper_gone(io: &std::io::Error) -> bool {
    matches!(
        io.kind(),
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::NotConnected
    )
}

/// A fresh control connection's refused read span. A helper that closed the connection
/// before the span was set (its exit drops the connection its accept thread took, and its
/// listener aborts the ones still queued) has ended the session, and XNU refuses every
/// option on a socket that can neither send nor receive (`sosetopt`: EINVAL): that is
/// `Closed`, as a refused `connect` is. The span is never zero (std refuses one itself,
/// with no errno) and a bad `timeval` is EDOM, so no other refusal reads as EINVAL here.
fn span_error(e: std::io::Error) -> ClientError {
    if e.raw_os_error() == Some(libc::EINVAL) {
        ClientError::Closed
    } else {
        ClientError::Io(e.to_string())
    }
}

/// A helper a [`Lab`] found dead or stopped before it started its own: what a start that
/// meets the framework's storage refusal names (§5.1, fact 44).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replaced {
    /// Its record.
    pub record: Record,
    /// When this lab found it dead or saw it stop (Unix seconds).
    pub at: u64,
}

/// The run's words for the framework error `error` a `Failed` carried, where the lab knows
/// more than the framework's text says (§5.1):
/// - the entitlement's refusal (fact 33): NOT RUN, naming the helper `exe` that lacks it and
///   what signs it;
/// - the storage attachment's refusal after a free `flock`, when this lab replaced a helper
///   (`replaced`): NOT RUN, naming that helper, whose VM process still holds the disk in
///   the half second after it ended (fact 44). Nothing retries.
///
/// Any other error is NOT RUN with the framework's own words.
#[must_use]
pub fn failed_words(error: &str, exe: &Path, replaced: Option<&Replaced>) -> String {
    let bare = error.strip_prefix(NOT_RUN).unwrap_or(error);
    if error.contains("com.apple.security.virtualization") {
        return format!(
            "NOT RUN — {} lacks the com.apple.security.virtualization entitlement, so \
             Virtualization.framework refused the VM ({bare}); the lab's placing signs the \
             copy under vz.noindex/host/ with crates/aterm-vz/{}",
            exe.display(),
            crate::sign::ENTITLEMENTS_FILE
        );
    }
    if error.contains("storage device attachment is invalid")
        && let Some(r) = replaced
    {
        let disk = r.record.live_disk.file_name().map_or_else(
            || r.record.live_disk.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        return format!(
            "NOT RUN — {disk} is still attached to the VM of helper {} (unique id {}), which \
             ended by {} (Unix seconds); its VM process exits within a second (fact 44): {bare}",
            r.record.pid, r.record.uniq, r.at
        );
    }
    not_run_words(error)
}

/// One session on the helper's control socket.
#[derive(Debug)]
pub struct Session {
    chan: Channel,
    span: Duration,
    /// Where the VM was at `Attach`, and since.
    pub phase: Phase,
    /// `keep` after this session's `Attach`.
    pub keep: bool,
    /// Whether the `Attach` swapped the `aterm` share (the lab asks the agent to rebind).
    pub swapped: bool,
    /// The agent's hello, once the VM is `Ready`.
    pub hello: Option<Hello>,
    /// The `Lost` words this session heard, whichever request's wait read them: a command's
    /// `Release` after its connection ended reads the `Lost` a detector broadcast, and
    /// [`Session::why_lost`] still names it (V4, measured).
    lost: Option<String>,
}

impl Session {
    /// Open a session on `stream` with `attach`, waiting at most `span` for each answer.
    ///
    /// # Errors
    /// The helper refused it, did not answer, or the socket failed.
    pub fn attach(
        stream: UnixStream,
        attach: &Attach,
        span: Duration,
    ) -> Result<Self, ClientError> {
        let chan = Channel::new(stream);
        chan.set_span(Some(span)).map_err(span_error)?;
        let mut session = Self {
            chan,
            span,
            phase: Phase::Absent,
            keep: false,
            swapped: false,
            hello: None,
            lost: None,
        };
        session.send(&Request::Attach(attach.clone()))?;
        session
            .until("Attach", &mut |_| {}, |reply, _| match reply {
                Reply::Attached {
                    phase,
                    keep,
                    swapped,
                } => Some(Ok((phase, keep, swapped))),
                Reply::Refused(why) => Some(Err(ClientError::Refused(why))),
                _ => None,
            })
            .map(|(phase, keep, swapped)| {
                session.phase = phase;
                session.keep = keep;
                session.swapped = swapped;
                session
            })
    }

    fn send(&self, request: &Request) -> Result<(), ClientError> {
        self.chan.send_request(request).map_err(|e| match e {
            ProtoError::Io(io) if helper_gone(&io) => ClientError::Closed,
            other => ClientError::Io(other.to_string()),
        })
    }

    /// Read replies until `want` takes one, handing the progress ones to `progress`; `Lost`,
    /// `Failed` and `Error` end the wait.
    fn until<T>(
        &mut self,
        what: &str,
        progress: &mut dyn FnMut(&str),
        mut want: impl FnMut(Reply, Option<OwnedFd>) -> Option<Result<T, ClientError>>,
    ) -> Result<T, ClientError> {
        loop {
            let (reply, fd) = match self.chan.recv_reply() {
                Ok(Some(got)) => got,
                Ok(None) => return Err(ClientError::Closed),
                Err(e) => return Err(read_error(e, what, self.span)),
            };
            match &reply {
                Reply::Lost { why, tail } => {
                    self.lost = Some(why.clone());
                    return Err(ClientError::Lost {
                        why: why.clone(),
                        tail: tail.clone(),
                    });
                }
                Reply::Failed { error, tail } => {
                    return Err(ClientError::Failed {
                        error: error.clone(),
                        tail: tail.clone(),
                    });
                }
                Reply::Error(why) => return Err(ClientError::Error(why.clone())),
                Reply::Started => {
                    if self.phase == Phase::Absent {
                        self.phase = Phase::Booting;
                    }
                    progress("the VM started");
                }
                Reply::Booting(note) => progress(note),
                Reply::Ready(hello) => {
                    self.phase = Phase::Ready;
                    self.hello = Some(hello.clone());
                }
                _ => {}
            }
            if let Some(done) = want(reply, fd) {
                return done;
            }
        }
    }

    /// Boot the VM with `plan` unless it runs already, and wait for the agent's `Ready`,
    /// handing each progress note to `progress`.
    ///
    /// # Errors
    /// The boot failed, was lost (a detector fired: its words), or the helper did not answer.
    pub fn boot(
        &mut self,
        plan: &Plan,
        progress: &mut dyn FnMut(&str),
    ) -> Result<Hello, ClientError> {
        if let Some(h) = &self.hello {
            return Ok(h.clone());
        }
        if self.phase == Phase::Absent {
            self.send(&Request::Boot(Box::new(plan.clone())))?;
        }
        self.wait_ready(progress)
    }

    /// `up`'s order (§4, §5.1): wait for the agent's `Ready`, booting with the plan `prepare`
    /// makes only when this session found no VM (`Absent`). A VM that runs, or that another
    /// session is booting, keeps the disks it booted on: nothing prepares, unlinks or
    /// replaces them under it, so `prepare` (which removes every `*.partial.img`) runs only
    /// while this session's helper holds the disk's `flock` and has started no VM.
    ///
    /// # Errors
    /// `prepare` failed (nothing was booted; the session's close ends the helper), or as
    /// [`Session::boot`].
    pub fn bring_up<E>(
        &mut self,
        prepare: impl FnOnce() -> Result<Plan, E>,
        progress: &mut dyn FnMut(&str),
    ) -> Result<Hello, UpError<E>> {
        if self.hello.is_none() && self.phase == Phase::Absent {
            let plan = prepare().map_err(UpError::Prepare)?;
            self.boot(&plan, progress).map_err(UpError::Client)
        } else {
            self.wait_ready(progress).map_err(UpError::Client)
        }
    }

    /// Wait for the agent's `Ready`.
    ///
    /// # Errors
    /// As [`Session::boot`].
    pub fn wait_ready(&mut self, progress: &mut dyn FnMut(&str)) -> Result<Hello, ClientError> {
        if let Some(h) = &self.hello {
            return Ok(h.clone());
        }
        // A session that attached to a Ready VM hears its `Ready` right after `Attached`.
        self.until("the boot", progress, |reply, _| match reply {
            Reply::Ready(h) => Some(Ok(h)),
            _ => None,
        })
    }

    /// A connection to the guest's `port`: its id, for [`Session::release`], and its
    /// descriptor.
    ///
    /// # Errors
    /// The helper refused it (its words) or did not answer.
    pub fn connect(&mut self, port: u32) -> Result<(u64, OwnedFd), ClientError> {
        self.send(&Request::Connect { port })?;
        self.until("Connect", &mut |_| {}, |reply, fd| match (reply, fd) {
            (Reply::Connected { id, .. }, Some(fd)) => Some(Ok((id, fd))),
            _ => None,
        })
    }

    fn done(&mut self, request: &Request, what: &str) -> Result<(), ClientError> {
        self.send(request)?;
        self.until(what, &mut |_| {}, |reply, _| {
            matches!(reply, Reply::Done).then_some(Ok(()))
        })
    }

    /// The lab closed connection `id`: the helper lets its object go.
    ///
    /// # Errors
    /// The helper did not answer.
    pub fn release(&mut self, id: u64) -> Result<(), ClientError> {
        self.done(&Request::Release { id }, "Release")
    }

    /// Swap the running `aterm` share to `entries`.
    ///
    /// # Errors
    /// The helper could not (its words) or did not answer.
    pub fn share(&mut self, entries: &[ShareEntry]) -> Result<(), ClientError> {
        self.done(&Request::Share(entries.to_vec()), "Share")
    }

    /// Set or clear `keep`.
    ///
    /// # Errors
    /// The helper did not answer.
    pub fn keep(&mut self, keep: bool) -> Result<(), ClientError> {
        self.done(&Request::Keep(keep), "Keep")?;
        self.keep = keep;
        Ok(())
    }

    /// The VM's state.
    ///
    /// # Errors
    /// The helper did not answer.
    pub fn state(&mut self) -> Result<StateReport, ClientError> {
        self.send(&Request::State)?;
        self.until("State", &mut |_| {}, |reply, _| match reply {
            Reply::State(s) => Some(Ok(*s)),
            _ => None,
        })
    }

    /// Wait for the VM to stop with no request of this session's (a guest asked to power
    /// off on a command connection, the bake's end): the helper's `Lost` once the VM
    /// stopped, whose words say how. A guest that powered itself off reads
    /// [`crate::helper::POWERED_OFF`]; a detector that fired, a `Stop` or a hard stop reads
    /// otherwise.
    ///
    /// # Errors
    /// The VM failed, the helper ended the session with no `Lost`, or it said nothing for the
    /// span.
    pub fn wait_stopped(&mut self) -> Result<String, ClientError> {
        let ended: Result<(), ClientError> =
            self.until("the guest's power-off", &mut |_| {}, |_, _| None);
        match ended {
            Err(ClientError::Lost { why, .. }) => Ok(why),
            Err(e) => Err(e),
            Ok(()) => Err(ClientError::Closed),
        }
    }

    /// Why a command connection of this session ended before its `Exit`: the session's
    /// `Lost` (heard now, or by an earlier request), or the VM's phase. A VM that is stopping
    /// is waited for, as
    /// [`Session::wait_stopped`] waits (a one-minute hang detector), so the words are the
    /// stop's own (the detector that fired, a `Stop`, the guest's power-off), never only
    /// "stopping": a guest that stopped answering under a command read "the VM is stopping"
    /// where §10 names the missed heartbeat (V4, measured).
    pub fn why_lost(&mut self) -> String {
        if let Some(why) = &self.lost {
            return why.clone();
        }
        match self.state() {
            Err(ClientError::Lost { why, .. }) => why,
            Err(ClientError::Closed) => "the helper ended the session".into(),
            Err(e) => e.to_string(),
            Ok(s) if s.phase == Phase::Stopping => match self.wait_stopped() {
                Ok(why) => why,
                Err(e) => format!("the VM was stopping, and then {e}"),
            },
            Ok(s) if s.phase != Phase::Ready => format!("the VM is {}", s.phase.name()),
            Ok(_) => "the agent closed the command connection before its Exit (the VM is still \
                      ready)"
                .into(),
        }
    }

    /// Close the session; the release rule runs (§5.1).
    pub fn detach(self) {
        let _ = self.send(&Request::Detach);
    }
}

/// Why [`Session::bring_up`] brought no VM up.
#[derive(Debug, PartialEq, Eq)]
pub enum UpError<E> {
    /// The plan could not be made (the disks, the kernel, the seed).
    Prepare(E),
    /// The session's boot or wait failed.
    Client(ClientError),
}

/// How a [`Lab`] starts, watches and kills a helper.
pub trait Start {
    /// Start a helper serving `paths`, not waiting for it: its pid.
    ///
    /// # Errors
    /// It could not be started.
    fn start(&mut self, paths: &Paths) -> Result<u32, String>;

    /// Whether the helper last started has exited, and how.
    fn exited(&mut self) -> Option<String>;

    /// The helper last started wrote its record: it is on its own now.
    fn settled(&mut self);

    /// Whether the helper `record` names may still run, for a wait on its end
    /// ([`crate::record::may_run`]), never for a signal.
    fn alive(&self, record: &Record) -> bool;

    /// `SIGKILL` the helper `record` names, its unique id read again and matched first.
    ///
    /// # Errors
    /// It was not signalled (the words say why).
    fn kill(&mut self, record: &Record) -> Result<(), String>;

    /// The process table records are read against.
    fn pids(&self) -> &dyn Pids;
}

/// The real [`Start`]: `aterm-vz-host serve <state>` in a session of its own, stdin
/// `/dev/null`, stdout and stderr appended to `run/helper.log`, never waited for: dropping
/// its `Child` hands the pid to aterm-uds' reaper, never a kill, so it outlives the lab.
#[derive(Debug)]
pub struct Detached {
    exe: PathBuf,
    child: Option<aterm_uds::spawnfd::Child>,
}

impl Detached {
    /// Start the helper at `exe` (its signed, content-addressed copy, [`crate::sign`]).
    #[must_use]
    pub const fn new(exe: PathBuf) -> Self {
        Self { exe, child: None }
    }
}

impl Start for Detached {
    fn start(&mut self, paths: &Paths) -> Result<u32, String> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let run = paths.run_dir();
        std::fs::create_dir_all(&run).map_err(|e| format!("{}: {e}", run.display()))?;
        let log_path = paths.helper_log();
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&log_path)
            .map_err(|e| format!("{}: {e}", log_path.display()))?;
        let err = log
            .try_clone()
            .map_err(|e| format!("{}: {e}", log_path.display()))?;
        crate::sign::mark_started(&self.exe);
        let mut cmd = aterm_uds::spawnfd::command(&self.exe);
        cmd.arg("serve")
            .arg(paths.root())
            .setsid(true)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(log)
            .stderr(err);
        let child = cmd
            .spawn()
            .map_err(|e| format!("{}: {e}", self.exe.display()))?;
        let pid = child.id();
        self.child = Some(child);
        Ok(pid)
    }

    fn exited(&mut self) -> Option<String> {
        let child = self.child.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => {
                self.child = None;
                Some(format!("it exited ({status})"))
            }
            _ => None,
        }
    }

    fn settled(&mut self) {
        self.child = None;
    }

    fn alive(&self, record: &Record) -> bool {
        crate::record::may_run(record, &Kernel)
    }

    fn kill(&mut self, record: &Record) -> Result<(), String> {
        crate::record::kill_helper(record, &Kernel)
    }

    fn pids(&self) -> &dyn Pids {
        &Kernel
    }
}

/// The lab's helper: the `Helper` port (§2), over the run files at `paths`.
#[derive(Debug)]
pub struct Lab<S: Start> {
    /// The run files.
    pub paths: Paths,
    /// How a helper is started.
    pub start: S,
    /// How long a helper may answer nothing before it is hung: [`SPAN`] (the tests' hung
    /// stand-ins use less).
    pub span: Duration,
    /// How long a helper that was asked to end (a `Stop`, its session's EOF) may take before
    /// it is escalated: [`SPAN`] (the tests' stand-ins, which never end, use less). The wait
    /// for its end after a SIGKILL is always [`SPAN`].
    pub end_span: Duration,
    /// The helper the last [`Self::session`] found dead or stopped before it started one.
    pub replaced: Option<Replaced>,
}

/// How long to wait between two reads of a file or a pid inside a no-progress wait.
const POLL: Duration = Duration::from_millis(10);

impl<S: Start> Lab<S> {
    /// A lab over `paths` that starts helpers with `start`.
    pub const fn new(paths: Paths, start: S) -> Self {
        Self {
            paths,
            start,
            span: SPAN,
            end_span: SPAN,
            replaced: None,
        }
    }

    /// A session for `attach`, on the live helper, or on one this starts: a dead helper's
    /// leftovers are removed first, a helper an earlier build's record names is asked to
    /// `Stop` and replaced ([`Self::stop_unproven`]), and a helper that refuses the session
    /// (another checkout's) is stopped as `down` would and replaced (§5.1).
    ///
    /// # Errors
    /// No helper could be started, or one did not answer (NOT RUN, naming it, once it was
    /// SIGKILLed), or the socket failed.
    pub fn session(&mut self, attach: &Attach) -> Result<Session, ClientError> {
        let mut replaced = false;
        self.replaced = None;
        for _ in 0..3 {
            let record = match crate::record::holder(&self.paths, self.start.pids()) {
                Holder::Live(r) => r,
                Holder::Dead(r) => {
                    crate::record::remove_leftovers(&self.paths, &r);
                    self.replaced = Some(Replaced {
                        record: r,
                        at: now_s(),
                    });
                    self.start_one()?
                }
                Holder::Unproven(r) => {
                    self.stop_unproven(&r)?;
                    self.replaced = Some(Replaced {
                        record: r,
                        at: now_s(),
                    });
                    self.start_one()?
                }
                Holder::None => self.start_one()?,
            };
            match self.attach_to(&record, attach) {
                Ok(session) => return Ok(session),
                Err(ClientError::Refused(why)) if !replaced => {
                    replaced = true;
                    self.stop_record(&record)
                        .map_err(|e| ClientError::Refused(format!("{why}; replacing it: {e}")))?;
                    self.replaced = Some(Replaced {
                        record,
                        at: now_s(),
                    });
                }
                Err(ClientError::Closed) => self.ended_or_killed(&record, "its session closed")?,
                Err(ClientError::Hung(what)) => return Err(self.escalate(&record, &what)),
                Err(e) => return Err(e),
            }
        }
        Err(ClientError::Start(
            "three helpers in a row ended before a session opened".into(),
        ))
    }

    /// A session on the live helper only, for a verb on a VM another verb brought up (`exec`
    /// under `up`): nothing is started, stopped or replaced, so a refusal (another checkout's
    /// helper or agent) is this run's to report, and the VM runs on. A helper that does not
    /// answer `Attach` is escalated (§5.1).
    ///
    /// # Errors
    /// No live helper, a refusal, a helper that did not answer (NOT RUN, naming it, once it
    /// was SIGKILLed), or the socket failed.
    pub fn join(&mut self, attach: &Attach) -> Result<Session, ClientError> {
        let record = match crate::record::holder(&self.paths, self.start.pids()) {
            Holder::Live(record) => record,
            Holder::Unproven(r) => {
                return Err(ClientError::NoHelper(format!(
                    "the VM at {} is helper {}'s, which an earlier build started (its record \
                     names no start time): `down` asks it to stop",
                    self.paths.root().display(),
                    r.pid
                )));
            }
            Holder::Dead(_) | Holder::None => {
                return Err(ClientError::NoHelper(format!(
                    "no helper runs the VM at {}",
                    self.paths.root().display()
                )));
            }
        };
        match self.attach_to(&record, attach) {
            Ok(session) => Ok(session),
            Err(ClientError::Hung(what)) => Err(self.escalate(&record, &what)),
            Err(e) => Err(e),
        }
    }

    fn attach_to(&self, record: &Record, attach: &Attach) -> Result<Session, ClientError> {
        let socket = self.paths.checked_socket().map_err(ClientError::Io)?;
        let stream = UnixStream::connect(&socket).map_err(|e| match e.kind() {
            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound => {
                ClientError::Closed
            }
            _ => ClientError::Io(format!(
                "helper {}'s socket {}: {e}",
                record.pid,
                socket.display()
            )),
        })?;
        Session::attach(stream, attach, self.span)
    }

    /// Start a helper and wait for its record: a one-minute hang detector, cut short by the
    /// helper's exit (a held disk among the reasons, in its log's words).
    fn start_one(&mut self) -> Result<Record, ClientError> {
        let pid = self.start.start(&self.paths).map_err(ClientError::Start)?;
        let begun = Instant::now();
        loop {
            if let Some(r) = Record::read(&self.paths.record())
                && r.pid == pid
            {
                self.start.settled();
                return Ok(r);
            }
            if let Some(how) = self.start.exited() {
                return Err(ClientError::Start(format!(
                    "helper {pid} {how} before it wrote its record: {}",
                    log_tail(&self.paths.helper_log())
                )));
            }
            if begun.elapsed() >= self.span {
                return Err(ClientError::Start(format!(
                    "helper {pid} wrote no record within {} s",
                    self.span.as_secs_f64()
                )));
            }
            std::thread::sleep(POLL);
        }
    }

    /// Wait at most `span` for the helper `record` names to end.
    fn wait_gone(&self, record: &Record, span: Duration) -> Result<(), ClientError> {
        let begun = Instant::now();
        while self.start.alive(record) {
            if begun.elapsed() >= span {
                return Err(ClientError::Hung(format!(
                    "helper {} did not exit within {} s",
                    record.pid,
                    span.as_secs_f64()
                )));
            }
            std::thread::sleep(POLL);
        }
        Ok(())
    }

    /// Wait for the helper `record` names to end once `after` (its `Stop` was answered, its
    /// session closed): one still alive after [`Self::end_span`] did not answer, and is
    /// escalated (§5.1), so no wait on a helper ends with it running and its record kept.
    fn ended_or_killed(&mut self, record: &Record, after: &str) -> Result<(), ClientError> {
        match self.wait_gone(record, self.end_span) {
            Err(ClientError::Hung(_)) => {
                let what = format!(
                    "{after}, and the helper did not exit within {} s",
                    self.end_span.as_secs_f64()
                );
                Err(self.escalate(record, &what))
            }
            other => other,
        }
    }

    /// SIGKILL the helper `record` names, which did not answer `what`, and say so as the run
    /// reports it. The wait for its end is a one-minute hang detector, whatever the spans
    /// are, since a SIGKILLed process's end must come.
    fn escalate(&mut self, record: &Record, what: &str) -> ClientError {
        let outcome = match self.start.kill(record) {
            Ok(()) => match self.wait_gone(record, SPAN) {
                Ok(()) => {
                    crate::record::remove_leftovers(&self.paths, record);
                    "it was SIGKILLed by its pid, its unique id checked".to_string()
                }
                Err(e) => format!("it was SIGKILLed by its pid, and {e}"),
            },
            Err(e) => format!("it was not signalled: {e}"),
        };
        ClientError::Hung(format!(
            "{NOT_RUN}{what}: helper {} (unique id {}, started {}); {outcome}",
            record.pid, record.uniq, record.started
        ))
    }

    /// `Stop` the helper an earlier build's record names (no start time, so it is never
    /// signalled: [`crate::record::Holder::Unproven`]) and wait for its unique id to leave its
    /// pid; its run files go once it has. A socket nobody listens on means that helper is
    /// gone, or the record is an earlier boot's: its run files go at once.
    ///
    /// # Errors
    /// It did not answer `Stop`, or did not end within [`Self::end_span`] once it had (NOT
    /// RUN, naming it: it was not signalled), or the socket failed.
    fn stop_unproven(&mut self, record: &Record) -> Result<(), ClientError> {
        let unsignalled = |what: &str| {
            ClientError::Hung(format!(
                "{NOT_RUN}{what}: helper {} (unique id {}, started {}), which an earlier build \
                 started; its record names no start time, so it was not signalled",
                record.pid, record.uniq, record.started
            ))
        };
        match stop(&self.paths, STOP_GRACE_MS, self.span) {
            Ok(()) => {}
            Err(ClientError::Closed) => {
                crate::record::remove_leftovers(&self.paths, record);
                return Ok(());
            }
            Err(ClientError::Hung(what)) => return Err(unsignalled(&what)),
            Err(e) => return Err(e),
        }
        match self.wait_gone(record, self.end_span) {
            Ok(()) => {
                crate::record::remove_leftovers(&self.paths, record);
                Ok(())
            }
            Err(ClientError::Hung(what)) => Err(unsignalled(&what)),
            Err(e) => Err(e),
        }
    }

    /// `Stop` the helper `record` names and wait for it to end, escalating as §5.1 says.
    fn stop_record(&mut self, record: &Record) -> Result<(), ClientError> {
        match stop(&self.paths, STOP_GRACE_MS, self.span) {
            Ok(()) => self.ended_or_killed(record, "it answered Stop"),
            Err(ClientError::Hung(what)) => Err(self.escalate(record, &what)),
            Err(ClientError::Closed) => self.ended_or_killed(record, "its socket closed on Stop"),
            Err(e) => Err(e),
        }
    }

    /// `down`: stop the live helper's VM and wait for the helper to end; a dead helper's
    /// leftovers are removed, and a helper an earlier build's record names is asked to `Stop`
    /// ([`Self::stop_unproven`]). Nothing is started.
    ///
    /// # Errors
    /// The helper did not answer (NOT RUN, naming it, once it was SIGKILLed).
    pub fn down(&mut self) -> Result<(), ClientError> {
        match crate::record::holder(&self.paths, self.start.pids()) {
            Holder::Live(r) => self.stop_record(&r),
            Holder::Unproven(r) => self.stop_unproven(&r),
            Holder::Dead(r) => {
                crate::record::remove_leftovers(&self.paths, &r);
                Ok(())
            }
            Holder::None => Ok(()),
        }
    }

    /// `status`: the live helper's state, or `None` with no live helper. Nothing is started,
    /// no session opens, and nothing is signalled: §5.1 escalates only an unanswered
    /// `Attach` or `Stop`, and a `State` may wait behind the helper's own one-minute waits.
    ///
    /// # Errors
    /// The helper did not answer (NOT RUN, naming it; it runs on).
    pub fn status(&mut self) -> Result<Option<StateReport>, ClientError> {
        let (Holder::Live(r) | Holder::Unproven(r)) =
            crate::record::holder(&self.paths, self.start.pids())
        else {
            return Ok(None);
        };
        match state(&self.paths, self.span) {
            Ok(s) => Ok(Some(s)),
            Err(ClientError::Closed) => Ok(None),
            Err(ClientError::Hung(what)) => Err(ClientError::Hung(format!(
                "{NOT_RUN}{what}: helper {} (unique id {}, started {}); status signals nothing",
                r.pid, r.uniq, r.started
            ))),
            Err(e) => Err(e),
        }
    }
}

/// Now, in Unix seconds.
fn now_s() -> u64 {
    aterm_time::unix_secs().unwrap_or(0)
}

/// `Stop`'s grace from `down`: one minute.
const STOP_GRACE_MS: u64 = 60_000;

fn connect(paths: &Paths, span: Duration) -> Result<Channel, ClientError> {
    let socket = paths.checked_socket().map_err(ClientError::Io)?;
    let stream = UnixStream::connect(&socket).map_err(|e| match e.kind() {
        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound => ClientError::Closed,
        _ => ClientError::Io(format!("{}: {e}", socket.display())),
    })?;
    let chan = Channel::new(stream);
    chan.set_span(Some(span)).map_err(span_error)?;
    Ok(chan)
}

/// Send `Stop{grace}` on a connection of its own (no session: the frozen kinds) and wait for
/// the helper to end it: `Stopping`, again every 5 s, then EOF.
///
/// # Errors
/// The helper did not answer for the span, or the socket failed.
pub fn stop(paths: &Paths, grace_ms: u64, span: Duration) -> Result<(), ClientError> {
    let mut chan = connect(paths, span)?;
    chan.send_request(&Request::Stop { grace_ms })
        .map_err(|e| read_error(e, "Stop", span))?;
    loop {
        match chan.recv_reply() {
            Ok(None) => return Ok(()),
            Ok(Some(_)) => {}
            Err(e) => {
                return match read_error(e, "Stop", span) {
                    ClientError::Closed => Ok(()),
                    other => Err(other),
                };
            }
        }
    }
}

/// `State` on a connection of its own.
///
/// # Errors
/// The helper did not answer for the span, or the socket failed.
pub fn state(paths: &Paths, span: Duration) -> Result<StateReport, ClientError> {
    let mut chan = connect(paths, span)?;
    chan.send_request(&Request::State)
        .map_err(|e| read_error(e, "State", span))?;
    loop {
        match chan.recv_reply() {
            Ok(Some((Reply::State(s), _))) => return Ok(*s),
            Ok(Some(_)) => {}
            Ok(None) => return Err(ClientError::Closed),
            Err(e) => return Err(read_error(e, "State", span)),
        }
    }
}

/// The last lines of the helper's log, for a start that failed.
fn log_tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let from = lines.len().saturating_sub(5);
    let tail = lines[from..].join(" / ");
    if tail.is_empty() {
        format!("{} is empty", path.display())
    } else {
        tail
    }
}

/// The Fake `Helper`: the real core and serve loop with the Fake machine, on a thread of
/// the test, over a scratch state directory.
#[cfg(any(test, debug_assertions))]
pub mod fake {
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;

    use super::Start;
    use crate::machine::fake::{Fake, Script, Seen};
    use crate::record::{Kernel, Paths, Pids, Record};

    /// Each start's machine: a fresh Fake from its script, and what the test reads back.
    pub type Make = Box<dyn FnMut() -> (Fake, Seen) + Send>;

    /// Starts the helper's core on a thread, with a Fake machine from `make`.
    pub struct Threaded {
        make: Make,
        /// The helper's content address its record and `Attach` name.
        pub helper_sha256: String,
        /// The connection-object policy ([`crate::helper::HOLD_CONNS`]).
        pub hold_conns: bool,
        /// What each start's Fake saw, in start order.
        pub seen: Arc<Mutex<Vec<Seen>>>,
        thread: Option<JoinHandle<Result<String, String>>>,
        /// How each finished helper ended, in order.
        pub ended: Vec<Result<String, String>>,
    }

    impl std::fmt::Debug for Threaded {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Threaded")
                .field("helper_sha256", &self.helper_sha256)
                .field("ended", &self.ended)
                .finish_non_exhaustive()
        }
    }

    impl Threaded {
        /// Every start gets a Fake answering `script`.
        #[must_use]
        pub fn new(script: Script, helper_sha256: &str) -> Self {
            Self::with(Box::new(move || Fake::new(script.clone())), helper_sha256)
        }

        /// Every start gets the Fake `make` makes.
        #[must_use]
        pub fn with(make: Make, helper_sha256: &str) -> Self {
            Self {
                make,
                helper_sha256: helper_sha256.into(),
                hold_conns: crate::helper::HOLD_CONNS,
                seen: Arc::default(),
                thread: None,
                ended: Vec::new(),
            }
        }

        /// The last start's Fake.
        ///
        /// # Panics
        /// Before a start.
        #[must_use]
        pub fn last(&self) -> Seen {
            self.seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .last()
                .cloned()
                .expect("a helper started")
        }

        /// How many helpers started.
        #[must_use]
        pub fn starts(&self) -> usize {
            self.seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len()
        }

        /// Wait for the running helper's thread to end (a one-minute hang detector), and
        /// how it ended.
        ///
        /// # Panics
        /// It did not end within the span.
        pub fn join(&mut self) -> Option<Result<String, String>> {
            let thread = self.thread.take()?;
            let begun = std::time::Instant::now();
            while !thread.is_finished() {
                assert!(
                    begun.elapsed() < crate::detect::SPAN,
                    "the Fake helper did not end within a minute"
                );
                std::thread::sleep(super::POLL);
            }
            let ended = thread.join().unwrap_or_else(|_| Err("it panicked".into()));
            self.ended.push(ended.clone());
            Some(ended)
        }
    }

    impl Start for Threaded {
        fn start(&mut self, paths: &Paths) -> Result<u32, String> {
            if let Some(t) = &self.thread
                && !t.is_finished()
            {
                return Err("a Fake helper still runs".into());
            }
            let _ = self.join();
            let (machine, seen) = (self.make)();
            self.seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(seen);
            let ident = crate::helper::this_process(self.helper_sha256.clone(), paths.live_disk());
            let paths = paths.clone();
            let hold = self.hold_conns;
            self.thread = Some(
                std::thread::Builder::new()
                    .name("fake-helper".into())
                    .spawn(move || crate::helper::run(&paths, machine, ident, hold))
                    .map_err(|e| e.to_string())?,
            );
            Ok(std::process::id())
        }

        fn exited(&mut self) -> Option<String> {
            if self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
                return self.join().map(|ended| format!("ended: {ended:?}"));
            }
            None
        }

        fn settled(&mut self) {}

        fn alive(&self, _record: &Record) -> bool {
            self.thread.as_ref().is_some_and(|t| !t.is_finished())
        }

        fn kill(&mut self, record: &Record) -> Result<(), String> {
            Err(format!(
                "the Fake helper is a thread of this test (pid {}): never signalled",
                record.pid
            ))
        }

        fn pids(&self) -> &dyn Pids {
            &Kernel
        }
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
