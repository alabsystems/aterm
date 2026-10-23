// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's side of AKP1: [`KeeperClient`] and [`KeeperLink`].

use std::cell::Cell;
use std::io;
use std::os::fd::{AsFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::time::Duration;

use aterm_uds::CtlStream;

use crate::frameio::{Incoming, read_frame, write_frame};
use crate::identity::{CheckError, Identity, IdentityPolicy};
use crate::wire::{
    CAP_JOURNAL, CAP_LINK, CAP_SENDS_BYE, CAP_SYNC, COMPANION_JOURNAL, Frame, JournalOffer,
    MarkerRef, MasterHeader, PROTO, PeerClass,
};

/// One OFFER as the window receives it.
#[derive(Debug)]
pub struct Offered {
    pub master: OwnedFd,
    pub header: MasterHeader,
    pub tag: Vec<u8>,
    pub meta: Vec<u8>,
    /// Crash-restore region offered with the master.
    pub companion: Option<OfferedCompanion>,
    /// Keeper poison brake (`JournalOffer::attempts`).
    pub attempts: u8,
}

/// Crash-restore journal region offered by a keeper.
#[derive(Debug)]
pub struct OfferedCompanion {
    pub region: OwnedFd,
    pub kind: u8,
    pub len: u64,
}

/// Error payload when a keeper fails this build's identity check or states a weaker policy.
#[derive(Debug)]
pub struct NotThisBuild(pub String);

impl std::fmt::Display for NotThisBuild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotThisBuild {}

/// Whether `e` is [`KeeperClient::connect`] refusing a keeper that is not this build's.
#[must_use]
pub fn refused_identity(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<NotThisBuild>())
}

fn check_failed(e: CheckError) -> io::Error {
    match e {
        CheckError::Refused(why) => {
            io::Error::new(io::ErrorKind::PermissionDenied, NotThisBuild(why))
        }
        CheckError::Unchecked(why) | CheckError::Unanswered(why) => io::Error::other(why),
    }
}

/// Minimum wait for a HELLO's answer from the keeper.
pub const WELCOME_WAIT: Duration = crate::identity::PEER_CHECK_HANG.saturating_add(FRAME_BUDGET);

const FRAME_BUDGET: Duration = Duration::from_secs(2);

/// One connection to a keeper.
#[derive(Debug)]
pub struct KeeperClient {
    stream: CtlStream,
    /// The read and write timeout [`Self::connect`] set: every frame's but a HELLO's answer,
    /// which waits [`WELCOME_WAIT`] at least ([`Self::answer`]).
    frame_wait: Duration,
    /// The policy this client checked the keeper under, which the keeper's
    /// own must be no weaker than ([`KeeperClient::served`]).
    policy: IdentityPolicy,
    /// The capabilities the keeper's WELCOME stated (0 before it, or from an older keeper):
    /// a window sends companions only to one that states [`CAP_JOURNAL`], and SYNC only to one
    /// that states [`CAP_SYNC`].
    keeper_caps: Cell<u8>,
    /// The number of the last SYNC written on this connection ([`Self::sync`]).
    syncs: Cell<u64>,
    /// The highest SYNC number the keeper wrote back ([`Self::drain`] reads it): every frame
    /// written before that SYNC was read.
    synced: Cell<u64>,
}

impl KeeperClient {
    /// Dial `path`, check the keeper's identity within `timeout`, and set a read timeout.
    /// # Errors
    /// The dial failing, the keeper failing the identity check (a [`NotThisBuild`] inside `PermissionDenied`), or the check not running (trustd not answering within `timeout` among the reasons).
    pub fn connect(path: &Path, identity: &Identity, timeout: Duration) -> io::Result<Self> {
        let stream = CtlStream::connect(path)?;
        identity.check(&stream, timeout).map_err(check_failed)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        Ok(Self {
            stream,
            frame_wait: timeout,
            policy: identity.policy(),
            keeper_caps: Cell::new(0),
            syncs: Cell::new(0),
            synced: Cell::new(0),
        })
    }

    /// Whether the keeper's WELCOME stated it takes crash-restore companions.
    #[must_use]
    pub const fn keeper_journals(&self) -> bool {
        self.keeper_caps.get() & CAP_JOURNAL != 0
    }

    /// Whether the keeper's WELCOME stated it answers SYNC.
    #[must_use]
    pub const fn keeper_syncs(&self) -> bool {
        self.keeper_caps.get() & CAP_SYNC != 0
    }

