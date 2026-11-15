// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The broker's socket, its admission of each client, and its one request per connection.

use std::io::{self, Write as _};
use std::net::IpAddr;
use std::os::fd::AsFd as _;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aterm_keeper::identity::CheckError;
use aterm_uds::CtlStream;
use aterm_uds::lan::wire::{self, Frame, Reader, Request, Target};
use aterm_uds::lan::{ConnectOptions, UdpOptions, here};

use crate::bonjour::{self, Bonjour};
use crate::counts::{Count, Counts, Sink};

/// The most connections served at once: one more is told the broker is busy
/// ([`Frame::Unchecked`]) and closed.
pub(crate) const MAX_LIVE: usize = 128;

/// How long a client may take to send its hello and request once admitted.
const REQUEST_HANG: Duration = Duration::from_secs(60);

/// How long a client may take to say it took a passed socket in ([`wire::pass`]): a hang
/// detector, as it says so as soon as its receive returns.
const TAKE_HANG: Duration = Duration::from_secs(60);

pub(crate) fn pass_socket(
    stream: &CtlStream,
    frame: &Frame,
    fd: std::os::fd::BorrowedFd<'_>,
    started: Instant,
) -> io::Result<()> {
    let deadline = started + TAKE_HANG;
    wire::pass(stream, frame, fd, deadline)
}

/// What `aterm lan serve` serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opts {
    /// The app bundle the elevation ran from: its signed updates are adopted (`--source`).
    pub source: PathBuf,
    /// The socket to serve ([`aterm_uds::lan::socket_path`]).
    pub socket: PathBuf,
}

/// One line to the broker's log (its stderr, which launchd sends to [`aterm_uds::lan::LOG_PATH`]).
pub(crate) fn log(line: std::fmt::Arguments<'_>) {
    let _ = writeln!(io::stderr(), "aterm-lan: {line}");
}

/// Who a connection must be to be served.
pub(crate) enum Admission {
    /// A peer of this uid: a test binary's own broker and its own clients.
    SameUid,
    /// A peer whose code meets the requirement, of any uid.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "code is checked on macOS")
    )]
    Code(Arc<aterm_keeper::identity::Identity>),
    /// A law's script in place of the kernel and macOS: it takes the path [`Self::Code`] takes
    /// ([`full_check`]), so what that path counts is counted of real connections.
    #[cfg(test)]
    Scripted(Scripted),
}

/// A scripted read of a connection's audit token.
#[cfg(test)]
type ScriptedToken = dyn Fn(&CtlStream) -> Option<aterm_uds::peer::AuditToken> + Send + Sync;

/// A scripted full check of the process a token names.
#[cfg(test)]
type ScriptedCheck =
    dyn Fn(Option<aterm_uds::peer::AuditToken>) -> Result<(), CheckError> + Send + Sync;

/// What a law scripts of a connection's admission.
#[cfg(test)]
pub(crate) struct Scripted {
    /// The audit token the kernel is said to give for the connection.
    pub(crate) token: Box<ScriptedToken>,
    /// The full check's verdict of the process that token names.
    pub(crate) full_check: Box<ScriptedCheck>,
}

/// The one path every admission by code takes: the full check `check` of the process `token`
/// names, counted where it is called ([`Count::ChecksRun`]: a call, whether or not a check then
/// starts), and as it returns the requesting process counted by its token with what the check
/// said of it.
fn full_check(
    counts: &Counts,
    token: Option<aterm_uds::peer::AuditToken>,
    check: impl FnOnce(Option<aterm_uds::peer::AuditToken>) -> Result<(), CheckError>,
) -> Result<(), CheckError> {
    counts.add(Count::ChecksRun);
    let verdict = check(token);
    counts.requester(token, verdict.is_ok());
    verdict
}

/// Resolve this image once. Only the canonical team branch supplies an update
/// checker; an ad-hoc designated peer requirement grants no adoption policy.
struct ImageAdmission {
    peers: Admission,
    #[cfg(target_os = "macos")]
    updates: Option<Arc<aterm_keeper::identity::Identity>>,
}

