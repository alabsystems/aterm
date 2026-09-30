// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WINDOW'S SIDE OF AKP1: [`KeeperClient`], one blocking connection, and
//! [`KeeperLink`], the worker thread with a bounded queue a window drives it
//! through (§5.3 step 1: the main thread makes no socket call; the queue holds
//! DUPLICATES, never descriptor numbers, so recycling cannot misroute a PTY).
//!
//! The client checks the KEEPER too before it sends a single master (§5.7):
//! the same uid, and under [`IdentityPolicy::Designated`] the same code — a
//! same-uid process that bound the socket first would otherwise receive a
//! live duplicate of every terminal.
//!
//! The window drives it through `aterm-gui`'s `keeper_link` (P3, opt-in): the
//! boot HELLO on a [`KeeperClient`], then a [`KeeperLink`] started on that same
//! connection.
//!
//! [`IdentityPolicy::Designated`]: crate::identity::IdentityPolicy::Designated

use std::io;
use std::os::fd::{AsFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::time::Duration;

use aterm_uds::CtlStream;

use crate::frameio::{Incoming, read_frame, write_frame};
use crate::identity::{CheckError, Identity};
use crate::wire::{CAP_LINK, CAP_SENDS_BYE, Frame, MarkerRef, MasterHeader, PROTO, PeerClass};

/// One OFFER as the window receives it: the master, its fixed header, and the
/// two opaque blobs.
#[derive(Debug)]
pub struct Offered {
    pub master: OwnedFd,
    pub header: MasterHeader,
    pub tag: Vec<u8>,
    pub meta: Vec<u8>,
}

/// A keeper that answered but failed this build's identity check (§5.7): the
/// payload of the `PermissionDenied` error [`KeeperClient::connect`] returns,
/// so a caller can tell it from a socket it may not open
/// ([`refused_identity`]).
#[derive(Debug)]
pub struct NotThisBuild(pub String);

impl std::fmt::Display for NotThisBuild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotThisBuild {}

/// Whether `e` is [`KeeperClient::connect`] refusing a keeper that answered
/// but is not this build's.
#[must_use]
pub fn refused_identity(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<NotThisBuild>())
}

/// The error [`KeeperClient::connect`] returns for a failed identity check:
/// a refusal is a [`NotThisBuild`] inside `PermissionDenied`; a check that
/// could not run keeps its own words.
fn check_failed(e: CheckError) -> io::Error {
    match e {
        CheckError::Refused(why) => {
            io::Error::new(io::ErrorKind::PermissionDenied, NotThisBuild(why))
        }
        CheckError::Unchecked(why) => io::Error::other(why),
    }
}

/// One connection to a keeper.
#[derive(Debug)]
pub struct KeeperClient {
    stream: CtlStream,
}

impl KeeperClient {
    /// Dial `path`, check the keeper's identity, and set a read timeout.
    ///
    /// # Errors
    /// The dial failing, the keeper failing the identity check (a
    /// [`NotThisBuild`] inside `PermissionDenied`), or the check not running.
    pub fn connect(path: &Path, identity: &Identity, timeout: Duration) -> io::Result<Self> {
        let stream = CtlStream::connect(path)?;
        identity.check(&stream).map_err(check_failed)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        Ok(Self { stream })
    }

    /// Send a frame (with its descriptor, for REGISTER).
    ///
    /// # Errors
    /// The write failing.
    pub fn send(&self, frame: &Frame, fd: Option<&OwnedFd>) -> io::Result<()> {
        write_frame(&self.stream, frame, fd.map(|f| f.as_fd()))
    }

    /// Read the next frame; `None` at EOF.
    ///
    /// # Errors
    /// A malformed frame, the timeout, or the socket's errors.
    pub fn recv(&self) -> io::Result<Option<Incoming>> {
        read_frame(&self.stream)
    }

    /// HELLO as a window, and collect the WELCOME's offers.
    ///
    /// # Errors
    /// The exchange failing or the keeper answering out of protocol.
    pub fn hello_app(&self, build: &str) -> io::Result<Vec<Offered>> {
        self.hello_app_marked(build, None)
    }