    /// Ask the keeper to confirm when it has read every frame written so far.
    /// # Errors
    /// The keeper did not state SYNC support, the sequence is exhausted, or the write failed.
    pub fn sync(&self) -> io::Result<u64> {
        if !self.keeper_syncs() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the keeper did not state SYNC support",
            ));
        }
        let seq = self
            .syncs
            .get()
            .checked_add(1)
            .ok_or_else(|| io::Error::other("the keeper SYNC sequence is exhausted"))?;
        self.send(&Frame::Sync { seq }, None)?;
        self.syncs.set(seq);
        Ok(seq)
    }

    /// The highest SYNC number the keeper answered on this connection.
    #[must_use]
    pub const fn synced(&self) -> u64 {
        self.synced.get()
    }

    fn welcomed(&self, identity: Option<IdentityPolicy>, caps: Option<u8>) -> io::Result<()> {
        self.served(identity)?;
        self.keeper_caps.set(caps.unwrap_or(0));
        Ok(())
    }

    fn served(&self, served: Option<IdentityPolicy>) -> io::Result<()> {
        match (self.policy, served) {
            (IdentityPolicy::SameUid, _) | (_, Some(IdentityPolicy::Designated)) => Ok(()),
            (IdentityPolicy::Designated, other) => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                NotThisBuild(match other {
                    Some(IdentityPolicy::SameUid) => {
                        "the keeper admits any same-uid peer (--identity uid), not only this \
                         build's code"
                            .to_string()
                    }
                    _ => "the keeper does not say it admits only this build's code (an older \
                          keeper)"
                        .to_string(),
                }),
            )),
        }
    }

    /// Send a frame and optional descriptor.
    /// # Errors
    /// The write failing.
    pub fn send(&self, frame: &Frame, fd: Option<&OwnedFd>) -> io::Result<()> {
        write_frame(&self.stream, frame, fd.map(|f| f.as_fd()))
    }

    /// Read the next frame, returning `None` at EOF.
    /// # Errors
    /// A malformed frame, the timeout, or the socket's errors.
    pub fn recv(&self) -> io::Result<Option<Incoming>> {
        read_frame(&self.stream)
    }

    fn answer<T>(&self, read: impl FnOnce(&Self) -> io::Result<T>) -> io::Result<T> {
        self.stream
            .set_read_timeout(Some(WELCOME_WAIT.max(self.frame_wait)))?;
        let answer = read(self);
        self.stream.set_read_timeout(Some(self.frame_wait))?;
        answer
    }

    /// Send HELLO as a window and collect the WELCOME's offers.
    /// # Errors
    /// The exchange failing or the keeper answering out of protocol.
    pub fn hello_app(&self, build: &str) -> io::Result<Vec<Offered>> {
        self.hello_app_marked(build, None, false)
    }

    /// Send [`Self::hello_app`] naming this window's crash marker.
    /// # Errors
    /// The exchange failing or the keeper answering out of protocol.
    pub fn hello_app_marked(
        &self,
        build: &str,
        marker: Option<MarkerRef>,
        journal: bool,
    ) -> io::Result<Vec<Offered>> {
        self.send(
            &Frame::Hello {
                proto: PROTO,
                class: PeerClass::App,
                caps: CAP_SENDS_BYE | if journal { CAP_JOURNAL } else { 0 },
                build: build.to_string(),
                marker,
            },
            None,
        )?;
        self.answer(|c| c.welcome_offers(journal))
    }

    fn welcome_offers(&self, journal: bool) -> io::Result<Vec<Offered>> {
        let n = match self.recv()?.map(|i| i.frame) {
            Some(Frame::Welcome {
                offers,
                identity,
                caps,
                ..
            }) => {
                self.welcomed(identity, caps)?;
                offers
            }
            other => return Err(protocol(format!("expected WELCOME, got {other:?}"))),
        };
        let mut offers = Vec::with_capacity(usize::from(n));
        for _ in 0..n {
            let (master, header, tag, meta, trail) = match self.recv()? {
                Some(Incoming {
                    frame:
                        Frame::Offer {
                            header,
                            tag,
                            meta,
                            journal: trail,
                        },
                    fd: Some(master),
                }) => (master, header, tag, meta, trail),
                other => return Err(protocol(format!("expected OFFER, got {other:?}"))),
            };
            let JournalOffer {
                attempts,
                companions,
            } = trail.filter(|_| journal).unwrap_or_default();
            let companion = match companions {
                0 => None,
                1 => Some(self.companion_offer(header.rdev)?),
                n => return Err(protocol(format!("an OFFER of {n} companions"))),
            };
            offers.push(Offered {
                master,
                header,
                tag,
                meta,
                companion,
                attempts,
            });
        }
        Ok(offers)
    }

    fn companion_offer(&self, rdev: u64) -> io::Result<OfferedCompanion> {
        match self.recv()? {
            Some(Incoming {
                frame:
                    Frame::CompanionOffer {
                        rdev: of,
                        kind,
                        len,
                    },
                fd: Some(region),
            }) if of == rdev => Ok(OfferedCompanion { region, kind, len }),
            other => Err(protocol(format!(
                "expected the COMPANION_OFFER of rdev {rdev}, got {other:?}"
            ))),
        }
    }

    /// Send HELLO as a window's link ([`CAP_LINK`]).
    /// # Errors
    /// The exchange failing or the keeper answering out of protocol.
    pub fn hello_link(
        &self,
        build: &str,
        marker: Option<MarkerRef>,
        journal: bool,
    ) -> io::Result<()> {
        self.send(
            &Frame::Hello {
                proto: PROTO,
                class: PeerClass::App,
                caps: CAP_SENDS_BYE | CAP_LINK | if journal { CAP_JOURNAL } else { 0 },
                build: build.to_string(),
                marker,
            },
            None,
        )?;
        self.answer(|c| match c.recv()?.map(|i| i.frame) {
            Some(Frame::Welcome {
                offers: 0,
                identity,
                caps,
                ..
            }) => c.welcomed(identity, caps),
            other => Err(protocol(format!(
                "expected an empty WELCOME, got {other:?}"
            ))),
        })
    }

    /// Send HELLO as a status client and ask for the status text.
    /// # Errors
    /// The exchange failing or the keeper answering out of protocol.
    pub fn status(&self, build: &str) -> io::Result<String> {
        self.send(
            &Frame::Hello {
                proto: PROTO,
                class: PeerClass::Status,
                caps: 0,
                build: build.to_string(),
                marker: None,
            },
            None,
        )?;
        self.answer(|c| match c.recv()?.map(|i| i.frame) {
            Some(Frame::Welcome { .. }) => Ok(()),
            other => Err(protocol(format!("expected WELCOME, got {other:?}"))),
        })?;
        self.send(
            &Frame::Status {
                text: String::new(),
            },
            None,
        )?;
        self.answer(|c| match c.recv()?.map(|i| i.frame) {
            Some(Frame::Status { text }) => Ok(text),
            other => Err(protocol(format!("expected STATUS, got {other:?}"))),
        })
    }

    /// Read every frame already waiting, keeping only a SYNC answer; returns `false` once the keeper is gone.
    #[must_use]
    pub fn drain(&self) -> bool {
        self.drain_with_refusals(|_| {})
    }

    fn drain_with_refusals(&self, mut refused: impl FnMut(u64)) -> bool {
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
                Ok(Some(Incoming {
                    frame: Frame::Sync { seq },
                    ..
                })) => self
                    .synced
                    .set(self.synced.get().max(seq.min(self.syncs.get()))),
                Ok(Some(Incoming {
                    frame: Frame::Refused { rdev, .. },
                    ..
                })) => refused(rdev),
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => return false,
            }
        }
    }

    /// Register a master duplicate with the keeper.
    /// # Errors
    /// The write failing.
    pub fn register(&self, master: &OwnedFd, header: MasterHeader, tag: Vec<u8>) -> io::Result<()> {
        self.send(&Frame::Register { header, tag }, Some(master))
    }

    /// Send the read-only custody descriptor of `rdev`'s journal region.
    /// # Errors
    /// The write failing.
    pub fn companion(&self, rdev: u64, region: &OwnedFd, len: u64) -> io::Result<()> {
        self.send(
            &Frame::Companion {
                rdev,
                kind: COMPANION_JOURNAL,
                len,
            },
            Some(region),
        )
    }

    /// Tell the keeper that this connection's claim on `rdev` has no journal region.
    /// # Errors
    /// The write failing.
    pub fn companion_release(&self, rdev: u64) -> io::Result<()> {
        self.send(
            &Frame::CompanionRelease {
                rdev,
                kind: COMPANION_JOURNAL,
            },
            None,
        )
    }
}

