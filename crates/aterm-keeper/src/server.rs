// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE KEEPER'S LOOP: [`crate::core::KeeperCore`] over a real socket, real
//! descriptors and the real kernel.
//!
//! One thread, one `poll(2)` over the listener, every connection and every
//! exit watch, so no descriptor can leak through a concurrent spawn (the Darwin
//! race `aterm-uds/src/lifeline.rs` records). The loop owns every custody copy
//! (`custody`, keyed by rdev) and does exactly what the core decides with them:
//! keep, close, or send a duplicate in an OFFER. It never reads, writes,
//! ioctls or changes the file-status flags of a master (those flags are shared
//! by every duplicate of the description); `fstat` for the rdev is the one call
//! it makes on one. It never signals anything. The only program it ever starts
//! is the relaunch: `/usr/bin/open -a <its own bundle>`, inheriting nothing
//! (`aterm_uds::pspawn`).

use std::collections::BTreeMap;
use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _, OwnedFd};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::CtlStream;
use aterm_uds::exitwatch::ExitWatch;

use crate::core::{ConnId, Holders, KeeperCore, KeeperEnv, MarkerEvidence, Out, Rdev};
use crate::frameio::{Incoming, read_frame, write_frame};
use crate::identity::Identity;
use crate::sys::{self, POLLERR, POLLHUP, POLLIN, PollFd};
use crate::wire::{Birth, Frame, MarkerRef, MasterHeader, PROTO};

/// How a relaunch is carried out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Relauncher {
    /// Never: this keeper runs outside an app bundle.
    Unavailable,
    /// `/usr/bin/open -a <bundle>` — the only production form.
    OpenBundle(PathBuf),
    /// A TEST SEAM, reachable from the library only (no flag spells it): run
    /// this argv instead, so a test can count relaunches without launching an
    /// app.
    TestCommand(Vec<String>),
}

/// What `serve` is told.
#[derive(Debug)]
pub struct ServeConfig {
    /// The socket to bind. Its directory must exist, be ours and be private.
    pub socket: PathBuf,
    /// Who may connect.
    pub identity: Identity,
    /// How to relaunch.
    pub relaunch: Relauncher,
    /// The keeper's version string (WELCOME, status).
    pub version: String,
    /// A TEST SEAM, library-only: close every registered master at once
    /// instead of keeping it — the negative control that shows the custody
    /// copy is what keeps a shell alive.
    pub test_drop_custody: bool,
}

struct Conn {
    stream: CtlStream,
    pid: u32,
}

/// The server. [`Server::run`] is the keeper; [`Server::step`] is one turn of
/// it, for tests that drive it in-process.
pub struct Server {
    listener: UnixListener,
    socket: PathBuf,
    core: KeeperCore,
    conns: BTreeMap<ConnId, Conn>,
    custody: BTreeMap<Rdev, OwnedFd>,
    watches: BTreeMap<u32, ExitWatch>,
    children: Vec<u32>,
    identity: Identity,
    relaunch: Relauncher,
    version: String,
    test_drop_custody: bool,
    next_conn: ConnId,
    started: Instant,
    own_pid: u32,
    refused_dials: u64,
    /// Frames read after their writer's exit had already arrived, before the
    /// exit was recorded ([`Server::drain_pid`]).
    frames_behind_exit: u64,
}

/// A per-connection receive/send budget: once a frame's header has arrived,
/// the rest must follow within this, or the connection is dropped (one slow
/// peer cannot stall the loop).
const IO_TIMEOUT: Duration = Duration::from_secs(2);
/// The loop's clock tick when nothing is readable.
const TICK_MS: i32 = 250;
/// The most frames [`Server::drain_pid`] reads from one connection of a dead
/// peer: more than a socket buffer of the smallest frame (8 KiB of 8-byte
/// headers).
const DRAIN_MAX_FRAMES: usize = 1024;

struct Env<'a> {
    own_pid: u32,
    peer_pids: Vec<u32>,
    _p: std::marker::PhantomData<&'a ()>,
}

impl KeeperEnv for Env<'_> {
    fn holders(&mut self, rdev: Rdev) -> Holders {
        match aterm_uds::holders::scan_rdev_holders(rdev, &[self.own_pid]) {
            Ok(scan) if !scan.holders.is_empty() => Holders::Held(scan.holders),
            // A peer of ours whose table could not be read may hold it.
            Ok(scan) if scan.unreadable.iter().any(|p| self.peer_pids.contains(p)) => {
                Holders::Unknown
            }
            Ok(_) => Holders::None,
            Err(_) => Holders::Unknown,
        }
    }
    fn shell_alive(&mut self, pid: u32, birth: Birth) -> bool {
        born(pid) == Some(birth)
    }
    fn pid_alive(&mut self, pid: u32, birth: Option<Birth>) -> bool {
        match birth {
            Some(b) => born(pid) == Some(b),
            None => aterm_uds::process::pid_alive(pid),
        }
    }
    fn marker(&mut self, pid: u32, marker: &MarkerRef) -> MarkerEvidence {
        read_marker(pid, marker)
    }
}