    /// [`Self::hello_app`], naming this window's crash marker (§5.4 row 3's
    /// cross-check): the keeper then reads a lost BYE with the marker gone and
    /// `exit(0)` as the quit it was.
    ///
    /// # Errors
    /// The exchange failing or the keeper answering out of protocol.
    pub fn hello_app_marked(
        &self,
        build: &str,
        marker: Option<MarkerRef>,
    ) -> io::Result<Vec<Offered>> {
        self.send(
            &Frame::Hello {
                proto: PROTO,
                class: PeerClass::App,
                caps: CAP_SENDS_BYE,
                build: build.to_string(),
                marker,
            },
            None,
        )?;
        let n = match self.recv()?.map(|i| i.frame) {
            Some(Frame::Welcome { offers, .. }) => offers,
            other => return Err(protocol(format!("expected WELCOME, got {other:?}"))),
        };
        let mut offers = Vec::with_capacity(usize::from(n));
        for _ in 0..n {
            match self.recv()? {
                Some(Incoming {
                    frame: Frame::Offer { header, tag, meta },
                    fd: Some(master),
                }) => offers.push(Offered {
                    master,
                    header,
                    tag,
                    meta,
                }),
                other => return Err(protocol(format!("expected OFFER, got {other:?}"))),
            }
        }
        Ok(offers)
    }

    /// HELLO as a window's link ([`CAP_LINK`]): registered masters only, no
    /// offers, not a launch.
    ///
    /// # Errors
    /// The exchange failing or the keeper answering out of protocol.
    pub fn hello_link(&self, build: &str, marker: Option<MarkerRef>) -> io::Result<()> {
        self.send(
            &Frame::Hello {
                proto: PROTO,
                class: PeerClass::App,
                caps: CAP_SENDS_BYE | CAP_LINK,
                build: build.to_string(),
                marker,
            },
            None,
        )?;
        match self.recv()?.map(|i| i.frame) {
            Some(Frame::Welcome { offers: 0, .. }) => Ok(()),
            other => Err(protocol(format!(
                "expected an empty WELCOME, got {other:?}"
            ))),
        }
    }

    /// HELLO as a status client, then ask for the status text.
    ///
    /// # Errors
    /// The exchange failing or the keeper answering out of protocol.
    pub fn status(&self) -> io::Result<String> {
        self.send(
            &Frame::Hello {
                proto: PROTO,
                class: PeerClass::Status,
                caps: 0,
                build: env!("CARGO_PKG_VERSION").to_string(),
                marker: None,
            },
            None,
        )?;
        match self.recv()?.map(|i| i.frame) {
            Some(Frame::Welcome { .. }) => {}
            other => return Err(protocol(format!("expected WELCOME, got {other:?}"))),
        }
        self.send(
            &Frame::Status {
                text: String::new(),
            },
            None,
        )?;
        match self.recv()?.map(|i| i.frame) {
            Some(Frame::Status { text }) => Ok(text),
            other => Err(protocol(format!("expected STATUS, got {other:?}"))),
        }
    }

    /// Read and discard every frame already waiting (a link reads nothing it
    /// acts on); `false` once the keeper has gone (EOF, a malformed frame).
    /// Never waits for a frame that has not begun to arrive.
    #[must_use]
    pub fn drain(&self) -> bool {
        use std::os::fd::AsRawFd as _;
        loop {
            let mut fds = [crate::sys::PollFd {
                fd: self.stream.as_raw_fd(),
                events: crate::sys::POLLIN,
                revents: 0,
            }];
            match crate::sys::poll(&mut fds, 0) {
                Ok(0) => return true,
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return false,
            }
            match self.recv() {
                // An OFFER's descriptor, were one ever sent here, closes.
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => return false,
            }
        }
    }

    /// REGISTER a master (a duplicate the caller made for the keeper).
    ///
    /// # Errors
    /// The write failing.
    pub fn register(&self, master: &OwnedFd, header: MasterHeader, tag: Vec<u8>) -> io::Result<()> {
        self.send(&Frame::Register { header, tag }, Some(master))
    }
}