fn protocol(why: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

/// A replacement region reached the keeper only after a SYNC behind its COMPANION was answered.
#[derive(Debug, Default)]
pub struct CompanionReceipt {
    confirmed: std::sync::atomic::AtomicBool,
    observer: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl CompanionReceipt {
    /// Also publish this exact accepted read to a process-local admission observer.
    #[must_use]
    pub const fn reporting_confirmation(
        observer: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            confirmed: std::sync::atomic::AtomicBool::new(false),
            observer: Some(observer),
        }
    }

    /// Whether the keeper confirmed reading this replacement.
    #[must_use]
    pub fn confirmed(&self) -> bool {
        self.confirmed.load(std::sync::atomic::Ordering::Acquire)
    }

    fn confirm(&self) {
        self.confirmed
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(observer) = &self.observer {
            observer.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    /// A captured keeper read, for in-memory queue fixtures only.
    #[cfg(any(test, debug_assertions))]
    pub fn confirm_captured(&self) {
        self.confirm();
    }
}

#[derive(Debug)]
struct CompanionRead {
    receipt: std::sync::Arc<CompanionReceipt>,
    sync: Option<u64>,
}

fn confirm_companion(read: &mut Option<CompanionRead>, synced: u64, refused: bool) {
    if !refused
        && read
            .as_ref()
            .is_some_and(|r| r.sync.is_some_and(|n| n <= synced))
        && let Some(read) = read.take()
    {
        read.receipt.confirm();
    }
}

/// One message to the keeper, queued by the window.
#[derive(Debug)]
pub enum LinkOp {
    /// Register a fresh or adopted master duplicate.
    Register {
        master: OwnedFd,
        header: MasterHeader,
        tag: Vec<u8>,
    },
    /// The newest META of a master.
    Meta { rdev: u64, meta: Vec<u8> },
    /// A tab or pane closed.
    Release { rdev: u64 },
    /// An update's successor, before its grant.
    Pending { pid: u32 },
    /// The quit, written on the final-exit path just before `exit(0)`.
    Bye,
    /// Attach the journal region of a master already queued for registration.
    Companion {
        rdev: u64,
        region: OwnedFd,
        len: u64,
        /// Optional read confirmation for a region that replaces a live predecessor.
        receipt: Option<std::sync::Arc<CompanionReceipt>>,
    },
    /// Release any journal region held for `rdev`.
    CompanionRelease { rdev: u64 },
}

/// Queue bound for ordinary ops.
pub const LINK_QUEUE: usize = 256;

/// Extra channel capacity for waited ops, which ordinary offers cannot fill.
pub const WAITED_ROOM: usize = 4;

/// How many ordinary ops the queue holds that its worker has not taken: never more than
/// [`LINK_QUEUE`], so the [`WAITED_ROOM`] places past them stay free for waited ops.
#[derive(Debug, Default)]
struct Room(std::sync::atomic::AtomicUsize);

impl Room {
    fn claim(&self) -> bool {
        self.0
            .try_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |held| (held < LINK_QUEUE).then_some(held + 1),
            )
            .is_ok()
    }

    fn give_back(&self, n: usize) {
        let _ = self.0.try_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |held| Some(held.saturating_sub(n)),
        );
    }

    fn clear(&self) {
        self.0.store(0, std::sync::atomic::Ordering::Release);
    }
}

struct Ended(std::sync::Arc<Room>);

impl Drop for Ended {
    fn drop(&mut self) {
        self.0.clear();
    }
}

/// Why [`KeeperLink::offer`] queued nothing, with the op it hands back.
#[derive(Debug)]
pub enum Refused {
    /// The queue is full.
    Full(LinkOp),
    /// The worker has ended.
    Gone(LinkOp),
}

/// The far end of a [`KeeperLink::stalled`] link.
#[cfg(any(test, debug_assertions))]
#[derive(Debug)]
pub struct Stalled {
    /// Behind a lock, so the stand-in can be shared as the worker it stands for is not.
    rx: std::sync::Mutex<Receiver<Queued>>,
    /// The link's room, which the ordinary ops taken give back as a worker's do.
    room: std::sync::Arc<Room>,
}

#[cfg(any(test, debug_assertions))]
impl Stalled {
    /// Take every op queued so far, in order.
    #[must_use]
    pub fn drain(&self) -> Vec<LinkOp> {
        self.drain_at(std::time::Instant::now())
    }

    fn drain_at(&self, now: std::time::Instant) -> Vec<LinkOp> {
        let taken: Vec<Queued> = self
            .rx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .try_iter()
            .collect();
        self.room
            .give_back(taken.iter().filter(|(_, ack)| ack.is_none()).count());
        taken
            .into_iter()
            .filter_map(|queued| claim_queued(queued, now))
            .map(|(op, _)| op)
            .collect()
    }
}

#[cfg(any(test, debug_assertions))]
impl Drop for Stalled {
    fn drop(&mut self) {
        self.room.clear();
    }
}

type Queued = (LinkOp, Option<Waited>);

/// Cancellation belongs to one queued wait, never to the link or another op.
#[derive(Debug)]
struct Waited {
    answer: SyncSender<bool>,
    state: std::sync::Arc<std::sync::atomic::AtomicU8>,
    until: Option<std::time::Instant>,
}

impl Waited {
    const QUEUED: u8 = 0;
    const CLAIMED: u8 = 1;
    const CANCELLED: u8 = 2;

    fn cancel(state: &std::sync::atomic::AtomicU8) {
        let _ = state.compare_exchange(
            Self::QUEUED,
            Self::CANCELLED,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        );
    }

    fn claim_at(&self, now: std::time::Instant) -> bool {
        if self.until.is_some_and(|until| now >= until) {
            Self::cancel(&self.state);
            return false;
        }
        self.state
            .compare_exchange(
                Self::QUEUED,
                Self::CLAIMED,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
    }

    fn try_send(&self, answer: bool) -> Result<(), mpsc::TrySendError<bool>> {
        self.answer.try_send(answer)
    }
}

fn claim_queued(queued: Queued, now: std::time::Instant) -> Option<Queued> {
    if let Some(waited) = &queued.1
        && !waited.claim_at(now)
    {
        let _ = waited.try_send(false);
        return None;
    }
    Some(queued)
}

/// Worker thread that owns the keeper connection and reconnects after a keeper restart.
#[derive(Debug)]
pub struct KeeperLink {
    /// Bounded at [`LINK_QUEUE`] ordinary ops plus [`WAITED_ROOM`] waited ones.
    tx: SyncSender<Queued>,
    /// The ordinary ops' share of the queue ([`Self::offer`]).
    room: std::sync::Arc<Room>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Who a link is: the window it speaks for, as each of its HELLOs says.
#[derive(Debug)]
struct LinkPeer {
    path: PathBuf,
    identity: Identity,
    build: String,
    /// This window's crash marker, named in every link HELLO.
    marker: Option<MarkerRef>,
    /// This window journals its sessions: every link HELLO states [`CAP_JOURNAL`], and its
    /// companions are told to a keeper that states it.
    journal: bool,
}

impl KeeperLink {
    /// Start the worker for the keeper at `path`.
    /// # Errors
    /// The thread failing to spawn.
    pub fn start(path: PathBuf, identity: Identity, build: String) -> io::Result<Self> {
        Self::start_on(path, identity, build, None)
    }

    /// Start the worker naming this window's crash marker in every link HELLO.
    /// # Errors
    /// The thread failing to spawn.
    pub fn start_marked(
        path: PathBuf,
        identity: Identity,
        build: String,
        marker: Option<MarkerRef>,
        booted: Option<KeeperClient>,
        journal: bool,
    ) -> io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel(LINK_QUEUE + WAITED_ROOM);
        let room = std::sync::Arc::new(Room::default());
        let ended = Ended(std::sync::Arc::clone(&room));
        let peer = LinkPeer {
            path,
            identity,
            build,
            marker,
            journal,
        };
        let thread = std::thread::Builder::new()
            .name("aterm-keeper-link".to_string())
            .spawn(move || {
                let ended = ended;
                link_worker(&peer, rx, booted, &ended.0);
            })?;
        Ok(Self {
            tx,
            room,
            thread: Some(thread),
        })
    }

    /// Start the worker on a connection that already sent the window's boot HELLO.
    /// # Errors
    /// The thread failing to spawn.
    pub fn start_on(
        path: PathBuf,
        identity: Identity,
        build: String,
        booted: Option<KeeperClient>,
    ) -> io::Result<Self> {
        Self::start_marked(path, identity, build, None, booted, false)
    }

    /// Queue `op`, returning `false` when the queue is full or the worker is gone.
    pub fn enqueue(&self, op: LinkOp) -> bool {
        self.offer(op).is_ok()
    }

    /// Queue `op`, or return it with the reason it was refused.
    /// # Errors
    /// [`Refused::Full`] while the queue holds [`LINK_QUEUE`] ordinary ops, or [`Refused::Gone`] once the worker has ended.
    pub fn offer(&self, op: LinkOp) -> Result<(), Refused> {
        if !self.room.claim() {
            return Err(Refused::Full(op));
        }
        self.tx.try_send((op, None)).map_err(|refused| {
            self.room.give_back(1);
            match refused {
                TrySendError::Full((op, _)) => Refused::Full(op),
                TrySendError::Disconnected((op, _)) => Refused::Gone(op),
            }
        })
    }

    /// Create a link with no worker whose queued ops stay until [`Stalled::drain`] takes them.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn stalled() -> (Self, Stalled) {
        let (tx, rx) = mpsc::sync_channel(LINK_QUEUE + WAITED_ROOM);
        let room = std::sync::Arc::new(Room::default());
        (
            Self {
                tx,
                room: std::sync::Arc::clone(&room),
                thread: None,
            },
            Stalled {
                rx: std::sync::Mutex::new(rx),
                room,
            },
        )
    }

    /// Queue `op` using waited room and wait for a written answer.
    pub fn send_and_wait(&self, op: LinkOp, within: Duration) -> bool {
        let terminal = matches!(op, LinkOp::Bye);
        let now = std::time::Instant::now();
        let Some(until) = now.checked_add(within) else {
            return false;
        };
        if !terminal && within.is_zero() {
            return false;
        }
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        let state = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(Waited::QUEUED));
        let waited = Waited {
            answer: ack_tx,
            state: std::sync::Arc::clone(&state),
            until: (!terminal).then_some(until),
        };
        if self.tx.try_send((op, Some(waited))).is_err() {
            return false;
        }
        match ack_rx.recv_timeout(until.saturating_duration_since(std::time::Instant::now())) {
            Ok(written) => written,
            Err(_) => {
                if !terminal {
                    Waited::cancel(&state);
                }
                false
            }
        }
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

/// What the link keeps of a master it registered, by rdev: the duplicate a restarted keeper is
/// registered with again, and where the master's journal stands.
#[derive(Debug)]
struct HeldMaster {
    master: OwnedFd,
    header: MasterHeader,
    tag: Vec<u8>,
    journal: HeldJournal,
    /// A refusal for this master on this connection prevents using its SYNC as
    /// acceptance evidence.
    companion_refused: bool,
}

/// Where a held master's crash-restore journal stands, as the keeper is told it (and told again
/// after a reconnect, behind the master's REGISTER).
#[derive(Debug)]
enum HeldJournal {
    /// Nothing said yet: the window's COMPANION or COMPANION_RELEASE is still queued. A
    /// reconnect says nothing for it, so the keeper's poison window stays open until the window
    /// itself settles the claim (a premature "no journal" would close the region a recovering
    /// window was offered, before it had a region of its own).
    Unsaid,
    /// The read-only custody descriptor of its region, and the region's length.
    Region {
        region: OwnedFd,
        len: u64,
        read: Option<CompanionRead>,
    },
    /// It has no region.
    Released,
}

type Held = std::collections::BTreeMap<u64, HeldMaster>;

type Owed = std::collections::BTreeMap<u64, Option<u64>>;

enum Send {
    Register(u64),
    Companion(u64),
    Frame(Frame),
}

/// Poll interval for an idle link connection.
pub const LINK_POLL: Duration = Duration::from_millis(500);

fn link_worker(peer: &LinkPeer, rx: Receiver<Queued>, booted: Option<KeeperClient>, room: &Room) {
    let mut held: Held = std::collections::BTreeMap::new();
    let mut metas: std::collections::BTreeMap<u64, Vec<u8>> = std::collections::BTreeMap::new();
    let mut owed: Owed = Owed::new();
    let mut client: Option<KeeperClient> = booted;
    let mut backoff = Duration::from_millis(50);
    let mut next_try = std::time::Instant::now();
    loop {
        let (op, ack) = match rx.recv_timeout(LINK_POLL) {
            Ok(queued) => {
                if queued.1.is_none() {
                    room.give_back(1);
                }
                match claim_queued(queued, std::time::Instant::now()) {
                    Some((op, ack)) => (Some(op), ack),
                    None => continue,
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => (None, None),
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        if let Some(c) = &client {
            let open = c.drain_with_refusals(|rdev| {
                if let Some(h) = held.get_mut(&rdev) {
                    h.companion_refused = true;
                }
            });
            let synced = c.synced();
            owed.retain(|_, sync| sync.is_none_or(|n| n > synced));
            for h in held.values_mut() {
                if let HeldJournal::Region { read, .. } = &mut h.journal {
                    confirm_companion(read, synced, h.companion_refused);
                }
            }
            if !open {
                client = None;
                unconfirm(&mut owed);
                unconfirm_companions(&mut held);
            }
        }
        let bye = matches!(op, Some(LinkOp::Bye));
        let mut must_reach = None;
        let send = op.and_then(|op| match op {
            LinkOp::Register {
                master,
                header,
                tag,
            } => {
                owed.remove(&header.rdev);
                held.insert(
                    header.rdev,
                    HeldMaster {
                        master,
                        header,
                        tag,
                        journal: HeldJournal::Unsaid,
                        companion_refused: false,
                    },
                );
                Some(Send::Register(header.rdev))
            }
            LinkOp::Release { rdev } => {
                metas.remove(&rdev);
                let registered = held.remove(&rdev).is_some();
                if registered {
                    must_reach = Some(rdev);
                }
                (registered || client.is_some()).then_some(Send::Frame(Frame::Release { rdev }))
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
            LinkOp::Companion {
                rdev,
                region,
                len,
                receipt,
            } => {
                let entry = held.get_mut(&rdev)?;
                entry.journal = HeldJournal::Region {
                    region,
                    len,
                    read: receipt.map(|receipt| CompanionRead {
                        receipt,
                        sync: None,
                    }),
                };
                peer.journal.then_some(Send::Companion(rdev))
            }
            LinkOp::CompanionRelease { rdev } => {
                let entry = held.get_mut(&rdev)?;
                entry.journal = HeldJournal::Released;
                peer.journal.then_some(Send::Frame(Frame::CompanionRelease {
                    rdev,
                    kind: COMPANION_JOURNAL,
                }))
            }
        });
        if send.is_none() && held.is_empty() && owed.is_empty() {
            if let Some(ack) = ack {
                let _ = ack.try_send(false);
            }
            continue;
        }
        let fresh = client.is_none();
        if fresh && (ack.is_some() || std::time::Instant::now() >= next_try) {
            client = reconnect(peer, &mut held, &metas, &mut owed);
            if client.is_some() {
                backoff = Duration::from_millis(50);
            } else {
                next_try = std::time::Instant::now() + backoff;
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        }
        let (Some(c), Some(send)) = (client.as_ref(), send) else {
            owe(&mut owed, must_reach);
            if let Some(ack) = ack {
                let _ = ack.try_send(false);
            }
            continue;
        };
        let mut sent = match &send {
            Send::Register(_) | Send::Companion(_) if fresh => Ok(()),
            Send::Frame(Frame::Meta { .. } | Frame::CompanionRelease { .. }) if fresh => Ok(()),
            Send::Register(rdev) => match held.get(rdev) {
                Some(h) => c.register(&h.master, h.header, h.tag.clone()),
                None => Ok(()),
            },
            Send::Companion(_) | Send::Frame(Frame::CompanionRelease { .. })
                if !c.keeper_journals() =>
            {
                Ok(())
            }
            Send::Companion(rdev) => match held.get(rdev) {
                Some(HeldMaster {
                    journal: HeldJournal::Region { region, len, .. },
                    ..
                }) => c.companion(*rdev, region, *len),
                _ => Ok(()),
            },
            Send::Frame(frame) => c.send(frame, None),
        };
        if sent.is_ok()
            && peer.journal
            && matches!(&send, Send::Companion(_))
            && let Err(e) = sync_companions(c, &mut held)
        {
            sent = Err(e);
        }
        if sent.is_ok()
            && let Some(rdev) = must_reach
            && c.keeper_syncs()
        {
            match c.sync() {
                Ok(n) => keep(&mut owed, rdev, Some(n)),
                Err(e) => sent = Err(e),
            }
        }
        if let Some(ack) = ack {
            let _ = ack.try_send(sent.is_ok());
        }
        if sent.is_err() {
            client = None;
            unconfirm(&mut owed);
            unconfirm_companions(&mut held);
            owe(&mut owed, must_reach);
        }
        if bye {
            return;
        }
    }
}

fn owe(owed: &mut Owed, rdev: Option<u64>) {
    if let Some(rdev) = rdev {
        keep(owed, rdev, None);
    }
}

fn keep(owed: &mut Owed, rdev: u64, sync: Option<u64>) {
    if owed.len() < LINK_QUEUE || owed.contains_key(&rdev) {
        owed.insert(rdev, sync);
    }
}

fn unconfirm(owed: &mut Owed) {
    for sync in owed.values_mut() {
        *sync = None;
    }
}

const fn unconfirm_companion(read: &mut Option<CompanionRead>) {
    if let Some(read) = read {
        read.sync = None;
    }
}

fn unconfirm_companions(held: &mut Held) {
    for h in held.values_mut() {
        if let HeldJournal::Region { read, .. } = &mut h.journal {
            unconfirm_companion(read);
        }
    }
}

fn sync_companions(c: &KeeperClient, held: &mut Held) -> io::Result<()> {
    // An older keeper gives no read evidence. Leave the receipt pending rather than
    // treating a write, or absence of the capability, as permission to drop its region.
    if !c.keeper_journals() || !c.keeper_syncs() {
        return Ok(());
    }
    let pending = held.values().any(|h| {
        matches!(
            &h.journal, HeldJournal::Region { read: Some(read), .. } if read.sync.is_none()
        )
    });
    if pending {
        let n = c.sync()?;
        for h in held.values_mut() {
            if let HeldJournal::Region {
                read: Some(read), ..
            } = &mut h.journal
                && read.sync.is_none()
            {
                read.sync = Some(n);
            }
        }
    }
    Ok(())
}

fn reconnect(
    peer: &LinkPeer,
    held: &mut Held,
    metas: &std::collections::BTreeMap<u64, Vec<u8>>,
    owed: &mut Owed,
) -> Option<KeeperClient> {
    let c = KeeperClient::connect(&peer.path, &peer.identity, Duration::from_secs(2)).ok()?;
    c.hello_link(&peer.build, peer.marker.clone(), peer.journal)
        .ok()?;
    let journals = peer.journal && c.keeper_journals();
    for h in held.values_mut() {
        h.companion_refused = false;
    }
    for (rdev, h) in held.iter() {
        c.register(&h.master, h.header, h.tag.clone()).ok()?;
        if journals {
            match &h.journal {
                HeldJournal::Region { region, len, .. } => c.companion(*rdev, region, *len).ok()?,
                HeldJournal::Released => c.companion_release(*rdev).ok()?,
                HeldJournal::Unsaid => {}
            }
        }
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
    for rdev in owed.keys() {
        c.send(&Frame::Release { rdev: *rdev }, None).ok()?;
    }
    if owed.is_empty() || !c.keeper_syncs() {
        owed.clear();
    } else {
        let n = c.sync().ok()?;
        for sync in owed.values_mut() {
            *sync = Some(n);
        }
    }
    if journals {
        sync_companions(&c, held).ok()?;
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

    const TRUSTD_SILENT: &str =
        "the process trustd gate was silent — the machine's, not the code's";

    fn trustd_silent(why: &str) -> bool {
        why.starts_with("macOS's code-signing service (trustd) is not answering")
    }

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
        let deadline = Instant::now() + Duration::from_secs(60);
        while !done(server) {
            assert!(Instant::now() < deadline, "never: {what}");
            server.step(20).expect("step");
        }
    }

    fn register_op(path: &str, local_id: u64) -> (u64, LinkOp) {
        let master = std::fs::File::open(path).expect("open");
        let rdev = master.metadata().expect("stat").rdev();
        let op = LinkOp::Register {
            master: OwnedFd::from(master),
            header: MasterHeader {
                rdev,
                shell_pid: std::process::id(),
                shell_birth: Birth::default(),
                local_id,
            },
            tag: Vec::new(),
        };
        (rdev, op)
    }

    #[test]
    fn a_listener_of_other_code_is_refused_as_not_this_build() {
        use std::os::unix::fs::PermissionsExt as _;
        aterm_uds::deadline::require_trust_service();
        let dir = std::env::temp_dir().join(format!("akl-other-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let own = Identity::for_self(crate::identity::IdentityPolicy::Designated).expect("own");

        let sock = dir.join("nc.sock");
        let mut nc = aterm_uds::spawnfd::command("/usr/bin/nc")
            .arg("-dlU")
            .arg(&sock)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn nc");
        let started = Instant::now();
        let refused = loop {
            match KeeperClient::connect(&sock, &own, Duration::from_secs(60)) {
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
        if trustd_silent(&e.to_string()) {
            aterm_spec::host::could_not_run(format_args!("{TRUSTD_SILENT}: {e}"));
        }
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

    #[test]
    fn a_keeper_at_the_uid_floor_is_refused_by_a_window_that_checks_code() {
        aterm_uds::deadline::require_trust_service();
        let dir = std::env::temp_dir().join(format!("akl-floor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let own = || Identity::for_self(IdentityPolicy::Designated).expect("own");
        for (name, keeper, accepted) in [
            ("uid.sock", Identity::same_uid(), false),
            ("designated.sock", own(), true),
        ] {
            let sock = dir.join(name);
            let mut server = Server::bind(ServeConfig {
                socket: sock.clone(),
                identity: keeper,
                relaunch: Relauncher::Unavailable,
                version: "test".into(),
                test_drop_custody: false,
            })
            .expect("bind");
            let window = std::thread::spawn(move || {
                let id = own();
                let dial = || KeeperClient::connect(&sock, &id, Duration::from_secs(60));
                let link = dial()?.hello_link("t", None, false);
                let boot = dial()?.hello_app("t").map(|_| ());
                Ok::<_, io::Error>((link, boot))
            });
            while !window.is_finished() {
                server.step(20).expect("step");
            }
            let keeper_gate_silent = server
                .test_unchecked_ever()
                .iter()
                .any(|why| trustd_silent(why));
            let joined = window.join().expect("the window thread");
            let silent = |e: &io::Error| trustd_silent(&e.to_string()) || keeper_gate_silent;
            if let Err(e) = &joined
                && silent(e)
            {
                aterm_spec::host::could_not_run(format_args!("{TRUSTD_SILENT}: {name}: {e}"));
            }
            let (link, boot) = joined.expect("the keeper's code is this build's");
            for (what, got) in [("link", link), ("boot", boot)] {
                if let Err(e) = &got
                    && silent(e)
                {
                    aterm_spec::host::could_not_run(format_args!(
                        "{TRUSTD_SILENT}: {what} HELLO to {name}: {e}"
                    ));
                }
                if accepted {
                    got.unwrap_or_else(|e| panic!("{what} HELLO to {name}: {e}"));
                } else {
                    let e = got.expect_err(what);
                    assert!(refused_identity(&e), "{what} HELLO to {name}: {e}");
                    assert!(e.to_string().contains("--identity uid"), "{e}");
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

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
        let unanswered = check_failed(CheckError::Unanswered(
            "macOS's code-signing service (trustd) is not answering".into(),
        ));
        assert!(!refused_identity(&unanswered), "{unanswered}");
        assert_eq!(unanswered.kind(), io::ErrorKind::Other, "{unanswered}");
    }

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

    #[test]
    fn a_restarted_keeper_is_told_each_region_again() {
        let base = std::env::temp_dir();
        let dir = base.join(format!("akl-journal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sock = dir.join("k.sock");
        let mut first = serve(&sock);
        let (journaled, journaled_op) = register_op("/dev/null", 1);
        let (unsaid, unsaid_op) = register_op("/dev/zero", 2);
        let link = KeeperLink::start_marked(
            sock.clone(),
            Identity::same_uid(),
            "t".into(),
            None,
            None,
            true,
        )
        .expect("link");
        assert!(link.enqueue(journaled_op));
        assert!(link.enqueue(unsaid_op));
        let len = aterm_uds::shm::PAGE;
        #[expect(
            clippy::disallowed_methods,
            reason = "a test playing the window makes the region the keeper is sent"
        )]
        // SAFETY: this fixture's link and keeper only transfer/hold/query custody, never reopen, map, write or resize it; `_region` remains inert until it is dropped.
        let (_region, custody) = unsafe { aterm_uds::shm::Region::create(len) }.expect("a region");
        assert!(link.enqueue(LinkOp::Companion {
            rdev: journaled,
            region: custody,
            len: len as u64,
            receipt: None,
        }));
        let view = |s: &Server, rdev| s.core().companion_view(rdev);
        let region_held = |s: &Server| {
            s.holds(journaled)
                && view(s, journaled)
                    .is_some_and(|v| v.offered.is_some() && v.reader_settled == Some(true))
        };
        step_until(&mut first, "the first keeper holds the region", region_held);
        drop(first);
        let mut second = serve(&sock);
        step_until(&mut second, "the restarted keeper holds it again", |s| {
            region_held(s) && s.holds(unsaid)
        });
        assert_eq!(
            view(&second, unsaid).map(|v| (v.offered, v.reader_settled)),
            Some((None, Some(false))),
            "a region not yet said is not told as none"
        );
        assert!(link.enqueue(LinkOp::CompanionRelease { rdev: journaled }));
        let released = |s: &Server| {
            s.holds(journaled)
                && view(s, journaled)
                    .is_some_and(|v| v.offered.is_none() && v.reader_settled == Some(true))
        };
        step_until(&mut second, "the release reaches it", released);
        drop(second);
        let mut third = serve(&sock);
        step_until(
            &mut third,
            "a third keeper is told it is released",
            released,
        );
        link.shutdown();
        drop(third);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_replacement_companion_is_confirmed_only_once_the_keeper_has_read_it() {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("akl-receipt-")
            .tempdir()
            .expect("exclusive receipt fixture");
        let sock = scratch.path().join("k.sock");
        let mut server = serve(&sock);
        let (rdev, register) = register_op("/dev/null", 1);
        let link = KeeperLink::start_marked(
            sock.clone(),
            Identity::same_uid(),
            "t".into(),
            None,
            None,
            true,
        )
        .expect("link");
        assert!(link.enqueue(register));
        let len = aterm_uds::shm::PAGE;
        let region = || {
            #[expect(
                clippy::disallowed_methods,
                reason = "a test playing the window makes the regions the keeper is sent"
            )]
            // SAFETY: this fixture's link and keeper only transfer/hold/query custody, never reopen, map, write or resize it; the region remains inert until it is dropped.
            let made = unsafe { aterm_uds::shm::Region::create(len) }.expect("a region");
            made
        };
        let (_first, custody) = region();
        assert!(link.enqueue(LinkOp::Companion {
            rdev,
            region: custody,
            len: len as u64,
            receipt: None,
        }));
        let offered = |s: &Server| s.core().companion_view(rdev).and_then(|v| v.offered);
        step_until(&mut server, "the keeper holds the first region", |s| {
            s.holds(rdev) && offered(s).is_some()
        });
        let first = offered(&server);

        let (_fresh, custody) = region();
        let receipt = std::sync::Arc::new(CompanionReceipt::default());
        assert!(link.enqueue(LinkOp::Companion {
            rdev,
            region: custody,
            len: len as u64,
            receipt: Some(std::sync::Arc::clone(&receipt)),
        }));
        assert!(
            link.send_and_wait(
                LinkOp::Meta {
                    rdev,
                    meta: b"m".to_vec(),
                },
                Duration::from_secs(60),
            ),
            "control: the COMPANION, its SYNC and the META behind them are written"
        );
        assert!(
            !receipt.confirmed(),
            "a written COMPANION is not a read one"
        );
        assert_eq!(
            offered(&server),
            first,
            "premise: the keeper has read nothing since"
        );
        step_until(
            &mut server,
            "the keeper's read confirms the receipt",
            |_| receipt.confirmed(),
        );
        assert!(
            offered(&server).is_some_and(|id| Some(id) != first),
            "the keeper offers the fresh region"
        );
        link.shutdown();
        drop(server);
    }

    #[test]
    fn a_release_the_link_could_not_write_is_told_to_the_next_connection() {
        let base = std::env::temp_dir();
        let dir = base.join(format!("akl-rel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sock = dir.join("k.sock");
        let aside = dir.join("k.aside");
        let mut server = serve(&sock);
        let (closed, closed_op) = register_op("/dev/null", 1);
        let (kept, kept_op) = register_op("/dev/zero", 2);
        assert_ne!(closed, kept);
        let link = KeeperLink::start(sock.clone(), Identity::same_uid(), "t".into()).expect("link");
        assert!(link.enqueue(closed_op));
        assert!(link.enqueue(kept_op));
        step_until(&mut server, "the keeper holds both", |s| {
            s.holds(closed) && s.holds(kept)
        });
        std::fs::rename(&sock, &aside).expect("move the keeper's socket aside");
        server.test_drop_connections();
        let written = link.send_and_wait(LinkOp::Release { rdev: closed }, Duration::from_secs(60));
        assert!(!written, "no keeper was reachable");
        std::fs::rename(&aside, &sock).expect("restore the keeper's socket");
        step_until(&mut server, "the RELEASE reaches the keeper", |s| {
            !s.holds(closed)
        });
        assert!(server.holds(kept), "the open tab stays in custody");
        link.shutdown();
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_release_the_keeper_dropped_unread_is_told_to_the_next_connection() {
        let base = std::env::temp_dir();
        let dir = base.join(format!("akl-unread-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sock = dir.join("k.sock");
        let mut server = serve(&sock);
        let (closed, closed_op) = register_op("/dev/null", 1);
        let (kept, kept_op) = register_op("/dev/zero", 2);
        let link = KeeperLink::start(sock.clone(), Identity::same_uid(), "t".into()).expect("link");
        assert!(link.enqueue(closed_op));
        assert!(link.enqueue(kept_op));
        step_until(&mut server, "the keeper holds both", |s| {
            s.holds(closed) && s.holds(kept)
        });
        let written = link.send_and_wait(LinkOp::Release { rdev: closed }, Duration::from_secs(60));
        assert!(written, "the RELEASE was written");
        server.test_drop_connections();
        assert!(server.holds(closed), "the keeper never read the RELEASE");
        step_until(&mut server, "the RELEASE reaches the keeper", |s| {
            !s.holds(closed)
        });
        assert!(server.holds(kept), "the open tab stays in custody");
        link.shutdown();
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_welcome_later_than_a_frame_still_answers_the_hello() {
        #[derive(Clone, Copy, Debug)]
        enum Hello {
            App,
            Link,
            Status,
        }
        let frame = Duration::from_millis(100);
        let late = Duration::from_millis(400);
        let dials: Vec<(usize, Hello)> = [Hello::App, Hello::Link, Hello::Status]
            .into_iter()
            .enumerate()
            .collect();
        aterm_spec::check::each(&dials, |&(n, hello)| {
            let dir = std::env::temp_dir().join(format!("akl-late-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("dir");
            let sock = dir.join("k.sock");
            let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
            let keeper = std::thread::spawn(move || {
                let (stream, _) = listener.accept().expect("accept");
                stream
                    .set_read_timeout(Some(Duration::from_secs(60)))
                    .expect("timeout");
                let hello = read_frame(&stream).expect("read").expect("a frame").frame;
                assert!(matches!(hello, Frame::Hello { .. }), "{hello:?}");
                std::thread::sleep(late);
                let welcome = Frame::Welcome {
                    proto: PROTO,
                    offers: 0,
                    version: "late".into(),
                    identity: Some(IdentityPolicy::SameUid),
                    caps: Some(0),
                };
                write_frame(&stream, &welcome, None).expect("welcome");
                if matches!(
                    hello,
                    Frame::Hello {
                        class: PeerClass::Status,
                        ..
                    }
                ) {
                    let ask = read_frame(&stream).expect("read").expect("a frame").frame;
                    assert!(matches!(ask, Frame::Status { .. }), "{ask:?}");
                    std::thread::sleep(late);
                    let status = Frame::Status {
                        text: "late".into(),
                    };
                    write_frame(&stream, &status, None).expect("status");
                }
                stream
            });
            let client = KeeperClient::connect(&sock, &Identity::same_uid(), frame).expect("dial");
            match hello {
                Hello::App => {
                    let offers = client
                        .hello_app_marked("t", None, true)
                        .expect("the late WELCOME is heard");
                    assert!(offers.is_empty());
                }
                Hello::Link => client
                    .hello_link("t", None, true)
                    .expect("the late WELCOME is heard"),
                Hello::Status => assert_eq!(
                    client
                        .status("t")
                        .expect("the late WELCOME and STATUS are heard"),
                    "late"
                ),
            }
            assert_eq!(
                client.stream.read_timeout().expect("its wait"),
                Some(frame),
                "{hello:?}"
            );
            drop(keeper.join().expect("the late keeper"));
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn a_keeper_that_answers_no_sync_is_sent_none() {
        use crate::wire::Kind;
        let base = std::env::temp_dir();
        let dir = base.join(format!("akl-nosync-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let sock = dir.join("k.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
        let older = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(60)))
                .expect("timeout");
            let mut kinds = Vec::new();
            loop {
                let frame = read_frame(&stream).expect("read").expect("a frame").frame;
                kinds.push(frame.kind());
                match frame {
                    Frame::Hello { .. } => {
                        let welcome = Frame::Welcome {
                            proto: PROTO,
                            offers: 0,
                            version: "older".into(),
                            identity: Some(IdentityPolicy::SameUid),
                            caps: Some(CAP_JOURNAL),
                        };
                        write_frame(&stream, &welcome, None).expect("welcome");
                    }
                    Frame::Pending { .. } => return kinds,
                    _ => {}
                }
            }
        });
        let (rdev, op) = register_op("/dev/null", 1);
        let link = KeeperLink::start(sock, Identity::same_uid(), "t".into()).expect("link");
        assert!(link.enqueue(op));
        let within = Duration::from_secs(60);
        assert!(link.send_and_wait(LinkOp::Release { rdev }, within));
        assert!(link.send_and_wait(LinkOp::Pending { pid: 1 }, within));
        let kinds = older.join().expect("the older keeper");
        assert_eq!(
            kinds,
            [Kind::Hello, Kind::Register, Kind::Release, Kind::Pending]
        );
        link.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

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
        let waiter = std::thread::spawn(move || {
            let pending = link.send_and_wait(LinkOp::Pending { pid: 1 }, Duration::from_secs(60));
            let bye = link.send_and_wait(LinkOp::Bye, Duration::from_secs(60));
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
            server
                .core()
                .pending_successors()
                .iter()
                .map(|p| p.pid)
                .collect::<Vec<_>>(),
            [1],
            "the PENDING arrived before the BYE"
        );
        assert!(server.holds(rdev), "the RELEASE after the BYE never came");
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

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
        assert!(!link.send_and_wait(LinkOp::Pending { pid: 1 }, Duration::from_secs(300)));
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "refused without waiting out the bound: {:?}",
            started.elapsed()
        );
        link.shutdown();
    }
}

/// What a full or ended link does with an op, on any unix (no keeper is reached).
#[cfg(test)]
mod offer_tests {
    use super::*;

    #[test]
    fn a_full_queue_hands_an_offered_op_back_and_a_gone_worker_does_too() {
        let (link, far) = KeeperLink::stalled();
        for rdev in 0..LINK_QUEUE as u64 {
            assert!(link.offer(LinkOp::Release { rdev }).is_ok(), "{rdev} fits");
        }
        let marked = u64::MAX;
        match link.offer(LinkOp::CompanionRelease { rdev: marked }) {
            Err(Refused::Full(LinkOp::CompanionRelease { rdev })) => assert_eq!(rdev, marked),
            other => panic!("a full queue hands the op back: {other:?}"),
        }
        assert!(
            !link.enqueue(LinkOp::CompanionRelease { rdev: marked - 1 }),
            "control: enqueue drops it"
        );
        let drained = far.drain();
        assert_eq!(drained.len(), LINK_QUEUE, "only what fit");
        assert!(
            !drained
                .iter()
                .any(|op| matches!(op, LinkOp::CompanionRelease { .. })),
            "neither the refused offer nor the dropped enqueue reached the keeper"
        );
        assert!(
            link.offer(LinkOp::CompanionRelease { rdev: marked })
                .is_ok(),
            "drained, the queue takes it"
        );
        assert!(matches!(
            far.drain().as_slice(),
            [LinkOp::CompanionRelease { rdev }] if *rdev == marked
        ));
        drop(far);
        assert!(matches!(
            link.offer(LinkOp::Release { rdev: 1 }),
            Err(Refused::Gone(LinkOp::Release { rdev: 1 }))
        ));
        link.shutdown();

        let observer = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let receipt = std::sync::Arc::new(CompanionReceipt::reporting_confirmation(
            std::sync::Arc::clone(&observer),
        ));
        let mut read = Some(CompanionRead {
            receipt: std::sync::Arc::clone(&receipt),
            sync: None,
        });
        confirm_companion(&mut read, u64::MAX, false);
        assert!(!receipt.confirmed() && read.is_some());
        read.as_mut().expect("pending").sync = Some(7);
        confirm_companion(&mut read, 6, false);
        assert!(!receipt.confirmed() && read.is_some());
        confirm_companion(&mut read, 7, true);
        assert!(!observer.load(std::sync::atomic::Ordering::Acquire));
        assert!(
            !receipt.confirmed() && read.is_some(),
            "a refusal is not acceptance"
        );
        unconfirm_companion(&mut read);
        confirm_companion(&mut read, 7, false);
        assert!(!observer.load(std::sync::atomic::Ordering::Acquire));
        assert!(
            !receipt.confirmed() && read.is_some(),
            "the disconnected op is unsent"
        );
        read.as_mut().expect("pending").sync = Some(1);
        confirm_companion(&mut read, 0, false);
        assert!(
            !receipt.confirmed() && read.is_some(),
            "reconnected but unanswered"
        );
        confirm_companion(&mut read, 1, false);
        assert!(receipt.confirmed() && read.is_none());
        assert!(observer.load(std::sync::atomic::Ordering::Acquire));

        let now = std::time::Instant::now();
        let make = || {
            let (answer, read) = mpsc::sync_channel(1);
            let state = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(Waited::QUEUED));
            let waited = Waited {
                answer,
                state: std::sync::Arc::clone(&state),
                until: Some(now + Duration::from_secs(60)),
            };
            ((LinkOp::Pending { pid: 7 }, Some(waited)), state, read)
        };
        let (queued, state, answer) = make();
        Waited::cancel(&state);
        assert!(claim_queued(queued, now).is_none());
        assert!(!answer.try_recv().expect("cancelled wait answered"));
        let (queued, _, answer) = make();
        assert!(claim_queued(queued, now + Duration::from_secs(60)).is_none());
        assert!(!answer.try_recv().expect("expired wait answered"));
        let (queued, state, answer) = make();
        let (op, waited) = claim_queued(queued, now).expect("live wait claimed");
        assert!(matches!(op, LinkOp::Pending { pid: 7 }));
        Waited::cancel(&state);
        assert_eq!(
            state.load(std::sync::atomic::Ordering::Acquire),
            Waited::CLAIMED,
            "a timeout cannot retract an already-running op"
        );
        waited
            .expect("waiter")
            .try_send(true)
            .expect("written answer");
        assert!(answer.try_recv().expect("live wait answered"));
    }

    #[test]
    fn a_waited_op_finds_room_in_a_queue_its_ordinary_ops_filled() {
        let (link, far) = KeeperLink::stalled();
        for rdev in 0..LINK_QUEUE as u64 {
            assert!(link.offer(LinkOp::Release { rdev }).is_ok(), "{rdev} fits");
        }
        assert!(
            matches!(
                link.offer(LinkOp::CompanionRelease { rdev: 1 }),
                Err(Refused::Full(_))
            ),
            "control: an ordinary op finds the queue full"
        );
        let now = std::time::Instant::now();
        let waited = u32::try_from(WAITED_ROOM).expect("a few places");
        for pid in 0..=waited {
            let (answer, _read) = mpsc::sync_channel(1);
            let request = Waited {
                answer,
                state: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(Waited::QUEUED)),
                until: Some(now + Duration::from_secs(60)),
            };
            let queued = link
                .tx
                .try_send((LinkOp::Pending { pid }, Some(request)))
                .is_ok();
            assert_eq!(queued, pid < waited, "only the reserved capacity fits");
        }
        assert!(
            !link.send_and_wait(LinkOp::Pending { pid: 99 }, Duration::ZERO),
            "an already-expired nonterminal wait is never admitted"
        );
        let drained = far.drain_at(now);
        assert_eq!(drained.len(), LINK_QUEUE + WAITED_ROOM);
        assert!(
            drained[..LINK_QUEUE]
                .iter()
                .all(|op| matches!(op, LinkOp::Release { .. }))
        );
        let pids: Vec<u32> = drained[LINK_QUEUE..]
            .iter()
            .map(|op| match op {
                LinkOp::Pending { pid } => *pid,
                other => panic!("only waited ops follow: {other:?}"),
            })
            .collect();
        assert_eq!(pids, (0..waited).collect::<Vec<_>>());
        assert!(
            link.offer(LinkOp::Release { rdev: 1 }).is_ok(),
            "read, the queue takes ordinary ops again"
        );

        let (link, far) = KeeperLink::stalled();
        while link.enqueue(LinkOp::Release { rdev: 1 }) {}
        drop(far);
        assert!(
            matches!(
                link.offer(LinkOp::Release { rdev: 2 }),
                Err(Refused::Gone(_))
            ),
            "an ended worker is gone, not full"
        );
    }
}
