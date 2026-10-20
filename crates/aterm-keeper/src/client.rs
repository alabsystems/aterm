// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's side of AKP1: [`KeeperClient`], one blocking connection, and [`KeeperLink`], the
//! worker thread with a bounded queue a window drives it through (§5.3 step 1: the main thread
//! makes no socket call; the queue holds duplicates, never descriptor numbers, so recycling
//! cannot misroute a PTY). `aterm-gui`'s `keeper_link` says the boot HELLO on a
//! [`KeeperClient`], then starts a [`KeeperLink`] on that connection.
//!
//! The client checks the keeper too before it sends a master (§5.7): the same uid, and under
//! [`IdentityPolicy::Designated`] the same code, or a same-uid process that bound the socket
//! first would receive a live duplicate of every terminal. Code alone is not enough (a genuine
//! keeper under `--identity uid` offers a crashed window's masters to any same-uid process), so
//! such a window also registers nothing with a keeper whose WELCOME does not state it requires
//! code too.
//!
//! [`IdentityPolicy::Designated`]: crate::identity::IdentityPolicy::Designated

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

/// One OFFER as the window receives it: the master, its fixed header, the
/// two opaque blobs, and (to a window that journals) the session's journal
/// region and the keeper's poison brake.
#[derive(Debug)]
pub struct Offered {
    pub master: OwnedFd,
    pub header: MasterHeader,
    pub tag: Vec<u8>,
    pub meta: Vec<u8>,
    /// The crash-restore region offered with the master (a COMPANION_OFFER), only to a HELLO
    /// that stated [`CAP_JOURNAL`].
    pub companion: Option<OfferedCompanion>,
    /// The keeper's poison brake (`JournalOffer::attempts`), as the OFFER stated it: 0 when the
    /// region is the dead reader's own and its tail may be replayed, 1 for its keyframe alone
    /// (from 2 no region comes). 0 from a keeper that stated none.
    pub attempts: u8,
}

/// A crash-restore journal region a keeper offered: its read-only descriptor, which the window
/// maps with `aterm_uds::shm::Region::map_readonly(region, len)`.
#[derive(Debug)]
pub struct OfferedCompanion {
    pub region: OwnedFd,
    pub kind: u8,
    pub len: u64,
}

/// A keeper that answered but failed this build's identity check (§5.7), or
/// whose WELCOME states a weaker policy than the window's own: the payload of
/// the `PermissionDenied` error [`KeeperClient::connect`] and the HELLOs
/// return, so a caller can tell it from a socket it may not open
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
        CheckError::Unchecked(why) | CheckError::Unanswered(why) => io::Error::other(why),
    }
}

/// One connection to a keeper.
#[derive(Debug)]
pub struct KeeperClient {
    stream: CtlStream,
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
    /// Dial `path`, check the keeper's identity (within `timeout`: the check
    /// waits on macOS's trustd), and set a read timeout.
    ///
    /// # Errors
    /// The dial failing, the keeper failing the identity check (a
    /// [`NotThisBuild`] inside `PermissionDenied`), or the check not running
    /// (trustd not answering within `timeout` among the reasons).
    pub fn connect(path: &Path, identity: &Identity, timeout: Duration) -> io::Result<Self> {
        let stream = CtlStream::connect(path)?;
        identity.check(&stream, timeout).map_err(check_failed)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        Ok(Self {
            stream,
            policy: identity.policy(),
            keeper_caps: Cell::new(0),
            syncs: Cell::new(0),
            synced: Cell::new(0),
        })
    }

    /// Whether the keeper's WELCOME stated it takes crash-restore companions.
    #[must_use]
    pub fn keeper_journals(&self) -> bool {
        self.keeper_caps.get() & CAP_JOURNAL != 0
    }

    /// Whether the keeper's WELCOME stated it answers SYNC.
    #[must_use]
    pub fn keeper_syncs(&self) -> bool {
        self.keeper_caps.get() & CAP_SYNC != 0
    }

