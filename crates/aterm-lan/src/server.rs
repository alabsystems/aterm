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

use aterm_uds::CtlStream;
use aterm_uds::lan::wire::{self, Frame, Reader, Request, Target};
use aterm_uds::lan::{ConnectOptions, UdpOptions, here};

use crate::bonjour::{self, Bonjour};

/// The most connections served at once: one more is told the broker is busy and closed.
pub(crate) const MAX_LIVE: usize = 128;

/// How long a client may take to send its hello and request once admitted.
const REQUEST_HANG: Duration = Duration::from_secs(10);

/// How long a client may take to say it took a passed socket in ([`wire::pass`]): a hang
/// detector, as it says so as soon as its receive returns.
const TAKE_HANG: Duration = Duration::from_secs(30);

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
    Code(aterm_keeper::identity::Identity),
}

impl Admission {
    /// The broker's admission: in a test binary the same uid; on macOS the team requirement of
    /// the running image's team, or (an ad-hoc image) its own designated requirement.
    fn of_this_image() -> io::Result<Self> {
        if aterm_uds::test_binary::scratch_root().is_some() {
            return Ok(Self::SameUid);
        }
        #[cfg(target_os = "macos")]
        {
            use aterm_keeper::identity::{Identity, IdentityPolicy};
            let identity = match aterm_keeper::identity::own_team().map_err(io::Error::other)? {
                Some(team) => Identity::for_requirement(&team_requirement(&team)?),
                None => Identity::for_self(IdentityPolicy::Designated),
            }
            .map_err(io::Error::other)?;
            log(format_args!("admits {}", identity.describe()));
            Ok(Self::Code(identity.regardless_of_uid()))
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the LAN broker serves macOS only: no Local Network privacy gates aterm elsewhere",
            ))
        }
    }

    /// Whether the peer on `stream` is admitted.
    fn check(&self, stream: &CtlStream) -> Result<(), String> {
        match self {
            Self::SameUid => match aterm_uds::peer::peer_uid(stream) {
                Some(uid) if uid == aterm_uds::peer::our_uid() => Ok(()),
                Some(uid) => Err(format!("peer uid {uid} is not ours")),
                None => Err("the kernel did not name the peer's uid".to_owned()),
            },
            Self::Code(identity) => identity
                .check_named(
                    stream,
                    aterm_uds::peer::peer_audit_token(stream),
                    aterm_keeper::identity::PEER_CHECK_HANG,
                )
                .map_err(|why| why.to_string()),
        }
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
    let server = Server::bind(&opts.socket, admission, bonjour::system())?;
    log(format_args!(
        "serving {} (protocol {}, build {}), source {}",
        opts.socket.display(),
        aterm_uds::lan::PROTOCOL,
        aterm_uds::lan::hello_build(),
        opts.source.display()
    ));
    #[cfg(target_os = "macos")]
    crate::adopt::watch(opts.source)?;
    server.run()
}

/// A bound broker socket and what serving it needs.
pub(crate) struct Server {
    listener: aterm_uds::CtlListener,
    shared: Arc<Shared>,
}

pub(crate) struct Shared {
    admission: Admission,
    bonjour: Box<dyn Bonjour>,
    live: AtomicUsize,
    /// The test seam the server was bound under, carried into each connection's thread.
    #[cfg(any(test, debug_assertions))]
    seam: aterm_uds::lan::Seam,
}