impl Admission {
    /// The broker's admission: in a test binary the same uid; on macOS the team requirement of
    /// the running image's team, or (an ad-hoc image) its own designated requirement.
    fn of_this_image() -> io::Result<ImageAdmission> {
        if aterm_uds::test_binary::scratch_root().is_some() {
            return Ok(ImageAdmission {
                peers: Self::SameUid,
                #[cfg(target_os = "macos")]
                updates: None,
            });
        }
        #[cfg(target_os = "macos")]
        {
            use aterm_keeper::identity::{Identity, IdentityPolicy};
            let (identity, team) =
                match aterm_keeper::identity::own_team().map_err(io::Error::other)? {
                    Some(team) => (Identity::for_requirement(&team_requirement(&team)?), true),
                    None => (Identity::for_self(IdentityPolicy::Designated), false),
                };
            let identity = identity.map_err(io::Error::other)?;
            log(format_args!("admits {}", identity.describe()));
            let identity = Arc::new(identity.regardless_of_uid());
            Ok(ImageAdmission {
                peers: Self::Code(Arc::clone(&identity)),
                updates: team.then_some(identity),
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the LAN broker serves macOS only: no Local Network privacy gates aterm elsewhere",
            ))
        }
    }

    /// Whether the peer on `stream` is admitted: a refusal of its uid or code, or a check that
    /// could not run or finish, which judged nothing.
    fn check(&self, stream: &CtlStream, counts: &Counts) -> Result<(), CheckError> {
        match self {
            Self::SameUid => match aterm_uds::peer::peer_uid(stream) {
                Some(uid) if uid == aterm_uds::peer::our_uid() => Ok(()),
                Some(uid) => Err(CheckError::Refused(format!("peer uid {uid} is not ours"))),
                None => Err(CheckError::Unchecked(
                    "the kernel did not name the peer's uid".to_owned(),
                )),
            },
            Self::Code(identity) => {
                full_check(counts, aterm_uds::peer::peer_audit_token(stream), |token| {
                    identity.check_named_costed(
                        stream,
                        token,
                        aterm_uds::peer::CHECK_HANG,
                        &counts.check_cost,
                    )
                })
            }
            #[cfg(test)]
            Self::Scripted(script) => full_check(counts, (script.token)(stream), |token| {
                (script.full_check)(token)
            }),
        }
    }
}

/// What a peer the broker did not admit is told instead of its hello: [`Frame::Refused`] for a
/// uid or code it judged and will not serve, which stands until its socket is made again, and
/// [`Frame::Unchecked`] for a check that could not run or did not finish (`trustd` did not
/// answer), after which the client asks again.
pub(crate) fn not_admitted(why: CheckError) -> Frame {
    match why {
        CheckError::Refused(words) => Frame::Refused { words },
        CheckError::Unchecked(words) | CheckError::Unanswered(words) => Frame::Unchecked { words },
    }
}

/// The requirement of aterm's code signed by `team`.
///
/// # Errors
/// `InvalidData` for a team identifier that is not 1 to 20 ASCII letters and digits (one that
/// could change the requirement's meaning).
#[cfg(target_os = "macos")]
pub(crate) fn team_requirement(team: &str) -> io::Result<String> {
    if team.is_empty() || team.len() > 20 || !team.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("signing team {team:?} is not a team identifier"),
        ));
    }
    Ok(format!(
        "identifier \"com.aterm.aterm\" and anchor apple generic and \
         certificate leaf[subject.OU] = \"{team}\""
    ))
}

/// Serve the broker on `opts.socket` until the process ends: on macOS as root, admitting aterm's
/// code, and adopting a signed update of `opts.source` (it then exits 0 for launchd to start the
/// new image, as it does when its own image is replaced).
///
/// It counts its connections, its full checks, its name resolutions and their CPU time into
/// [`aterm_uds::lan::COUNTS_NAME`] beside the socket ([`crate::counts`]): on a running Mac,
/// `cat /var/run/systems.alab.aterm-lan/broker.counts`.
///
/// # Errors
/// Not root (outside a test binary), off macOS, the socket's folder or the socket that cannot be
/// made, or the admission requirement that cannot be read.
pub fn serve(opts: Opts) -> io::Result<()> {
    if aterm_uds::test_binary::scratch_root().is_none() && aterm_uds::peer::our_uid() != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the LAN broker runs as root: launchd starts it (systems.alab.aterm-lan)",
        ));
    }
    let admission = Admission::of_this_image()?;
    let server = Server::bind(&opts.socket, admission.peers, bonjour::system())?;
    log(format_args!(
        "serving {} (protocol {}, build {}), source {}",
        opts.socket.display(),
        aterm_uds::lan::PROTOCOL,
        aterm_uds::lan::hello_build(),
        opts.source.display()
    ));
    #[cfg(target_os = "macos")]
    crate::adopt::watch(opts.source, admission.updates)?;
    server.run()
}