    /// SYNC: ask the keeper to say when it has read every frame written so far. Returns the
    /// SYNC's number, which [`Self::synced`] reaches once the answer is read ([`Self::drain`]).
    /// Only to a keeper that states [`CAP_SYNC`] ([`Self::keeper_syncs`]); an older one would
    /// refuse the kind and drop the connection.
    ///
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

    /// The highest SYNC number the keeper answered on this connection (0 before any).
    #[must_use]
    pub fn synced(&self) -> u64 {
        self.synced.get()
    }

    /// Read a WELCOME's policy and caps: the policy must serve this window ([`Self::served`]),
    /// and the caps say whether companions may be sent here.
    fn welcomed(&self, identity: Option<IdentityPolicy>, caps: Option<u8>) -> io::Result<()> {
        self.served(identity)?;
        self.keeper_caps.set(caps.unwrap_or(0));
        Ok(())
    }

    /// Whether a keeper whose WELCOME stated `served` may be handed this window's masters: under
    /// [`IdentityPolicy::Designated`], only one that requires the same of its peers (§5.7). The
    /// same-uid floor, or a keeper too old to say, is refused as a [`NotThisBuild`].
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
        self.hello_app_marked(build, None, false)
    }

    /// [`Self::hello_app`], naming this window's crash marker (§5.4 row 3's
    /// cross-check): the keeper then reads a lost BYE with the marker gone and
    /// `exit(0)` as the quit it was. With `journal`, the HELLO states
    /// [`CAP_JOURNAL`]: this window journals its sessions, and each OFFER may
    /// bring the session's region (its COMPANION_OFFER, read right after it).
    ///
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
            // A keeper sends the trailing fields, and the region after them, only to a HELLO
            // that stated CAP_JOURNAL; a window that did not reads the OFFER it always read.
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

    /// The COMPANION_OFFER an OFFER of `rdev` said follows it.
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

    /// HELLO as a window's link ([`CAP_LINK`]): registered masters only, no
    /// offers, not a launch. With `journal`, it states [`CAP_JOURNAL`] too, as
    /// the window's boot HELLO did: the keeper reads a claim from a window that
    /// does not state it as the end of every region the master had.
    ///
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
        match self.recv()?.map(|i| i.frame) {
            Some(Frame::Welcome {
                offers: 0,
                identity,
                caps,
                ..
            }) => self.welcomed(identity, caps),
            other => Err(protocol(format!(
                "expected an empty WELCOME, got {other:?}"
            ))),
        }
    }

    /// HELLO as a status client of build `build`, then ask for the status text.
    ///
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

    /// Read every frame already waiting, keeping only a SYNC's answer ([`Self::synced`]; a link
    /// acts on nothing else); `false` once the keeper has gone (EOF, a malformed frame).
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
                // Never past the last one written: an answer is to a SYNC this side sent.
                Ok(Some(Incoming {
                    frame: Frame::Sync { seq },
                    ..
                })) => self
                    .synced
                    .set(self.synced.get().max(seq.min(self.syncs.get()))),
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

    /// COMPANION: the read-only custody descriptor of `rdev`'s journal region, `len` bytes, beside
    /// a master this connection claimed. Only to a keeper that states [`CAP_JOURNAL`]
    /// ([`Self::keeper_journals`]); an older one would refuse the kind and drop the connection.
    ///
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

    /// COMPANION_RELEASE: this connection's claim on `rdev` has no journal region.
    ///
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

/// One message to the keeper, queued by the window.
#[derive(Debug)]
pub enum LinkOp {
    /// A fresh or adopted master: the queue owns this duplicate.
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
    /// The journal region of a master already queued for REGISTER: the queue owns this
    /// read-only custody descriptor, and the link keeps it with the master, telling a
    /// restarted keeper again after the master's REGISTER.
    Companion {
        rdev: u64,
        region: OwnedFd,
        len: u64,
    },
    /// The master `rdev` has no journal region (any more): the link drops the one it kept
    /// and tells the keeper, again after a restart too.
    CompanionRelease { rdev: u64 },
}