/// What the crash marker of window `pid` says now (§5.4 row 3). The keeper
/// builds the file names itself from the KERNEL's pid for the connection and
/// the start the HELLO named — `crash-marker-<pid>-<nanos>-<app|other>.log`
/// (an update's candidate renames `-other` to `-app` at its Commit), and the
/// `.seen` name a later start gives a dead marker it has read — so a window
/// can point the keeper at nothing but its own marker. Only an absolute
/// directory of ours counts: a marker "missing" from a directory the keeper
/// cannot see is no evidence. Nothing is written, and a found marker is opened
/// read-only, never following a symlink, only to try its lock.
#[must_use]
pub fn read_marker(pid: u32, marker: &MarkerRef) -> MarkerEvidence {
    use std::os::unix::ffi::OsStrExt as _;
    let dir = Path::new(std::ffi::OsStr::from_bytes(&marker.dir));
    if !dir.is_absolute() {
        return MarkerEvidence::Unknown;
    }
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_dir() && m.uid() == aterm_uds::peer::our_uid() => {}
        _ => return MarkerEvidence::Unknown,
    }
    for owner in ["app", "other"] {
        let name = format!("crash-marker-{pid}-{}-{owner}.log", marker.nanos);
        for (file, seen) in [(name.clone(), false), (format!("{name}.seen"), true)] {
            let path = dir.join(file);
            match std::fs::symlink_metadata(&path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Ok(m) if m.file_type().is_file() => {
                    if seen {
                        // A later start read it as a death with no exit path.
                        return MarkerEvidence::Dead;
                    }
                    return match aterm_uds::ownerlock::probe(&path) {
                        aterm_uds::ownerlock::Owner::Live => MarkerEvidence::Held,
                        aterm_uds::ownerlock::Owner::Dead => MarkerEvidence::Dead,
                        aterm_uds::ownerlock::Owner::Unknown => MarkerEvidence::Unknown,
                    };
                }
                // A symlink, a directory, an unreadable entry: no evidence.
                _ => return MarkerEvidence::Unknown,
            }
        }
    }
    MarkerEvidence::Released
}

/// A pid's kernel start time, in the wire's shape.
#[must_use]
pub fn born(pid: u32) -> Option<Birth> {
    let b = aterm_uds::process::read_process_birth(i32::try_from(pid).ok()?)?;
    Some(Birth {
        seconds: b.seconds,
        micros: u32::try_from(b.microseconds).ok()?,
    })
}