/// A bound broker socket and what serving it needs.
pub(crate) struct Server {
    listener: aterm_uds::CtlListener,
    pub(crate) shared: Arc<Shared>,
    /// Where the accept loop writes the counts ([`crate::counts`]).
    pub(crate) sink: Sink,
}

pub(crate) struct Shared {
    admission: Admission,
    bonjour: Box<dyn Bonjour>,
    /// Where a connection's lines go: the broker's [`log`], or nowhere when the one client is
    /// this process (the direct path), which hears each failure itself as a frame.
    log: fn(std::fmt::Arguments<'_>),
    pub(crate) live: AtomicUsize,
    /// What this server counts of its connections ([`crate::counts`]). Only a served socket's
    /// accept loop writes them to a file; the direct path's are counted and read by nobody.
    pub(crate) counts: Counts,
    /// The test seam the server was bound under, carried into each connection's thread.
    #[cfg(any(test, debug_assertions))]
    seam: aterm_uds::lan::Seam,
}

impl Shared {
    /// What serving needs, under this thread's seam, its connections' lines going to `log`.
    pub(crate) fn new(
        admission: Admission,
        bonjour: Box<dyn Bonjour>,
        log: fn(std::fmt::Arguments<'_>),
    ) -> Self {
        Self {
            admission,
            bonjour,
            log,
            live: AtomicUsize::new(0),
            counts: Counts::new(),
            #[cfg(any(test, debug_assertions))]
            seam: aterm_uds::lan::seam(),
        }
    }
}

/// One live connection's count, given back when it ends.
struct Live<'a>(&'a AtomicUsize);

impl Drop for Live<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// How long a socket already at the path may take to accept the probe that asks whether a broker
/// serves it: a hang detector, as a live broker accepts at once and a stale socket refuses.
const PROBE_HANG: Duration = Duration::from_secs(60);

/// Make `socket`'s folder (root's, 0755, when this is root) and clear a stale socket there (one
/// nothing accepts on): a socket a broker still serves is left to it.
///
/// # Errors
/// `AddrInUse` for a socket a broker serves, `AlreadyExists` for something there that is no
/// socket, or the folder's, the probe's or the removal's error.
pub(crate) fn prepare(socket: &Path) -> io::Result<()> {
    prepare_at(socket, Instant::now())
}

pub(crate) fn prepare_at(socket: &Path, started: Instant) -> io::Result<()> {
    let dir = socket
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "a socket with no folder"))?;
    std::fs::create_dir_all(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a folder", dir.display()),
        ));
    }
    if aterm_uds::peer::our_uid() == 0 {
        std::os::unix::fs::lchown(dir, Some(0), Some(0))?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
        let meta = std::fs::symlink_metadata(dir)?;
        if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is not root's alone", dir.display()),
            ));
        }
    }
    match std::fs::symlink_metadata(socket) {
        Ok(meta) if meta.file_type().is_socket() => {
            let deadline = started + PROBE_HANG;
            match aterm_uds::connect::until(socket, deadline) {
                Ok(_) => Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("a broker already serves {}", socket.display()),
                )),
                Err(error) if error.raw_os_error() == Some(libc::ECONNREFUSED) => {
                    std::fs::remove_file(socket)
                }
                Err(error) => Err(error),
            }
        }
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} is there and is no socket", socket.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