/// The queue's bound: a window never blocks on the keeper.
pub const LINK_QUEUE: usize = 256;

/// One queued op, and who waits for it to be written (PENDING before the
/// grant, BYE before `exit(0)`).
type Queued = (LinkOp, Option<SyncSender<bool>>);

/// The window's link: a worker thread that owns the connection, reconnects
/// after a keeper restart and re-registers every master it still holds a
/// duplicate of (§5.3 step 9), then tells the keeper every RELEASE of a
/// registered master no keeper confirmed it read ([`Owed`]).
#[derive(Debug)]
pub struct KeeperLink {
    tx: SyncSender<Queued>,
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
    ///
    /// # Errors
    /// The thread failing to spawn.
    pub fn start(path: PathBuf, identity: Identity, build: String) -> io::Result<Self> {
        Self::start_on(path, identity, build, None)
    }

    /// [`Self::start`], naming this window's crash marker in every link HELLO
    /// (each connection is judged on its own, §5.4). With `journal`, every link
    /// HELLO states [`CAP_JOURNAL`], as the boot HELLO did, and the link carries
    /// the window's companions ([`LinkOp::Companion`]).
    ///
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
        let (tx, rx) = mpsc::sync_channel(LINK_QUEUE);
        let peer = LinkPeer {
            path,
            identity,
            build,
            marker,
            journal,
        };
        let thread = std::thread::Builder::new()
            .name("aterm-keeper-link".to_string())
            .spawn(move || link_worker(&peer, &rx, booted))?;
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
        Self::start_marked(path, identity, build, None, booted, false)
    }

    /// Queue `op`; `false` when the queue is full or the worker is gone (the
    /// window carries on: custody is insurance, not state).
    pub fn enqueue(&self, op: LinkOp) -> bool {
        match self.tx.try_send((op, None)) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        }
    }

    /// Queue `op` and wait at most `within` for the worker to have written it to a keeper:
    /// `true` only then. Every op queued before it is written first. `false` (no keeper, a full
    /// queue, the wait ran out) never blocks the caller past `within`.
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

/// What the link keeps of a master it registered, by rdev: the duplicate a restarted keeper is
/// registered with again, and where the master's journal stands.
#[derive(Debug)]
struct HeldMaster {
    master: OwnedFd,
    header: MasterHeader,
    tag: Vec<u8>,
    journal: HeldJournal,
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
    Region { region: OwnedFd, len: u64 },
    /// It has no region.
    Released,
}

type Held = std::collections::BTreeMap<u64, HeldMaster>;

/// The registered masters whose RELEASE no keeper has confirmed it read, by rdev: `None` owed to
/// the next connection (the write failed, or the connection ended before an answer), `Some(n)`
/// written on this connection ahead of its SYNC `n`, and dropped once the keeper answers that
/// SYNC. A keeper drops a connection with frames still unread (a malformed frame, a stalled
/// one), and a write that succeeded says only that the bytes left. A live keeper that saw only
/// the link drop still holds what the dropped connection claimed, and a reconnect registers only
/// what the window holds, so each one owed is told to the next connection after its
/// registrations. A keeper that read it already, or restarted, refuses the copy as NotClaimant
/// (drained); and since the keeper honours a RELEASE only for claims of its own connection or an
/// older one of the window, a first copy still unread on the dropped connection cannot end a
/// claim this link made since at the same rdev. To a keeper that answers no SYNC (an older one),
/// a written RELEASE is all the link can know. Bounded by [`LINK_QUEUE`]: past it, one is lost.
type Owed = std::collections::BTreeMap<u64, Option<u64>>;

/// What one queued op sends once connected.
enum Send {
    Register(u64),
    Companion(u64),
    Frame(Frame),
}