impl Server {
    /// Bind the socket. A stale socket file at the path is replaced only when
    /// nothing answers on it.
    ///
    /// # Errors
    /// The directory not being private and ours, a live keeper already on the
    /// socket (`AddrInUse`), or the bind failing.
    pub fn bind(config: ServeConfig) -> io::Result<Self> {
        let dir = config
            .socket
            .parent()
            .ok_or_else(|| io::Error::other("the socket path has no directory"))?;
        ensure_private_dir(dir)?;
        if config.socket.exists() {
            if CtlStream::connect(&config.socket).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("a keeper already answers on {}", config.socket.display()),
                ));
            }
            std::fs::remove_file(&config.socket)?;
        }
        let listener = UnixListener::bind(&config.socket)?;
        prove_socket_is_ours(dir, &config.socket)?;
        listener.set_nonblocking(true)?;
        sys::raise_nofile();
        let can_relaunch = config.relaunch != Relauncher::Unavailable;
        Ok(Self {
            listener,
            socket: config.socket,
            core: KeeperCore::new(can_relaunch),
            conns: BTreeMap::new(),
            custody: BTreeMap::new(),
            watches: BTreeMap::new(),
            children: Vec::new(),
            identity: config.identity,
            relaunch: config.relaunch,
            version: config.version,
            test_drop_custody: config.test_drop_custody,
            next_conn: 1,
            started: Instant::now(),
            own_pid: std::process::id(),
            refused_dials: 0,
            frames_behind_exit: 0,
        })
    }

    /// The bound socket.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Serve until the process is ended (launchd's SIGTERM, a kill).
    ///
    /// # Errors
    /// `poll` failing for a reason other than a signal.
    pub fn run(&mut self) -> io::Result<()> {
        loop {
            self.step(TICK_MS)?;
        }
    }

    fn now(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn env(&self) -> Env<'static> {
        Env {
            own_pid: self.own_pid,
            peer_pids: self.conns.values().map(|c| c.pid).collect(),
            _p: std::marker::PhantomData,
        }
    }

    /// One turn: wait up to `timeout_ms` for a connection, a frame or an exit,
    /// handle what arrived, then tick the clock.
    ///
    /// # Errors
    /// `poll` failing for a reason other than a signal.
    pub fn step(&mut self, timeout_ms: i32) -> io::Result<()> {
        let mut fds = vec![PollFd {
            fd: self.listener.as_raw_fd(),
            events: POLLIN,
            revents: 0,
        }];
        let conn_ids: Vec<ConnId> = self.conns.keys().copied().collect();
        for id in &conn_ids {
            fds.push(PollFd {
                fd: self.conns[id].stream.as_raw_fd(),
                events: POLLIN,
                revents: 0,
            });
        }
        let watch_pids: Vec<u32> = self.watches.keys().copied().collect();
        for pid in &watch_pids {
            fds.push(PollFd {
                fd: self.watches[pid].as_raw_fd(),
                events: POLLIN,
                revents: 0,
            });
        }
        match sys::poll(&mut fds, timeout_ms) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
        let ready = |pfd: &PollFd| pfd.revents & (POLLIN | POLLHUP | POLLERR) != 0;
        if ready(&fds[0]) {
            self.accept_all();
        }
        for (i, id) in conn_ids.iter().enumerate() {
            if ready(&fds[1 + i]) {
                self.read_conn(*id);
            }
        }
        for (i, pid) in watch_pids.iter().enumerate() {
            if ready(&fds[1 + conn_ids.len() + i]) {
                self.read_watch(*pid);
            }
        }
        // Exits whose kqueue event is already queued but not yet polled are
        // read on every turn too (cheap: a zero-timeout kevent each).
        for pid in self.watches.keys().copied().collect::<Vec<_>>() {
            self.read_watch(pid);
        }
        let now = self.now();
        let mut env = self.env();
        let outs = self.core.tick(now, &mut env);
        self.apply(outs);
        self.children.retain(|pid| !sys::reap_if_done(*pid));
        Ok(())
    }

    fn accept_all(&mut self) {
        loop {
            let stream = match self.listener.accept() {
                Ok((s, _)) => s,
                Err(_) => return,
            };
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
            let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
            if self.identity.check(&stream).is_err() {
                self.refused_dials += 1;
                continue;
            }
            let Ok(pid) = aterm_uds::fdpass::peer_pid(&stream) else {
                self.refused_dials += 1;
                continue;
            };
            let birth = born(pid);
            let id = self.next_conn;
            self.next_conn += 1;
            if !self.core.accept(id, pid, birth) {
                self.refused_dials += 1;
                continue;
            }
            // The death witness is registered at the connect, while the kernel
            // has just named the pid (§5.3 step 5).
            if !self.watches.contains_key(&pid)
                && let Some(w) = ExitWatch::watch(pid)
            {
                self.watches.insert(pid, w);
            }
            self.conns.insert(id, Conn { stream, pid });
        }
    }

    fn read_watch(&mut self, pid: u32) {
        let Some(status) = self.watches.get(&pid).and_then(ExitWatch::exit_status) else {
            return;
        };
        self.watches.remove(&pid);
        self.drain_pid(pid);
        let now = self.now();
        let mut env = self.env();
        let outs = self.core.peer_exit(pid, status, now, &mut env);
        self.apply(outs);
    }

    /// Watch `pid`'s exit (a PENDING successor, an orphan's leader). A pid
    /// already gone cannot be watched: whatever waited on it is judged now.
    fn watch_pid(&mut self, pid: u32) {
        if self.watches.contains_key(&pid) {
            return;
        }
        match ExitWatch::watch(pid) {
            Some(w) => {
                self.watches.insert(pid, w);
            }
            None => {
                let mut env = self.env();
                let outs = self.core.prune_dead_orphans(&mut env);
                self.apply(outs);
            }
        }
    }

    fn drop_conn(&mut self, id: ConnId) {
        self.conns.remove(&id);
        let now = self.now();
        let mut env = self.env();
        let outs = self.core.eof(id, now, &mut env);
        self.core.forget_if_passive(id);
        self.apply(outs);
    }

    /// STREAM ORDER (§5.3 step 4, "K reads BYE and then EOF"): every frame a
    /// peer wrote before it died — a claim, its BYE — is read before its death
    /// is recorded. The exit watch is a descriptor of its own, so a turn can
    /// read the exit while frames written before it still wait on the
    /// connection: a turn reads one frame per connection, and the every-turn
    /// sweep reads exits the `poll` never reported. The core refuses a claim
    /// from a peer whose exit it has recorded, so a claim read after the exit
    /// left its master Offered to a dead window — back to Orphaned, the custody
    /// copy kept, the shell never hung up by the quit (measured 2026-09-29:
    /// `kernel_custody`'s quit under load, the keeper the master's only
    /// holder). A dead peer's buffer is finite and ends in EOF, and the
    /// zero-timeout `poll` never waits; [`DRAIN_MAX_FRAMES`] only stops a
    /// connection some live process still writes on.
    fn drain_pid(&mut self, pid: u32) {
        let ids: Vec<ConnId> = self
            .conns
            .iter()
            .filter(|(_, c)| c.pid == pid)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            for _ in 0..DRAIN_MAX_FRAMES {
                let Some(conn) = self.conns.get(&id) else {
                    break;
                };
                let mut pfd = [PollFd {
                    fd: conn.stream.as_raw_fd(),
                    events: POLLIN,
                    revents: 0,
                }];
                match sys::poll(&mut pfd, 0) {
                    Ok(n) if n > 0 && pfd[0].revents & (POLLIN | POLLHUP | POLLERR) != 0 => {
                        if self.read_conn(id) {
                            self.frames_behind_exit += 1;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    _ => break,
                }
            }
        }
    }

    /// Read one frame of `id` and handle it: `true` when a frame was read,
    /// `false` when the connection is over (EOF, a malformed frame, a stall).
    fn read_conn(&mut self, id: ConnId) -> bool {
        let Some(conn) = self.conns.get(&id) else {
            return false;
        };
        let incoming = match read_frame(&conn.stream) {
            Ok(Some(i)) => i,
            // EOF, a malformed frame, a stalled peer: the connection is over.
            Ok(None) | Err(_) => {
                self.drop_conn(id);
                return false;
            }
        };
        self.handle(id, incoming);
        true
    }

    fn handle(&mut self, id: ConnId, incoming: Incoming) {
        let now = self.now();
        let mut env = self.env();
        let Incoming { frame, fd } = incoming;
        let outs = match frame {
            Frame::Hello {
                class,
                caps,
                marker,
                ..
            } => {
                if let Some(marker) = marker {
                    self.core.name_marker(id, marker, &mut env);
                }
                self.core.hello(id, class, caps, now, &mut env)
            }
            Frame::Register { header, tag } => self.register(id, header, tag, fd),
            Frame::Meta { rdev, meta } => self.core.meta(id, rdev, meta),
            Frame::Release { rdev } => self.core.release(id, rdev),
            Frame::Pending { pid } => {
                let birth = born(pid);
                self.core.pending(id, pid, now, birth)
            }
            Frame::Bye => {
                self.core.bye(id);
                Vec::new()
            }
            Frame::Adopt => self.core.adopt(id, &mut env),
            Frame::Status { .. } => {
                let text = self.status_text();
                self.send(id, &Frame::Status { text }, None);
                Vec::new()
            }
            // P5's frame, accepted and not yet acted on.
            Frame::RestartWhenSafe => Vec::new(),
            // Keeper-to-window kinds from a window: a protocol error.
            Frame::Welcome { .. }
            | Frame::Offer { .. }
            | Frame::Orphans { .. }
            | Frame::Refused { .. } => {
                self.drop_conn(id);
                return;
            }
        };
        self.apply(outs);
    }

    /// REGISTER: read the descriptor's rdev (`fstat`, the one call ever made
    /// on a master) and let the core decide.
    fn register(
        &mut self,
        id: ConnId,
        header: MasterHeader,
        tag: Vec<u8>,
        fd: Option<OwnedFd>,
    ) -> Vec<Out> {
        let Some(fd) = fd else {
            return vec![Out::Refused {
                conn: id,
                code: crate::wire::RefuseCode::NoDescriptor,
                rdev: header.rdev,
            }];
        };
        let file = std::fs::File::from(fd);
        let fd_rdev = match file.metadata() {
            Ok(m) if m.file_type().is_char_device() => m.rdev(),
            // Not a character device: no master of any terminal.
            _ => u64::MAX,
        };
        let fd = OwnedFd::from(file);
        let outs = self.core.register(id, header, tag, fd_rdev);
        for out in &outs {
            if *out == Out::Keep(header.rdev) && !self.test_drop_custody {
                self.custody.insert(header.rdev, fd);
                return outs;
            }
        }
        // DropDuplicate, a refusal, or the test seam: the arrived copy closes.
        drop(fd);
        outs
    }

    fn apply(&mut self, outs: Vec<Out>) {
        for out in outs {
            match out {
                Out::Keep(_) | Out::DropDuplicate(_) => {}
                Out::Close(rdev) => {
                    self.custody.remove(&rdev);
                }
                Out::Welcome { conn, offers } => {
                    let frame = Frame::Welcome {
                        proto: PROTO,
                        offers,
                        version: self.version.clone(),
                    };
                    self.send(conn, &frame, None);
                }
                Out::Offer { conn, rdev } => {
                    let Some(rec) = self.core.records().get(&rdev).cloned() else {
                        continue;
                    };
                    let frame = Frame::Offer {
                        header: rec.header,
                        tag: rec.tag,
                        meta: rec.meta,
                    };
                    // The kernel duplicates the description into the
                    // recipient; the custody copy stays here (F9).
                    if let Some(fd) = self
                        .custody
                        .get(&rdev)
                        .map(|f| f.as_fd().try_clone_to_owned())
                    {
                        match fd {
                            Ok(fd) => self.send(conn, &frame, Some(&fd)),
                            Err(_) => self.send_raw_fail(conn),
                        }
                    }
                }
                Out::Orphans { conn, n } => self.send(conn, &Frame::Orphans { n }, None),
                Out::Refused { conn, code, rdev } => {
                    self.send(conn, &Frame::Refused { code, rdev }, None);
                }
                Out::WatchPid(pid) => self.watch_pid(pid),
                Out::Relaunch => self.relaunch_now(),
            }
        }
    }

    fn send_raw_fail(&mut self, conn: ConnId) {
        // A duplicate could not be made (EMFILE): the recipient cannot be told
        // the truth frame by frame, so its connection ends and the offer
        // returns to Orphaned with its death.
        self.drop_conn(conn);
    }

    fn send(&mut self, conn: ConnId, frame: &Frame, fd: Option<&OwnedFd>) {
        let Some(c) = self.conns.get(&conn) else {
            return;
        };
        if write_frame(&c.stream, frame, fd.map(|f| f.as_fd())).is_err() {
            self.drop_conn(conn);
        }
    }

    fn relaunch_now(&mut self) {
        let spawned = match &self.relaunch {
            Relauncher::Unavailable => return,
            Relauncher::OpenBundle(bundle) => {
                let bundle = bundle.to_string_lossy().into_owned();
                aterm_uds::pspawn::spawn_cloexec_default("/usr/bin/open", &["-a", &bundle])
            }
            Relauncher::TestCommand(argv) => match argv.split_first() {
                Some((program, rest)) => {
                    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
                    aterm_uds::pspawn::spawn_cloexec_default(program, &rest)
                }
                None => return,
            },
        };
        // `open`'s exit status is no evidence of a launch (M8): only a HELLO
        // is, which the core waits for.
        if let Ok(pid) = spawned {
            self.children.push(pid);
        }
    }

    /// The status text: who the keeper is, then the core's lines.
    #[must_use]
    pub fn status_text(&self) -> String {
        let mut lines = vec![
            "keeper=running".to_string(),
            format!("pid={} version={}", self.own_pid, self.version),
            format!("socket={}", self.socket.display()),
            format!("identity={}", self.identity.describe()),
            format!("custody_copies={}", self.custody.len()),
            format!("refused_dials={}", self.refused_dials),
            format!("frames_behind_exit={}", self.frames_behind_exit),
        ];
        lines.extend(self.core.status_lines());
        let mut text = lines.join("\n");
        text.truncate(crate::wire::MAX_STATUS);
        text
    }

    /// The core, for in-process tests.
    #[must_use]
    pub fn core(&self) -> &KeeperCore {
        &self.core
    }

    /// Whether a custody copy of `rdev` is held.
    #[must_use]
    pub fn holds(&self, rdev: Rdev) -> bool {
        self.custody.contains_key(&rdev)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// Create `dir` if missing (0700) and require it to be a real directory of
/// ours that no one else can write.
fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new().mode(0o700).create(dir)?;
        }
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir()
        || meta.uid() != aterm_uds::peer::our_uid()
        || meta.permissions().mode() & 0o022 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a private directory of ours", dir.display()),
        ));
    }
    Ok(())
}

/// After the bind: the node is a socket of ours inside our real directory.
fn prove_socket_is_ours(dir: &Path, path: &Path) -> io::Result<()> {
    ensure_private_dir(dir)?;
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.file_type().is_socket() || meta.uid() != aterm_uds::peer::our_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the bound node is not a socket of ours",
        ));
    }
    Ok(())
}