impl Server {
    /// Bind `socket` (its folder made, a stale socket cleared, mode 0666: clients are admitted by
    /// code, not by the socket's mode).
    pub(crate) fn bind(
        socket: &Path,
        admission: Admission,
        bonjour: Box<dyn Bonjour>,
    ) -> io::Result<Self> {
        prepare(socket)?;
        let listener = aterm_uds::kept_out(|| aterm_uds::CtlListener::bind(socket))?;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o666))?;
        Ok(Self {
            listener,
            shared: Arc::new(Shared::new(admission, bonjour, log)),
            sink: Sink::beside(socket),
        })
    }

    /// Accept one connection and count it: the connection to serve (`None` for one told the
    /// broker is serving its most), and whether the counts are due to be written
    /// ([`aterm_uds::tally::due`]: at the 1st, 2nd, 4th ... 256th connection, then each 256th).
    ///
    /// # Errors
    /// The accept's error.
    fn accept(&self) -> io::Result<(Option<CtlStream>, bool)> {
        let stream = loop {
            match aterm_uds::accept(
                &self.listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            ) {
                Ok((stream, _)) => break stream,
                Err(error) if error.raw_os_error() == Some(libc::ECONNABORTED) => {}
                Err(error) => return Err(error),
            }
        };
        let due = aterm_uds::tally::due(self.shared.counts.add(Count::Accepted));
        if self.shared.live.fetch_add(1, Ordering::AcqRel) >= MAX_LIVE {
            self.shared.live.fetch_sub(1, Ordering::AcqRel);
            self.shared.counts.add(Count::Busy);
            let _ = wire::send(
                &stream,
                &Frame::Unchecked {
                    words: format!("the LAN broker is serving {MAX_LIVE} connections"),
                },
            );
            return Ok((None, due));
        }
        Ok((Some(stream), due))
    }

    /// One turn of the accept loop: accept a connection and count it, hand it to `serve` (not
    /// one told the broker is serving its most), and then write the counts where they are due.
    /// The write comes after the hand-off, so the connection it was due at is not kept from its
    /// thread by it; a connection that arrives during the write waits in the listen queue for
    /// it, which is one small file made and renamed at 9 of the first 256 connections and at
    /// each 256th after. No timer writes the counts, so a broker nothing connects to writes
    /// nothing.
    ///
    /// # Errors
    /// The accept's error.
    pub(crate) fn step(&mut self, serve: impl FnOnce(&Arc<Shared>, CtlStream)) -> io::Result<()> {
        let (stream, due) = self.accept()?;
        if let Some(stream) = stream {
            serve(&self.shared, stream);
        }
        if due {
            self.sink.write(&self.shared.counts, log);
        }
        Ok(())
    }

    /// Accept and serve connections, each on its own thread, until accepting fails.
    ///
    /// # Errors
    /// The accept's error.
    pub(crate) fn run(mut self) -> io::Result<()> {
        loop {
            self.step(|shared, stream| {
                let serving = Arc::clone(shared);
                let spawned =
                    std::thread::Builder::new()
                        .name("lan-conn".into())
                        .spawn(move || {
                            let shared = serving;
                            let _live = Live(&shared.live);
                            // This thread was started for this connection: its whole CPU time is the
                            // connection's, added when it ends and, for a standing operation, each
                            // time it hears something.
                            let mut cpu = aterm_uds::cpu::ThreadCpu::born();
                            let mut spent = || cpu.add_to(&shared.counts.conn_cpu_ns);
                            #[cfg(any(test, debug_assertions))]
                            aterm_uds::lan::seam_here(shared.seam.clone(), || {
                                connection(&shared, &stream, &mut spent);
                            });
                            #[cfg(not(any(test, debug_assertions)))]
                            connection(&shared, &stream, &mut spent);
                            spent();
                        });
                if let Err(error) = spawned {
                    shared.live.fetch_sub(1, Ordering::AcqRel);
                    shared.counts.add(Count::NoThread);
                    log(format_args!("no thread for a connection: {error}"));
                }
            })?;
        }
    }
}

/// Serve one connection: admit its peer, exchange hellos, and answer its one request. `spent` is
/// called where a standing operation heard something: the serving thread's CPU time so far can be
/// added there (the accept loop's threads do; the direct path, on its caller's thread, adds none).
pub(crate) fn connection(shared: &Shared, stream: &CtlStream, spent: &mut dyn FnMut()) {
    connection_clock(shared, stream, Instant::now, spent);
}