/// How often an idle link looks at its connection: it drains what the keeper sent (an unread
/// socket would stall the keeper's writes) and notices a keeper that went away, so a restarted
/// keeper is told what this window holds without waiting for its next op (§5.3 step 9).
pub const LINK_POLL: Duration = Duration::from_millis(500);

fn link_worker(peer: &LinkPeer, rx: &Receiver<Queued>, booted: Option<KeeperClient>) {
    // The duplicates this window still holds for the keeper, by rdev, so a
    // restarted keeper can be told again — and the newest META of each.
    let mut held: Held = std::collections::BTreeMap::new();
    let mut metas: std::collections::BTreeMap<u64, Vec<u8>> = std::collections::BTreeMap::new();
    let mut owed: Owed = Owed::new();
    let mut client: Option<KeeperClient> = booted;
    let mut backoff = Duration::from_millis(50);
    let mut next_try = std::time::Instant::now();
    loop {
        let (op, ack) = match rx.recv_timeout(LINK_POLL) {
            Ok((op, ack)) => (Some(op), ack),
            Err(mpsc::RecvTimeoutError::Timeout) => (None, None),
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        if let Some(c) = &client {
            let open = c.drain();
            // What the keeper answered a SYNC behind has reached it, the answers read just
            // before an EOF included.
            let synced = c.synced();
            owed.retain(|_, sync| sync.is_none_or(|n| n > synced));
            if !open {
                client = None;
                unconfirm(&mut owed);
            }
        }
        let bye = matches!(op, Some(LinkOp::Bye));
        // The RELEASE of a master this link registered: one that must reach a
        // keeper even when this turn cannot write it.
        let mut must_reach = None;
        let send = op.and_then(|op| match op {
            LinkOp::Register {
                master,
                header,
                tag,
            } => {
                // A new master at a reused device number: an owed RELEASE of
                // the old one must not close it.
                owed.remove(&header.rdev);
                held.insert(
                    header.rdev,
                    HeldMaster {
                        master,
                        header,
                        tag,
                        journal: HeldJournal::Unsaid,
                    },
                );
                Some(Send::Register(header.rdev))
            }
            // A master this link never registered may be one the keeper offered on this
            // connection (the boot HELLO's, `start_on`), and the keeper honours a RELEASE from the
            // connection an offer was made to: a recovering window releases an offer it could not
            // place (`aterm-gui`'s `keeper_link::Claim`). Sent only on a live connection: an offer
            // does not survive its connection, and on any other the keeper refuses it (drained).
            LinkOp::Release { rdev } => {
                metas.remove(&rdev);
                // The master's region, if it had one, closes here with it.
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
            // A region rides beside its master only: one for a master this link holds no
            // registration of is dropped (its descriptor closes here).
            LinkOp::Companion { rdev, region, len } => {
                let entry = held.get_mut(&rdev)?;
                entry.journal = HeldJournal::Region { region, len };
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
        // An idle link with nothing held or owed has nothing to tell a keeper.
        if send.is_none() && held.is_empty() && owed.is_empty() {
            if let Some(ack) = ack {
                let _ = ack.try_send(false);
            }
            continue;
        }
        let fresh = client.is_none();
        // A caller waiting on this op does not wait out a backoff.
        if fresh && (ack.is_some() || std::time::Instant::now() >= next_try) {
            client = reconnect(peer, &held, &metas, &mut owed);
            if client.is_some() {
                backoff = Duration::from_millis(50);
            } else {
                next_try = std::time::Instant::now() + backoff;
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        }
        let (Some(c), Some(send)) = (client.as_ref(), send) else {
            // No keeper (the op is dropped; what is held and every owed RELEASE is told at the
            // reconnect), or an idle turn.
            owe(&mut owed, must_reach);
            if let Some(ack) = ack {
                let _ = ack.try_send(false);
            }
            continue;
        };
        let mut sent = match &send {
            // A fresh connection already registered everything held, told it every META and
            // every journal it could take.
            Send::Register(_) | Send::Companion(_) if fresh => Ok(()),
            Send::Frame(Frame::Meta { .. } | Frame::CompanionRelease { .. }) if fresh => Ok(()),
            Send::Register(rdev) => match held.get(rdev) {
                Some(h) => c.register(&h.master, h.header, h.tag.clone()),
                None => Ok(()),
            },
            // A keeper that does not take companions (an older one) would refuse their kinds
            // and drop the connection: what is kept is told to the next keeper that does.
            Send::Companion(_) | Send::Frame(Frame::CompanionRelease { .. })
                if !c.keeper_journals() =>
            {
                Ok(())
            }
            Send::Companion(rdev) => match held.get(rdev) {
                Some(HeldMaster {
                    journal: HeldJournal::Region { region, len },
                    ..
                }) => c.companion(*rdev, region, *len),
                _ => Ok(()),
            },
            Send::Frame(frame) => c.send(frame, None),
        };
        // A RELEASE that must reach a keeper is followed by a SYNC, and kept until its answer.
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
            owe(&mut owed, must_reach);
        }
        if bye {
            // Nothing follows a quit: the window is about to `exit(0)`, and the
            // keeper reads the BYE, then this connection's end.
            return;
        }
    }
}

/// Keep the RELEASE of `rdev`, a master this link registered, for the next
/// connection.
fn owe(owed: &mut Owed, rdev: Option<u64>) {
    if let Some(rdev) = rdev {
        keep(owed, rdev, None);
    }
}

/// Keep the RELEASE of `rdev` until `sync` is answered (`None`: until the next connection).
fn keep(owed: &mut Owed, rdev: u64, sync: Option<u64>) {
    if owed.len() < LINK_QUEUE || owed.contains_key(&rdev) {
        owed.insert(rdev, sync);
    }
}

/// The connection ended: every RELEASE it carried that the keeper did not confirm is owed to
/// the next one.
fn unconfirm(owed: &mut Owed) {
    for sync in owed.values_mut() {
        *sync = None;
    }
}

fn reconnect(
    peer: &LinkPeer,
    held: &Held,
    metas: &std::collections::BTreeMap<u64, Vec<u8>>,
    owed: &mut Owed,
) -> Option<KeeperClient> {
    let c = KeeperClient::connect(&peer.path, &peer.identity, Duration::from_secs(2)).ok()?;
    // A link registers; it takes no offers (the boot HELLO does, through the
    // window's admission).
    c.hello_link(&peer.build, peer.marker.clone(), peer.journal)
        .ok()?;
    let journals = peer.journal && c.keeper_journals();
    for (rdev, h) in held {
        c.register(&h.master, h.header, h.tag.clone()).ok()?;
        // Each master's journal right behind its claim, as the window first told it: a
        // restarted keeper holds no region, and a live one moved the dropped link's to this one.
        if journals {
            match &h.journal {
                HeldJournal::Region { region, len } => c.companion(*rdev, region, *len).ok()?,
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
    // Then every RELEASE no keeper confirmed: the keeper takes one from a newer link of the
    // process whose dropped link claimed it. Kept until the answer to the SYNC behind them.
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
    Some(c)
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use super::*;
    use crate::server::{Relauncher, ServeConfig, Server};
    use crate::wire::Birth;
    use std::os::unix::fs::MetadataExt as _;
    use std::time::Instant;

    /// `trustd` going silent after the one probe refuses every later check
    /// through the process's gate: the machine's moment, never the code's.
    const TRUSTD_SILENT: &str =
        "the process trustd gate was silent — the machine's, not the code's";

    /// Whether `why` is the identity check's words for a gate that refused it
    /// unanswered (`identity::unanswered`).
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
        // A hang detector, not a latency budget.
        let deadline = Instant::now() + Duration::from_secs(60);
        while !done(server) {
            assert!(Instant::now() < deadline, "never: {what}");
            server.step(20).expect("step");
        }
    }

    /// The REGISTER of a duplicate of `path` (a character device, as a master is), and its rdev.
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

    /// Apple's `nc` listening on the socket fails the designated check ([`refused_identity`]).
    /// Negative controls: no socket, and a socket this process may not open (also
    /// `PermissionDenied`), are not read as that refusal.
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
        // `nc` binds, then listens: a dial between the two is refused, so retry until it listens.
        let started = Instant::now();
        let refused = loop {
            // The check's own deadline is a hang detector too, not a budget.
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

    /// §5.7: though the keeper's code (this very build) passes, the link and the boot HELLO are
    /// both refused. Control: the same build serving under its designated requirement is accepted.
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
                // The check's deadline is a hang detector, not a budget.
                let dial = || KeeperClient::connect(&sock, &id, Duration::from_secs(60));
                let link = dial()?.hello_link("t", None, false);
                let boot = dial()?.hello_app("t").map(|_| ());
                Ok::<_, io::Error>((link, boot))
            });
            // The window's own deadlines (the check's, then each read timeout) are the hang
            // detectors.
            while !window.is_finished() {
                server.step(20).expect("step");
            }
            // Every unchecked turn-away the keeper made, kept though a later
            // check passed: the boot dial's pass must not hide the link's.
            let keeper_gate_silent = server
                .test_unchecked_ever()
                .iter()
                .any(|why| trustd_silent(why));
            let joined = window.join().expect("the window thread");
            // A check of either side that `trustd` did not answer is COULD NOT RUN, never a
            // verdict on the code.
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

    /// A check that could not run (no audit token, the peer's code not found) keeps its words.
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

    /// §5.3 step 9: a restarted keeper is told again what the window holds without waiting for
    /// the window's next op.
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

    /// The crash-restore plan §5, "K restart": a link that journals tells a restarted keeper each
    /// master's region again right behind its REGISTER, and a released one as released; a
    /// master whose region the window has not said yet is told nothing, so its claim stays
    /// unsettled until the window settles it.
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
        // SAFETY: this fixture's link and keeper only transfer/hold/query custody, never
        // reopen, map, write or resize it; `_region` remains inert until it is dropped.
        let (_region, custody) = unsafe { aterm_uds::shm::Region::create(len) }.expect("a region");
        assert!(link.enqueue(LinkOp::Companion {
            rdev: journaled,
            region: custody,
            len: len as u64,
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
        // The reconnect registers in rdev order, one frame a turn: wait for both masters.
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

    /// A live keeper drops the link and a tab closes while no reconnect can succeed: the RELEASE
    /// still reaches the keeper once one does (the reconnect registers only what is held).
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
        // The keeper lives (its listener too, under another name), but drops
        // the link, and no reconnect can reach it.
        std::fs::rename(&sock, &aside).expect("move the keeper's socket aside");
        server.test_drop_connections();
        // Waited for, so the test knows the link tried and could not write it.
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

    /// A live keeper drops the link with a RELEASE the link wrote still unread (a malformed frame or
    /// a stalled one ends a connection there). The write succeeded, so only the keeper's answer to
    /// the SYNC behind it could say the RELEASE was read: unanswered, it is told again to the next
    /// connection, and the closed tab leaves custody.
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
        // Written, while the keeper is not stepped: nothing reads it before the drop.
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

    /// A keeper whose WELCOME states no CAP_SYNC (an older one, which refuses an unknown kind and
    /// drops the connection with every frame behind it) is sent no SYNC: the link's frames reach
    /// it as they always did.
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
            // A hang detector, not a latency budget.
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

    /// PENDING (the update worker gates the grant on it) and BYE (written before `exit(0)`) are
    /// acknowledged only once written to a live keeper, arrive in order, and nothing follows a BYE.
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
        // Given five minutes, so the one-minute hang detector tells a refusal from a link that
        // waited them out.
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