impl Shared {
    /// What serving needs, under this thread's seam.
    pub(crate) fn new(admission: Admission, bonjour: Box<dyn Bonjour>) -> Self {
        Self {
            admission,
            bonjour,
            live: AtomicUsize::new(0),
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
const PROBE_HANG: Duration = Duration::from_secs(10);

/// Make `socket`'s folder (root's, 0755, when this is root) and clear a stale socket there (one
/// nothing accepts on): a socket a broker still serves is left to it.
///
/// # Errors
/// `AddrInUse` for a socket a broker serves, `AlreadyExists` for something there that is no
/// socket, or the folder's, the probe's or the removal's error.
pub(crate) fn prepare(socket: &Path) -> io::Result<()> {
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
            match aterm_uds::connect::until(socket, Instant::now() + PROBE_HANG) {
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
            shared: Arc::new(Shared::new(admission, bonjour)),
        })
    }

    /// Accept and serve connections, each on its own thread, until accepting fails.
    ///
    /// # Errors
    /// The accept's error.
    pub(crate) fn run(self) -> io::Result<()> {
        loop {
            let (stream, _) = match aterm_uds::accept(&self.listener) {
                Ok(accepted) => accepted,
                Err(error) if error.raw_os_error() == Some(libc::ECONNABORTED) => continue,
                Err(error) => return Err(error),
            };
            if self.shared.live.fetch_add(1, Ordering::AcqRel) >= MAX_LIVE {
                self.shared.live.fetch_sub(1, Ordering::AcqRel);
                let _ = wire::send(
                    &stream,
                    &Frame::Refused {
                        words: format!("the LAN broker is serving {MAX_LIVE} connections"),
                    },
                );
                continue;
            }
            let shared = Arc::clone(&self.shared);
            let spawned = std::thread::Builder::new()
                .name("lan-conn".into())
                .spawn(move || {
                    let _live = Live(&shared.live);
                    #[cfg(any(test, debug_assertions))]
                    aterm_uds::lan::seam_here(shared.seam.clone(), || connection(&shared, &stream));
                    #[cfg(not(any(test, debug_assertions)))]
                    connection(&shared, &stream);
                });
            if let Err(error) = spawned {
                self.shared.live.fetch_sub(1, Ordering::AcqRel);
                log(format_args!("no thread for a connection: {error}"));
            }
        }
    }
}

/// Serve one connection: admit its peer, exchange hellos, and answer its one request.
pub(crate) fn connection(shared: &Shared, stream: &CtlStream) {
    let pid = aterm_uds::fdpass::peer_pid(stream).unwrap_or(0);
    if let Err(why) = shared.admission.check(stream) {
        log(format_args!("refused pid {pid}: {why}"));
        let _ = wire::send(stream, &Frame::Refused { words: why });
        return;
    }
    let hello = Frame::Hello {
        protocol: aterm_uds::lan::PROTOCOL,
        build: aterm_uds::lan::hello_build().to_owned(),
    };
    if wire::send(stream, &hello).is_err() {
        return;
    }
    let mut reader = Reader::new(0);
    let deadline = Some(Instant::now() + REQUEST_HANG);
    match reader.frame(stream, deadline) {
        Ok(Some(Frame::Hello { protocol, .. })) if protocol == aterm_uds::lan::PROTOCOL => {}
        Ok(Some(Frame::Hello { protocol, build })) => {
            log(format_args!(
                "pid {pid} speaks protocol {protocol} (build {build}), this broker {}",
                aterm_uds::lan::PROTOCOL
            ));
            return;
        }
        Ok(None) => return,
        Ok(Some(other)) => {
            log(format_args!("pid {pid} opened with {other:?}"));
            return;
        }
        Err(error) => {
            log(format_args!("pid {pid}: {error}"));
            return;
        }
    }
    let request = match reader.frame(stream, deadline) {
        Ok(Some(Frame::Request(request))) => request,
        // A hello alone: `aterm lan status`.
        Ok(None) => return,
        Ok(Some(other)) => {
            log(format_args!("pid {pid} asked {other:?}"));
            return;
        }
        Err(error) => {
            log(format_args!("pid {pid}: {error}"));
            return;
        }
    };
    if reader.buffered() {
        log(format_args!("pid {pid} sent bytes past its one request"));
        return;
    }
    if let Err((code, words)) = answer(shared, stream, request) {
        log(format_args!("pid {pid}: {words}"));
        let _ = wire::send(stream, &Frame::Error { code, words });
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
fn answer(shared: &Shared, stream: &CtlStream, request: Request) -> Result<(), Failure> {
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
                    here::dial_any(std::slice::from_ref(&addr), deadline, options)
                        .map_err(|e| failure(&e))?
                }
                Target::Host(name) => {
                    let mut addrs = here::resolve(&name, port, within).map_err(|e| failure(&e))?;
                    if !addrs
                        .iter()
                        .any(|addr| aterm_uds::lan::is_local_network(&addr.ip()))
                    {
                        return Err(not_local(format_args!("{name} ({addrs:?})")));
                    }
                    addrs.retain(|addr| aterm_uds::lan::is_local_network(&addr.ip()));
                    here::dial_any(&addrs, deadline, options).map_err(|e| failure(&e))?
                }
            };
            wire::pass(
                stream,
                &Frame::Socket {
                    reached: Some(reached),
                },
                connected.as_fd(),
                Instant::now() + TAKE_HANG,
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
            wire::pass(
                stream,
                &Frame::Socket { reached: None },
                socket.as_fd(),
                Instant::now() + TAKE_HANG,
            )
            .map_err(|e| failure(&e))
        }
        Request::Resolve { name, within_ms } => {
            if !aterm_uds::lan::is_local_name(&name) {
                return Err(not_local(&name));
            }
            let resolved = here::resolve(&name, 0, within(within_ms)?).map_err(|e| failure(&e))?;
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
            bonjour::drive(stream, live.as_mut())
        }
    }
}