#[cfg(test)]
pub(crate) fn connection_at(shared: &Shared, stream: &CtlStream, started: Instant) {
    connection_clock(shared, stream, || started, &mut || {});
}

fn connection_clock(
    shared: &Shared,
    stream: &CtlStream,
    started: impl FnOnce() -> Instant,
    spent: &mut dyn FnMut(),
) {
    let ended = served(shared, stream, started, spent);
    shared.counts.add(ended);
}

/// One connection served: the one way it ended, which [`connection_clock`] counts.
fn served(
    shared: &Shared,
    stream: &CtlStream,
    started: impl FnOnce() -> Instant,
    spent: &mut dyn FnMut(),
) -> Count {
    let pid = aterm_uds::fdpass::peer_pid(stream).unwrap_or(0);
    if let Err(why) = shared.admission.check(stream, &shared.counts) {
        let (judged, ended) = match why {
            CheckError::Refused(_) => ("refused", Count::Refused),
            CheckError::Unchecked(_) => ("could not check", Count::Unchecked),
            CheckError::Unanswered(_) => ("could not check", Count::Unanswered),
        };
        (shared.log)(format_args!("{judged} pid {pid}: {why}"));
        let _ = wire::send(stream, &not_admitted(why));
        return ended;
    }
    shared.counts.add(Count::Admitted);
    let hello = Frame::Hello {
        protocol: aterm_uds::lan::PROTOCOL,
        build: aterm_uds::lan::hello_build().to_owned(),
    };
    if wire::send(stream, &hello).is_err() {
        return Count::HelloUnsent;
    }
    let mut reader = Reader::new(0);
    let deadline = Some(started() + REQUEST_HANG);
    match reader.frame(stream, deadline) {
        Ok(Some(Frame::Hello { protocol, .. })) if protocol == aterm_uds::lan::PROTOCOL => {}
        Ok(Some(Frame::Hello { protocol, build })) => {
            (shared.log)(format_args!(
                "pid {pid} speaks protocol {protocol} (build {build}), this broker {}",
                aterm_uds::lan::PROTOCOL
            ));
            return Count::WrongProtocol;
        }
        Ok(None) => return Count::ClosedUnsaid,
        Ok(Some(other)) => {
            (shared.log)(format_args!("pid {pid} opened with {other:?}"));
            return Count::WrongFirstFrame;
        }
        Err(error) => {
            (shared.log)(format_args!("pid {pid}: {error}"));
            return Count::ReadError;
        }
    }
    let request = match reader.frame(stream, deadline) {
        Ok(Some(Frame::Request(request))) => request,
        // A hello alone: `aterm lan status`.
        Ok(None) => return Count::HelloOnly,
        Ok(Some(other)) => {
            (shared.log)(format_args!("pid {pid} asked {other:?}"));
            return Count::WrongRequestFrame;
        }
        Err(error) => {
            (shared.log)(format_args!("pid {pid}: {error}"));
            return Count::ReadError;
        }
    };
    if reader.buffered() {
        (shared.log)(format_args!("pid {pid} sent bytes past its one request"));
        return Count::TrailingBytes;
    }
    shared.counts.add(match &request {
        Request::Connect { .. } => Count::Connect,
        Request::Udp { .. } => Count::Udp,
        Request::Resolve { .. } => Count::Resolve,
        Request::Browse { .. } => Count::Browse,
        Request::ResolveService { .. } => Count::ResolveService,
        Request::Query { .. } => Count::Query,
        Request::Register(_) => Count::Register,
    });
    match answer(shared, stream, request, spent) {
        Ok(()) => Count::Answered,
        Err((code, words)) => {
            (shared.log)(format_args!("pid {pid}: {words}"));
            let _ = wire::send(stream, &Frame::Error { code, words });
            Count::Failed
        }
    }
}

/// A failed request: the code (an `errno`, or a DNS-SD error) and the words.
pub(crate) type Failure = (i32, String);