fn protocol(why: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

/// One message to the keeper, queued by the window.
#[derive(Debug)]
pub enum LinkOp {
    /// A fresh or adopted master: the queue owns this DUPLICATE.
    Register {
        master: OwnedFd,
        header: MasterHeader,
        tag: Vec<u8>,
    },
    /// The newest META of a master. The link keeps the latest per master and
    /// tells a restarted keeper again.
    Meta { rdev: u64, meta: Vec<u8> },
    /// A tab or pane closed.
    Release { rdev: u64 },
    /// An update's successor, before its grant.
    Pending { pid: u32 },
    /// The quit, written on the final-exit path just before `exit(0)`. The
    /// link sends nothing after it.
    Bye,
}

/// The queue's bound: a window never blocks on the keeper.
pub const LINK_QUEUE: usize = 256;

/// One queued op, and who waits for it to be written (PENDING before the
/// grant, BYE before `exit(0)`).
type Queued = (LinkOp, Option<SyncSender<bool>>);

/// The window's link: a worker thread that owns the connection, reconnects
/// after a keeper restart and re-registers every master it still holds a
/// duplicate of (§5.3 step 9).
#[derive(Debug)]
pub struct KeeperLink {
    tx: SyncSender<Queued>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl KeeperLink {
    /// Start the worker for the keeper at `path`.
    ///
    /// # Errors
    /// The thread failing to spawn.
    pub fn start(path: PathBuf, identity: Identity, build: String) -> io::Result<Self> {
        Self::start_on(path, identity, build, None)
    }

    /// [`Self::start`], naming this window's crash marker in every link HELLO
    /// (each connection is judged on its own, §5.4).
    ///
    /// # Errors
    /// The thread failing to spawn.
    pub fn start_marked(
        path: PathBuf,
        identity: Identity,
        build: String,
        marker: Option<MarkerRef>,
        booted: Option<KeeperClient>,
    ) -> io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel(LINK_QUEUE);
        let thread = std::thread::Builder::new()
            .name("aterm-keeper-link".to_string())
            .spawn(move || link_worker(&path, &identity, &build, marker.as_ref(), &rx, booted))?;
        Ok(Self {
            tx,
            thread: Some(thread),
        })
    }

    /// Start the worker on a connection that already said the window's boot
    /// HELLO ([`KeeperClient::hello_app`]): the masters it was offered are
    /// claimed on that same connection, so the keeper sees one window, whose
    /// end it judges once. A dropped connection is replaced as [`Self::start`]'s
    /// is (a link HELLO, not a launch).
    ///
    /// # Errors
    /// The thread failing to spawn.
    pub fn start_on(
        path: PathBuf,
        identity: Identity,
        build: String,
        booted: Option<KeeperClient>,
    ) -> io::Result<Self> {
        Self::start_marked(path, identity, build, None, booted)
    }

    /// Queue `op`; `false` when the queue is full or the worker is gone (the
    /// window carries on: custody is insurance, not state).
    pub fn enqueue(&self, op: LinkOp) -> bool {
        match self.tx.try_send((op, None)) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        }
    }

    /// Queue `op` and wait at most `within` for the worker to have WRITTEN it
    /// to a keeper: `true` only then. Every op queued before it is written
    /// first. `false` — no keeper answered, the queue was full, or the wait ran
    /// out — never blocks the caller past `within`.
    pub fn send_and_wait(&self, op: LinkOp, within: Duration) -> bool {
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        if self.tx.try_send((op, Some(ack_tx))).is_err() {
            return false;
        }
        ack_rx.recv_timeout(within).unwrap_or(false)
    }

    /// Close the queue and wait for the worker to drain it.
    pub fn shutdown(mut self) {
        let (tx, _) = mpsc::sync_channel(1);
        drop(std::mem::replace(&mut self.tx, tx));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

type Held = std::collections::BTreeMap<u64, (OwnedFd, MasterHeader, Vec<u8>)>;

/// What one queued op sends once connected.
enum Send {
    Register(u64),
    Frame(Frame),
}

/// How often an idle link looks at its connection: it drains what the keeper
/// sent (ORPHANS, REFUSED — a link reads nothing else, and an unread socket
/// would stall the keeper's writes) and notices a keeper that went away, so a
/// restarted keeper is told what this window holds without waiting for the
/// window's next op (§5.3 step 9).
pub const LINK_POLL: Duration = Duration::from_millis(500);

fn link_worker(
    path: &Path,
    identity: &Identity,
    build: &str,
    marker: Option<&MarkerRef>,
    rx: &Receiver<Queued>,
    booted: Option<KeeperClient>,
) {
    // The duplicates this window still holds for the keeper, by rdev, so a
    // restarted keeper can be told again — and the newest META of each.
    let mut held: Held = std::collections::BTreeMap::new();
    let mut metas: std::collections::BTreeMap<u64, Vec<u8>> = std::collections::BTreeMap::new();
    let mut client: Option<KeeperClient> = booted;
    let mut backoff = Duration::from_millis(50);
    let mut next_try = std::time::Instant::now();
    loop {
        let (op, ack) = match rx.recv_timeout(LINK_POLL) {
            Ok((op, ack)) => (Some(op), ack),
            Err(mpsc::RecvTimeoutError::Timeout) => (None, None),
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        if client.as_ref().is_some_and(|c| !c.drain()) {
            client = None;
        }
        let bye = matches!(op, Some(LinkOp::Bye));
        let send = op.and_then(|op| match op {
            LinkOp::Register {
                master,
                header,
                tag,
            } => {
                held.insert(header.rdev, (master, header, tag));
                Some(Send::Register(header.rdev))
            }
            // A master this link never registered is nothing to release.
            LinkOp::Release { rdev } => {
                metas.remove(&rdev);
                held.remove(&rdev)
                    .map(|_| Send::Frame(Frame::Release { rdev }))
            }
            LinkOp::Meta { rdev, meta } => {
                if !held.contains_key(&rdev) {
                    return None;
                }
                metas.insert(rdev, meta.clone());
                Some(Send::Frame(Frame::Meta { rdev, meta }))
            }
            LinkOp::Pending { pid } => Some(Send::Frame(Frame::Pending { pid })),
            LinkOp::Bye => Some(Send::Frame(Frame::Bye)),
        });
        // An idle link with nothing held has nothing to tell a keeper.
        if send.is_none() && held.is_empty() {
            if let Some(ack) = ack {
                let _ = ack.try_send(false);
            }
            continue;
        }
        let fresh = client.is_none();
        // A caller waiting on this op does not wait out a backoff.
        if fresh && (ack.is_some() || std::time::Instant::now() >= next_try) {
            client = reconnect(path, identity, build, marker, &held, &metas);
            if client.is_some() {
                backoff = Duration::from_millis(50);
            } else {
                next_try = std::time::Instant::now() + backoff;
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        }
        let (Some(c), Some(send)) = (client.as_ref(), send) else {
            // No keeper (the op is dropped, and what is held is told to the
            // next keeper at the reconnect), or an idle turn.
            if let Some(ack) = ack {
                let _ = ack.try_send(false);
            }
            continue;
        };
        let sent = match &send {
            // A fresh connection already registered everything held.
            Send::Register(_) if fresh => Ok(()),
            Send::Register(rdev) => match held.get(rdev) {
                Some((fd, header, tag)) => c.register(fd, *header, tag.clone()),
                None => Ok(()),
            },
            // …and told it every META.
            Send::Frame(Frame::Meta { .. }) if fresh => Ok(()),
            Send::Frame(frame) => c.send(frame, None),
        };
        if let Some(ack) = ack {
            let _ = ack.try_send(sent.is_ok());
        }
        if sent.is_err() {
            client = None;
        }
        if bye {
            // Nothing follows a quit: the window is about to `exit(0)`, and the
            // keeper reads the BYE, then this connection's end.
            return;
        }
    }
}

fn reconnect(
    path: &Path,
    identity: &Identity,
    build: &str,
    marker: Option<&MarkerRef>,
    held: &Held,
    metas: &std::collections::BTreeMap<u64, Vec<u8>>,
) -> Option<KeeperClient> {
    let c = KeeperClient::connect(path, identity, Duration::from_secs(2)).ok()?;
    // A link registers; it takes no offers (the boot HELLO does, through the
    // window's admission).
    c.hello_link(build, marker.cloned()).ok()?;
    for (fd, header, tag) in held.values() {
        c.register(fd, *header, tag.clone()).ok()?;
    }
    for (rdev, meta) in metas {
        c.send(
            &Frame::Meta {
                rdev: *rdev,
                meta: meta.clone(),
            },
            None,
        )
        .ok()?;
    }
    Some(c)
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use super::*;
    use crate::server::{Relauncher, ServeConfig, Server};
    use crate::wire::Birth;
    use std::os::unix::fs::MetadataExt as _;
    use std::time::Instant;

    fn serve(sock: &Path) -> Server {
        Server::bind(ServeConfig {
            socket: sock.to_path_buf(),
            identity: Identity::same_uid(),
            relaunch: Relauncher::Unavailable,
            version: "test".into(),
            test_drop_custody: false,
        })
        .expect("bind")
    }

    fn step_until(server: &mut Server, what: &str, done: impl Fn(&Server) -> bool) {
        // A hang detector, not a latency budget.
        let deadline = Instant::now() + Duration::from_secs(60);
        while !done(server) {
            assert!(Instant::now() < deadline, "never: {what}");
            server.step(20).expect("step");
        }
    }

    /// A listener that is not this build's code — Apple's `nc` — answers on
    /// the socket: the designated check refuses it, and the error says so
    /// ([`refused_identity`]). NEGATIVE CONTROLS: no socket at all, and a
    /// socket of ours this process may not open (also `PermissionDenied`),
    /// are not read as that refusal.
    #[test]
    fn a_listener_of_other_code_is_refused_as_not_this_build() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("akl-other-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let own = Identity::for_self(crate::identity::IdentityPolicy::Designated).expect("own");

        let sock = dir.join("nc.sock");
        let mut nc = std::process::Command::new("/usr/bin/nc")
            .arg("-dlU")
            .arg(&sock)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn nc");
        // A hang detector: `nc` binds, then listens; a dial between the two is
        // refused, so the dial is retried until it reaches the listener.
        let started = Instant::now();
        let refused = loop {
            match KeeperClient::connect(&sock, &own, Duration::from_secs(2)) {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    assert!(started.elapsed().as_secs() < 60, "nc never listened: {e}");
                    std::thread::sleep(Duration::from_millis(5));
                }
                other => break other,
            }
        };
        let _ = nc.kill();
        let _ = nc.wait();
        let e = refused.expect_err("nc is not this build's code");
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
        assert!(refused_identity(&e), "{e}");

        let absent = KeeperClient::connect(&dir.join("none.sock"), &own, Duration::from_secs(2))
            .expect_err("nothing there");
        assert!(!refused_identity(&absent), "{absent}");

        let closed = dir.join("closed.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&closed).expect("bind");
        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        let denied = KeeperClient::connect(&closed, &own, Duration::from_secs(2))
            .expect_err("a socket this process may not open");
        assert_eq!(denied.kind(), io::ErrorKind::PermissionDenied, "{denied}");
        assert!(!refused_identity(&denied), "{denied}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Only a peer the check refused is [`NotThisBuild`]; a check that could
    /// not run (no audit token, the peer's code not found) keeps its words
    /// and is not read as that refusal.
    #[test]
    fn only_a_refused_check_is_not_this_build() {
        let refused = check_failed(CheckError::Refused("OSStatus -67050".into()));
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert!(refused_identity(&refused), "{refused}");
        let unchecked = check_failed(CheckError::Unchecked(
            "the kernel did not give the peer's audit token".into(),
        ));
        assert!(!refused_identity(&unchecked), "{unchecked}");
        assert_eq!(
            unchecked.to_string(),
            "the kernel did not give the peer's audit token"
        );
    }

    /// §5.3 step 9: a keeper that restarts is told again what the window
    /// holds, WITHOUT waiting for the window's next op — a window that crashes
    /// in a quiet stretch after a keeper restart must still find its masters
    /// in custody.
    #[test]
    fn a_restarted_keeper_is_told_again_without_a_new_op() {
        let base = std::env::temp_dir();
        let dir = base.join(format!("akl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sock = dir.join("k.sock");
        let mut first = serve(&sock);
        let master = std::fs::File::open("/dev/null").expect("open");
        let rdev = master.metadata().expect("stat").rdev();
        let link = KeeperLink::start(sock.clone(), Identity::same_uid(), "t".into()).expect("link");
        assert!(link.enqueue(LinkOp::Register {
            master: OwnedFd::from(master),
            header: MasterHeader {
                rdev,
                shell_pid: std::process::id(),
                shell_birth: Birth::default(),
                local_id: 1,
            },
            tag: Vec::new(),
        }));
        step_until(&mut first, "the first keeper holds it", |s| s.holds(rdev));
        drop(first);
        let mut second = serve(&sock);
        step_until(&mut second, "the restarted keeper holds it", |s| {
            s.holds(rdev)
        });
        link.shutdown();
        drop(second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// PENDING and BYE are WAITED for: the window's update worker gates the
    /// grant on the PENDING having been written, and the final exit writes the
    /// BYE before `exit(0)`. Both come back `true` only once written to a live
    /// keeper, the keeper reads them in order, and nothing follows a BYE.
    #[test]
    fn pending_and_bye_are_acknowledged_once_written() {
        let base = std::env::temp_dir();
        let dir = base.join(format!("akl-ack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sock = dir.join("k.sock");
        let mut server = serve(&sock);
        let master = std::fs::File::open("/dev/null").expect("open");
        let rdev = master.metadata().expect("stat").rdev();
        let link = KeeperLink::start(sock.clone(), Identity::same_uid(), "t".into()).expect("link");
        assert!(link.enqueue(LinkOp::Register {
            master: OwnedFd::from(master),
            header: MasterHeader {
                rdev,
                shell_pid: std::process::id(),
                shell_birth: Birth::default(),
                local_id: 1,
            },
            tag: Vec::new(),
        }));
        // The keeper is stepped on this thread, so the waits run on another.
        let waiter = std::thread::spawn(move || {
            let pending = link.send_and_wait(LinkOp::Pending { pid: 1 }, Duration::from_secs(60));
            let bye = link.send_and_wait(LinkOp::Bye, Duration::from_secs(60));
            // Nothing is written after the BYE: the worker has ended.
            let after = link.send_and_wait(LinkOp::Release { rdev }, Duration::from_millis(200));
            (pending, bye, after)
        });
        step_until(&mut server, "the keeper read the BYE", |s| {
            s.core().peers().values().any(|p| p.bye)
        });
        let (pending, bye, after) = waiter.join().expect("waiter");
        assert!(pending && bye, "written: pending={pending} bye={bye}");
        assert!(!after, "no op is written after the BYE");
        assert_eq!(
            server.core().pending_successor().map(|p| p.pid),
            Some(1),
            "the PENDING arrived before the BYE"
        );
        assert!(server.holds(rdev), "the RELEASE after the BYE never came");
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With no keeper at all, a waited op says so promptly rather than
    /// holding the window's update or exit for the whole wait.
    #[test]
    fn a_waited_op_with_no_keeper_is_refused_promptly() {
        let base = std::env::temp_dir();
        let dir = base.join(format!("akl-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sock = dir.join("absent.sock");
        let link = KeeperLink::start(sock, Identity::same_uid(), "t".into()).expect("link");
        let master = std::fs::File::open("/dev/null").expect("open");
        let rdev = master.metadata().expect("stat").rdev();
        assert!(link.enqueue(LinkOp::Register {
            master: OwnedFd::from(master),
            header: MasterHeader {
                rdev,
                shell_pid: std::process::id(),
                shell_birth: Birth::default(),
                local_id: 1,
            },
            tag: Vec::new(),
        }));
        let started = Instant::now();
        assert!(!link.send_and_wait(LinkOp::Pending { pid: 1 }, Duration::from_secs(60)));
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "refused without waiting out the bound: {:?}",
            started.elapsed()
        );
        link.shutdown();
    }
}