/// `error` as the wire's code: its `errno`, or one its kind stands for.
fn errno(error: &io::Error) -> i32 {
    error.raw_os_error().unwrap_or(match error.kind() {
        io::ErrorKind::PermissionDenied => libc::EACCES,
        io::ErrorKind::TimedOut => libc::ETIMEDOUT,
        io::ErrorKind::InvalidInput => libc::EINVAL,
        io::ErrorKind::NotFound => libc::ENOENT,
        io::ErrorKind::Unsupported => libc::ENOTSUP,
        _ => libc::EIO,
    })
}

fn failure(error: &io::Error) -> Failure {
    (errno(error), error.to_string())
}

/// A request's bound, at most [`here::MAX_WITHIN`]; zero is refused.
fn within(ms: u32) -> Result<Duration, Failure> {
    if ms == 0 {
        return Err((libc::EINVAL, "a request bound of 0 ms".to_owned()));
    }
    Ok(Duration::from_millis(u64::from(ms)).min(here::MAX_WITHIN))
}

fn not_local(what: impl std::fmt::Display) -> Failure {
    (
        libc::EACCES,
        format!(
            "{what} is not on the local network: the LAN broker serves only local-network destinations"
        ),
    )
}

/// Answer `request` on `stream`: the reply, or the failure to send instead.
fn answer(
    shared: &Shared,
    stream: &CtlStream,
    request: Request,
    spent: &mut dyn FnMut(),
) -> Result<(), Failure> {
    match request {
        Request::Connect {
            target,
            port,
            within_ms,
            bound_if,
            source,
            hop_limit,
        } => {
            let within = within(within_ms)?;
            let deadline = Instant::now() + within;
            let options = ConnectOptions {
                bound_if,
                source,
                hop_limit,
            };
            let (connected, reached) = match target {
                Target::Addr(mut addr) => {
                    addr.set_port(port);
                    if !aterm_uds::lan::is_local_network(&addr.ip()) {
                        return Err(not_local(addr));
                    }
                    here::dial_any_while_open(
                        std::slice::from_ref(&addr),
                        deadline,
                        options,
                        stream.as_fd(),
                    )
                    .map_err(|e| failure(&e))?
                }
                Target::Host(name) => {
                    let mut addrs =
                        here::resolve_counted(&name, port, within, &shared.counts.resolve_threads)
                            .map_err(|e| failure(&e))?;
                    if !addrs
                        .iter()
                        .any(|addr| aterm_uds::lan::is_local_network(&addr.ip()))
                    {
                        return Err(not_local(format_args!("{name} ({addrs:?})")));
                    }
                    addrs.retain(|addr| aterm_uds::lan::is_local_network(&addr.ip()));
                    here::dial_any_while_open(&addrs, deadline, options, stream.as_fd())
                        .map_err(|e| failure(&e))?
                }
            };
            pass_socket(
                stream,
                &Frame::Socket {
                    reached: Some(reached),
                },
                connected.as_fd(),
                Instant::now(),
            )
            .map_err(|e| failure(&e))
        }
        Request::Udp {
            bind,
            reuse,
            groups,
            multicast_if,
            ttl,
        } => {
            let socket = here::udp(&UdpOptions {
                bind,
                reuse,
                groups,
                multicast_if,
                ttl,
            })
            .map_err(|e| failure(&e))?;
            pass_socket(
                stream,
                &Frame::Socket { reached: None },
                socket.as_fd(),
                Instant::now(),
            )
            .map_err(|e| failure(&e))
        }
        Request::Resolve { name, within_ms } => {
            if !aterm_uds::lan::is_local_name(&name) {
                return Err(not_local(&name));
            }
            let resolved =
                here::resolve_counted(&name, 0, within(within_ms)?, &shared.counts.resolve_threads)
                    .map_err(|e| failure(&e))?;
            let mut addrs: Vec<IpAddr> = Vec::new();
            for addr in resolved {
                if !addrs.contains(&addr.ip()) && addrs.len() < wire::MAX_ITEMS {
                    addrs.push(addr.ip());
                }
            }
            wire::send(stream, &Frame::Addresses(addrs)).map_err(|e| failure(&e))
        }
        bonjour_request => {
            let mut live = shared.bonjour.start(&bonjour_request)?;
            bonjour::drive(stream, live.as_mut(), spent)
        }
    }
}
