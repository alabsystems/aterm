// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded, volatile address hints. A beacon's token names no device: its nonce rotates within a
//! transport lifetime. A serving or enrolling node's token also carries a tag of its lab
//! ([`LabKey`]) drawn with that nonce, so a node that knows the lab's pins leaves another lab's
//! beacons undialled and dials its own lab's first (CR2), while a node that does not cannot link
//! one beacon to the next. A hint grants nothing, a tag included: only an authenticated round
//! verifies an endpoint.

use crate::control::{Repair, Report};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

/// Room for every member of a full Commons roster, so a steward can dial each one and hear
/// whether it is silent or which aterm it runs (C26).
const MAX_PEERS: usize = aterm_secrets::commons::roster::MAX_MEMBERS + 1;
/// Each cache can retain a full roster's worth of unverified hints independently.
pub(crate) const MAX_UNVERIFIED_ENDPOINTS: usize = 2 * MAX_PEERS;
const PER_IP: usize = 2;
const EXPIRES: Duration = Duration::from_secs(45);
const VERIFIED_EXPIRES: Duration = Duration::from_secs(600);
/// The longest a peer's quiet rounds dialled in hold back this side's own dial of its endpoint
/// ([`Hints::vouch`]), counted from the endpoint's last completed round. Only a round this side
/// dials verifies an endpoint, and one not verified again within [`VERIFIED_EXPIRES`] is
/// dropped, so the dial of an endpoint held back comes due at least this often, and from then
/// it is picked before the ordinary scan's turn ([`Hints::next_allowed`]): it waits for a place
/// and the dial spacing, behind a follow-up or a led candidate that is due, and not for the
/// scan to come round to it.
const REVERIFY: Duration = Duration::from_secs(480);
const _: () = assert!(REVERIFY.as_secs() < VERIFIED_EXPIRES.as_secs());
const WIRE_LEN: usize = 38;
/// Progress can take three extra turns before ordinary discovery gets another turn.
const FOLLOW_UP_BURST: usize = 3;

/// The key a node's beacons tag its lab with (`crate::control::beacon_key`): a digest of its
/// rounds' TLS discriminator, so two nodes' keys differ exactly when no round between them can get
/// past TLS. Members and enrollees know the pins it is made from; a node on the LAN that does not
/// can neither read a tag nor link two of them.
pub(super) type LabKey = [u8; 32];
/// What a tagged token starts with. A token without it (one an older build drew whole at random)
/// names no lab; one with it names a lab by the tag that follows. Any change to how the tag is
/// drawn takes a new mark, which an older reader reads as naming no lab.
const LAB_MARK: &[u8; 4] = b"ACL1";
/// A tagged token: [`LAB_MARK`], then the tag ([`tag`]), then this many bytes of the rotating
/// nonce, the tag's salt.
const LAB_NONCE: usize = 32 - LAB_MARK.len() - 8;

/// The tag a node of the lab `key` draws with `nonce`: 8 bytes of SHA-256 over a label, the key
/// and the nonce. A new nonce (every rotation) draws a new tag.
fn tag(key: &LabKey, nonce: &[u8]) -> [u8; 8] {
    let mut sha = aterm_digest::Sha256::new();
    sha.update(b"aterm/commons/beacon-tag/1\0");
    sha.update(key);
    sha.update(nonce);
    sha.finalize()[..8].try_into().expect("eight bytes")
}

/// The token a node of the lab `key` beacons with `nonce`: [`LAB_MARK`], its [`tag`] and the
/// nonce's first [`LAB_NONCE`] bytes. With no lab (a presence-only transport) it is the nonce
/// whole.
fn token(key: Option<&LabKey>, nonce: [u8; 32]) -> [u8; 32] {
    let Some(key) = key else {
        return nonce;
    };
    let mut token = [0; 32];
    token[..4].copy_from_slice(LAB_MARK);
    token[4..12].copy_from_slice(&tag(key, &nonce[..LAB_NONCE]));
    token[12..].copy_from_slice(&nonce[..LAB_NONCE]);
    token
}

/// Whose lab a beacon's token names, as this node reads it ([`Hints::sender`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sender {
    /// Its tag is this node's lab's: it may be a peer.
    Ours,
    /// Its tag is another lab's: no round with it can get past TLS. An older build's random token
    /// reads so once in 2^32 rotations, which leaves it undialled until its next rotation.
    Theirs,
    /// It names no lab (an older build), or this node has none to read it by.
    Unnamed,
}

fn delay_hash(own: &[u8; 32], attempt: u64) -> astream_wire::sync::Digest {
    let mut sha = aterm_digest::Sha256::new();
    sha.update(own);
    sha.update(attempt.to_be_bytes());
    sha.finalize()
}

fn retry_delay(own: &[u8; 32], attempt: u64) -> Duration {
    let hash = delay_hash(own, attempt);
    let jitter = u64::from(u16::from_be_bytes([hash[0], hash[1]])) % 2000;
    Duration::from_millis(3000 + jitter)
}

fn follow_up_delay(own: &[u8; 32], attempt: u64) -> Duration {
    let hash = delay_hash(own, attempt);
    let jitter = u64::from(u16::from_be_bytes([hash[2], hash[3]])) % 300;
    Duration::from_millis(200 + jitter)
}

/// Brief contention keeps this many ordinary retries before failed rounds back off.
const QUICK_BUSY_RETRIES: u32 = 3;
/// The first wait in a failed endpoint's exponential backoff.
const BACKOFF_FIRST: Duration = Duration::from_secs(5);
/// The longest wait between dials of an endpoint that keeps not answering, or whose answer this
/// side keeps refusing.
const BACKOFF_MAX: Duration = Duration::from_secs(120);

/// The `streak`th wait in exponential backoff: [`BACKOFF_FIRST`] doubling to
/// [`BACKOFF_MAX`], plus up to 2 s of jitter.
fn backoff_delay(own: &[u8; 32], attempt: u64, streak: u32) -> Duration {
    let hash = delay_hash(own, attempt);
    let jitter = u64::from(u16::from_be_bytes([hash[4], hash[5]])) % 2000;
    let doubled = BACKOFF_FIRST.saturating_mul(1 << streak.saturating_sub(1).min(16));
    doubled.min(BACKOFF_MAX) + Duration::from_millis(jitter)
}

/// How a dialed round ended, which sets its endpoint's next dial ([`Hints::finish`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The peer authenticated and the round repaired; `transferred` when it moved records.
    Repaired { transferred: bool },
    /// The round ended unfinished after the peer authenticated, or after it took this side's hello
    /// and closed or went quiet before sending its own ([`Repair::unanswered`]): contention (a
    /// crossed dial on either side, a busy writer or a full peer cache on either side), a peer
    /// that closed mid-round, a peer that refuses this Mac or has not yet approved its enrollment
    /// (which the dialing side cannot tell from contention), or a local failure to start one.
    /// The first [`QUICK_BUSY_RETRIES`] failed rounds keep the ordinary interval;
    /// further failures back off until a control exchange completes.
    Busy,
    /// The peer authenticated and the control exchange completed, but the application round
    /// deferred its table (`crate::replica::Report::table_deferred`) and moved nothing: not a
    /// repair. The endpoint stays established and its backoff ends; the next dial comes soon after
    /// a first such round, so the table's retry is not left to the quiet interval, and at that
    /// interval after consecutive ones, so a table that stays unavailable holds no worker.
    Deferred,
    /// The round never reached the peer's answer: no connection, no TLS within the handshake's
    /// limit (a peer asleep, cut off, hung or saturated), a hello this side refused (its proof,
    /// witness or membership), or a round that panicked. Such a failure repeats until its cause
    /// changes, so failed rounds increase the wait ([`backoff_delay`]).
    Refused,
}
impl Outcome {
    /// What a finished round says of its endpoint. `success` is the worker's: the round
    /// completed, was not cancelled and authenticated its peer. `control` is
    /// the completed control exchange's durable acknowledgement report.
    pub(crate) fn of(success: bool, repair: &Repair, control: Option<Report>) -> Self {
        if success {
            let transferred = control.is_some_and(|r| r.sent != 0 || r.received != 0)
                || repair_transferred(repair);
            #[cfg(test)]
            let deferral_succeeds = DEFERRAL_SUCCEEDS.with(std::cell::Cell::get);
            #[cfg(not(test))]
            let deferral_succeeds = false;
            if !transferred
                && !deferral_succeeds
                && repair.application().is_some_and(|r| r.table_deferred)
            {
                Self::Deferred
            } else {
                Self::Repaired { transferred }
            }
        } else if repair.authenticated_actor().is_some() || repair.unanswered() {
            Self::Busy
        } else {
            Self::Refused
        }
    }
}

fn repair_transferred(repair: &Repair) -> bool {
    matches!(repair.handoff(), Some(Ok(count)) if *count > 0)
        || repair.membership().is_some_and(|r| r.sent + r.received > 0)
        || repair.application().is_some_and(|r| {
            r.sent
                + r.received
                + r.keys_sent
                + r.keys_received
                + r.conflicts_sent
                + r.conflicts_received
                > 0
        })
}

/// Why an unverified endpoint is dialled ahead of ordinary discovery (CR1, CR2). No reason is
/// trust: a lead orders dials, and only an authenticated round verifies an endpoint. It lasts
/// until a dial of the endpoint shows no peer of this lab is there ([`Repair::disowned`]: nothing
/// accepted the connection, or another lab's listener did), so one that is not the peer it looked
/// like gets one early dial and then waits its turn. A dial that stalls or goes unanswered keeps
/// it: a busy peer of this lab looks the same, and backs off ([`Hints::finish`]). Endpoints of one
/// lead go in an order each node draws for itself ([`Hints::spread`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
enum Lead {
    #[default]
    None,
    /// It beaconed this node's lab's tag ([`LabKey`]): a Mac that remembers nothing dials its own
    /// lab's endpoints before the rest of the LAN's.
    Tagged,
    /// An endpoint this Mac verified before it restarted ([`Hints::remember`]), not heard since.
    Remembered,
    /// An endpoint at an address a peer authenticated from ([`Hints::kin`]): it beaconed from
    /// there, or it is a remembered endpoint that beaconed again.
    Kin,
}

#[derive(Debug)]
struct Peer {
    seen: Instant,
    next: Instant,
    verified: bool,
    busy: bool,
    failed: bool,
    /// Its dial goes ahead of ordinary discovery ([`Lead`]) while it is unverified.
    lead: Lead,
    /// A push hurried this peer while its round ran ([`Hints::hurry`]): dial it again as the round
    /// ends.
    hurried: bool,
    /// A post-push quiet round already used its immediate confirmation turn. Progress or a
    /// new push re-arms that turn; a peer that cannot take this table keeps ordinary spacing.
    quiet_push: bool,
    /// Consecutive incomplete or refused dials. Changing failure kind is not recovery.
    failures: u32,
    /// Its last round ended [`Outcome::Deferred`].
    deferring: bool,
    /// A round its seat dialled in has pulled its next dial in since its own last completed
    /// round ([`Hints::vouch`]): once per failure streak, so a dead endpoint of a live seat
    /// still backs off.
    pulled: bool,
    /// A local append came while its round ran ([`Hints::announce`]): where the round repairs,
    /// its next dial comes within the follow-up interval ([`Hints::finish`]).
    told: bool,
    /// Its last round completed and moved nothing, its hint names no table still to take
    /// ([`Repair::owes`]), and its next dial waits out the ordinary interval after it
    /// ([`Hints::finish`]): nothing has asked for a sooner one since (a push, a local append, a
    /// seat's new build). Only such an endpoint is held back by its seat's quiet rounds dialled
    /// in ([`Hints::vouch`]).
    resting: bool,
    /// A quiet round its seat dialled in has moved its next dial later since it was last picked
    /// ([`Hints::vouch`]): once its last completed round is [`REVERIFY`] old it is picked
    /// before the ordinary scan ([`Hints::next_allowed`]).
    held: bool,
    repair: Repair,
}

impl Peer {
    fn established(&self) -> bool {
        self.verified && !self.failed
    }
    /// A candidate no beacon or round has told anything of yet.
    fn candidate(now: Instant, lead: Lead) -> Self {
        Self {
            seen: now,
            next: now,
            verified: false,
            busy: false,
            failed: false,
            lead,
            hurried: false,
            quiet_push: false,
            failures: 0,
            deferring: false,
            pulled: false,
            told: false,
            resting: false,
            held: false,
            repair: Repair::default(),
        }
    }
    /// The lead its next dial takes: none once it is verified, when it is served as an
    /// established peer or rediscovered by its own turn.
    fn lead(&self) -> Lead {
        if self.verified { Lead::None } else { self.lead }
    }
    /// Whether what is heard of it may raise its lead: it is unverified, and it leads already or
    /// no dial of it has failed.
    fn may_lead(&self) -> bool {
        !self.verified && (self.lead > Lead::None || (self.failures == 0 && !self.failed))
    }
}

/// A seat's endpoint this Mac verified, kept across restarts ([`Hints::remember`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Remembered {
    pub(super) actor: astream_wire::sync::Actor,
    pub(super) address: SocketAddr,
    /// Unix ms when a round with it last completed.
    pub(super) at_ms: u64,
}

/// The most remembered endpoints: the most recently verified seats' ([`Hints::remember`]).
pub(super) const MAX_REMEMBERED: usize = 32;
/// A remembered endpoint older than this is forgotten: a week away is a new Mac's start.
const REMEMBER_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// How stale the kept times of remembered endpoints may grow while none changes.
const REMEMBER_REFRESH: Duration = Duration::from_secs(3600);
/// The file the remembered endpoints are kept in, in the endpoint's Commons custody directory.
pub(super) const REMEMBERED_NAME: &str = "commons-peers.state";
const REMEMBERED_MAGIC: &[u8; 5] = b"ATPE1";
/// What a kept file names before its entries, and must name to be read: the trust it was kept
/// under (network, authority, genesis) and this Mac's seat.
pub(super) type MemoryScope = [u8; 16 + 32 + 32 + 48];
const REMEMBERED_ENTRY: usize = 48 + 4 + 2 + 8;
pub(super) const REMEMBERED_MAX: usize = REMEMBERED_MAGIC.len()
    + std::mem::size_of::<MemoryScope>()
    + 1
    + MAX_REMEMBERED * REMEMBERED_ENTRY;

impl Remembered {
    /// `entries`, kept for `scope`, in their one spelling: sorted by seat, at most
    /// [`MAX_REMEMBERED`].
    pub(super) fn encode(scope: &MemoryScope, entries: &[Self]) -> Vec<u8> {
        let mut sorted = entries.to_vec();
        sorted.sort_by_key(|entry| entry.actor);
        sorted.dedup_by_key(|entry| entry.actor);
        sorted.truncate(MAX_REMEMBERED);
        let mut bytes = Vec::with_capacity(REMEMBERED_MAX);
        bytes.extend_from_slice(REMEMBERED_MAGIC);
        bytes.extend_from_slice(scope);
        bytes.push(u8::try_from(sorted.len()).expect("bounded by MAX_REMEMBERED"));
        for entry in &sorted {
            let IpAddr::V4(ip) = entry.address.ip() else {
                unreachable!("only IPv4 endpoints are remembered")
            };
            bytes.extend_from_slice(&entry.actor);
            bytes.extend_from_slice(&ip.octets());
            bytes.extend_from_slice(&entry.address.port().to_be_bytes());
            bytes.extend_from_slice(&entry.at_ms.to_le_bytes());
        }
        bytes
    }
    /// The entries `bytes` spell for `scope` in their one spelling; another scope, or any other
    /// spelling, reads as none.
    pub(super) fn decode(scope: &MemoryScope, bytes: &[u8]) -> Option<Vec<Self>> {
        let rest = bytes
            .strip_prefix(REMEMBERED_MAGIC)?
            .strip_prefix(scope.as_slice())?;
        let (&[count], rest) = rest.split_first_chunk::<1>()?;
        let count = usize::from(count);
        if count > MAX_REMEMBERED || rest.len() != count * REMEMBERED_ENTRY {
            return None;
        }
        let mut entries: Vec<Self> = Vec::with_capacity(count);
        for entry in rest.as_chunks::<REMEMBERED_ENTRY>().0 {
            let actor: astream_wire::sync::Actor = entry[..48].try_into().expect("48 bytes");
            if entries.last().is_some_and(|last| last.actor >= actor) {
                return None;
            }
            let ip = std::net::Ipv4Addr::from(<[u8; 4]>::try_from(&entry[48..52]).expect("four"));
            let port = u16::from_be_bytes(entry[52..54].try_into().expect("two"));
            if port == 0 || !beaconable(ip) {
                return None;
            }
            entries.push(Self {
                actor,
                address: SocketAddr::from((ip, port)),
                at_ms: u64::from_le_bytes(entry[54..].try_into().expect("eight")),
            });
        }
        Some(entries)
    }
}

/// Whether a beacon or a remembered endpoint may name `ip`: no unspecified, multicast,
/// reserved or broadcast source.
fn beaconable(ip: std::net::Ipv4Addr) -> bool {
    !(ip.octets()[0] == 0 || ip.octets()[0] >= 224 || ip.is_broadcast())
}

#[cfg(test)]
thread_local! {
    /// MUTANT of CR1's dial order, on for one test thread: no candidate leads ([`Lead`]), so
    /// discovery dials in its address order alone, as it did before CR1.
    pub(super) static UNLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of CR1's outcome, on for one test thread: a deferred round that moved nothing is
    /// [`Outcome::Repaired`], as it was before CR1.
    pub(super) static DEFERRAL_SUCCEEDS: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// MUTANT of CR1's first cut, on for one test thread: every round a seat dials in pulls its
    /// endpoints' next dials in, failing or not ([`Peer::pulled`]).
    pub(super) static REPULLS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of CR1's first cut, on for one test thread: each round completed with a seat
    /// remembers its endpoint, so a seat on two attachments changes it every round.
    pub(super) static FLAPS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of CR1's first cut, on for one test thread: a led dial that authenticated nobody
    /// spends nothing ([`Hints::spent`]).
    pub(super) static UNSPENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of CR2's lab tag, on for one test thread: a beacon's tag is read as no lab's
    /// ([`Sender::Unnamed`]), as before CR2, so another lab's endpoints are candidates in address
    /// order and this lab's lead nothing.
    pub(super) static UNTAGGED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of CR2's spread, on for one test thread: endpoints go in address order on every
    /// node ([`Hints::spread`]), as before CR2.
    pub(super) static UNSPREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of CR2's spend rule, on for one test thread: every led dial that authenticated
    /// nobody drops its endpoint's lead, a stalled one included, as CR1 did
    /// ([`Repair::disowned`]).
    pub(super) static STALL_DISOWNS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the mutant `cell` is on (tests); never in a build.
#[cfg(test)]
fn mutant(cell: &'static std::thread::LocalKey<std::cell::Cell<bool>>) -> bool {
    cell.with(std::cell::Cell::get)
}

#[derive(Debug)]
pub(super) struct Hints {
    /// This node's lab's key, which its token's tag is drawn with ([`token`]); none on a
    /// presence-only transport, which reads every beacon as [`Sender::Unnamed`].
    lab: Option<LabKey>,
    /// This node's private draw for the order it dials endpoints in ([`Self::spread`]): random
    /// once per transport and never sent. None: address order (a test that names an order).
    seed: Option<[u8; 32]>,
    own: [u8; 32],
    previous: Option<[u8; 32]>,
    attempt: u64,
    peers: BTreeMap<SocketAddr, Peer>,
    presence: BTreeMap<[u8; 32], (IpAddr, Instant)>,
    cursor: Option<SocketAddr>,
    /// A push runs ([`Self::hurry`] until [`Self::calm`]).
    hurrying: bool,
    established_cursor: Option<SocketAddr>,
    follow_up: VecDeque<SocketAddr>,
    follow_up_left: usize,
    /// An incompatible build is not retried because time passed or a beacon repeated.
    needs_update: BTreeMap<astream_wire::sync::Actor, Option<String>>,
    versions: BTreeMap<astream_wire::sync::Actor, String>,
    addresses: BTreeMap<IpAddr, astream_wire::sync::Actor>,
    /// Seats that speak `Bus` ([`Self::bus_speakers`]): never dialled or hurried, still known.
    bus: BTreeSet<astream_wire::sync::Actor>,
    /// Addresses a peer authenticated from, each when it last did: a round this side dialled
    /// there, a round dialled from there ([`Self::vouch`]), or a remembered endpoint
    /// ([`Self::remember`]). A beacon from one leads ([`Lead::Kin`]). At most [`MAX_PEERS`],
    /// each kept [`VERIFIED_EXPIRES`].
    vouched: BTreeMap<IpAddr, Instant>,
    /// Addresses where a dial led by a beacon ([`Lead::Kin`] or [`Lead::Tagged`]) authenticated
    /// nobody, each when: a port beaconed from one, new or heard again once its endpoint expired,
    /// leads again only once a peer authenticates from there, or [`VERIFIED_EXPIRES`] later. The
    /// endpoint dialled keeps its lead unless the dial showed no peer of this lab is there
    /// ([`Repair::disowned`]), and backs off. Beacons forged from a peer's address lead at most
    /// [`PER_IP`] endpoints per round the peer makes from there. A tag is copied from any beacon
    /// heard and replayed from any address on the link, so forged tagged beacons lead at most
    /// [`PER_IP`] endpoints per address per [`VERIFIED_EXPIRES`], each dialled ahead of the crowd
    /// under its own backoff until it expires ([`EXPIRES`]), while fewer than [`MAX_PEERS`]
    /// addresses are spent, and the oldest is forgotten past that: the tag orders dials and
    /// protects nothing from an active attacker on the LAN, as no beacon did before it. At most
    /// [`MAX_PEERS`].
    spent: BTreeMap<IpAddr, Instant>,
    /// Each seat's endpoint that a round this side dialled last completed with, and when: what
    /// this Mac remembers across a restart ([`Self::remember`], [`Self::remembered`]). At most
    /// [`MAX_REMEMBERED`], the most recent.
    remembered: BTreeMap<astream_wire::sync::Actor, (SocketAddr, Instant)>,
    /// A seat's remembered endpoint changed since [`Self::remembered`] last read them.
    remembered_changed: bool,
    /// When [`Self::remembered`] last read them.
    remembered_read: Option<Instant>,
}

/// Whether the peer at `address` is a seat in `bus`, by its signed fleet advertisement's address
/// or its last authenticated round.
fn speaks_bus(
    bus: &BTreeSet<astream_wire::sync::Actor>,
    addresses: &BTreeMap<IpAddr, astream_wire::sync::Actor>,
    address: &SocketAddr,
    peer: &Peer,
) -> bool {
    !bus.is_empty()
        && addresses
            .get(&address.ip())
            .copied()
            .or_else(|| peer.repair.actor())
            .is_some_and(|actor| bus.contains(&actor))
}

impl Hints {
    /// Hints whose beacons carry `own` whole, naming no lab: a presence-only transport's.
    pub(super) fn new(own: [u8; 32]) -> Self {
        Self::of(None, own)
    }
    /// Hints of a node of the lab `key`, whose token carries its tag drawn with `nonce`
    /// ([`token`]), dialling in the order `seed` draws ([`Self::spread`]).
    pub(super) fn tagged(key: LabKey, nonce: [u8; 32], seed: [u8; 32]) -> Self {
        Self {
            seed: Some(seed),
            ..Self::of(Some(key), nonce)
        }
    }
    fn of(lab: Option<LabKey>, nonce: [u8; 32]) -> Self {
        Self {
            lab,
            seed: None,
            own: token(lab.as_ref(), nonce),
            previous: None,
            attempt: 0,
            peers: BTreeMap::new(),
            presence: BTreeMap::new(),
            cursor: None,
            hurrying: false,
            established_cursor: None,
            follow_up: VecDeque::new(),
            follow_up_left: FOLLOW_UP_BURST,
            needs_update: BTreeMap::new(),
            versions: BTreeMap::new(),
            addresses: BTreeMap::new(),
            bus: BTreeSet::new(),
            vouched: BTreeMap::new(),
            spent: BTreeMap::new(),
            remembered: BTreeMap::new(),
            remembered_changed: false,
            remembered_read: None,
        }
    }
    /// Seed what this Mac remembers from before it restarted, read at `now_ms` (unix ms): each
    /// endpoint verified within [`REMEMBER_MS`] becomes a candidate that leads
    /// ([`Lead::Remembered`]), and its address vouches for beacons from there ([`Lead::Kin`]).
    /// Nothing here is trust: each is verified only by a round that authenticates its seat. The
    /// most recent go first where the bounds hold fewer.
    pub(super) fn remember(&mut self, entries: Vec<Remembered>, now: Instant, now_ms: u64) {
        let mut entries: Vec<_> = entries
            .into_iter()
            .filter(|entry| now_ms.saturating_sub(entry.at_ms) < REMEMBER_MS)
            .collect();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.at_ms));
        entries.truncate(MAX_REMEMBERED);
        for entry in entries {
            let age = Duration::from_millis(now_ms.saturating_sub(entry.at_ms));
            let at = now.checked_sub(age).unwrap_or(now);
            self.remembered
                .entry(entry.actor)
                .or_insert((entry.address, at));
            self.vouch_address(entry.address.ip(), now);
            self.admit(entry.address, Lead::Remembered, now);
        }
    }
    /// What this Mac remembers now, with when each was last verified as `now` reads it, if it
    /// changed since the last call or that call was [`REMEMBER_REFRESH`] ago, so the times kept
    /// stay within it; a caller keeps them across a restart ([`Self::remember`]).
    pub(super) fn remembered(&mut self, now: Instant, now_ms: u64) -> Option<Vec<Remembered>> {
        let stale = self
            .remembered_read
            .is_none_or(|read| now.saturating_duration_since(read) >= REMEMBER_REFRESH);
        if !std::mem::take(&mut self.remembered_changed) && !(stale && !self.remembered.is_empty())
        {
            return None;
        }
        self.remembered_read = Some(now);
        Some(
            self.remembered
                .iter()
                .map(|(actor, (address, at))| Remembered {
                    actor: *actor,
                    address: *address,
                    at_ms: now_ms.saturating_sub(
                        u64::try_from(now.saturating_duration_since(*at).as_millis())
                            .unwrap_or(u64::MAX),
                    ),
                })
                .collect(),
        )
    }
    /// Remember `address` as `actor`'s endpoint, verified at `now`: the most recent
    /// [`MAX_REMEMBERED`] seats are kept. A changed endpoint or a new seat is a change to keep
    /// ([`Self::remembered`]); a later round with the same endpoint only refreshes its time, which
    /// the next change carries. While the endpoint remembered still answers, a round with another
    /// of the seat's (a Mac on Wi-Fi and Ethernet beacons from both) refreshes the one kept, so
    /// the file is written when an endpoint stops answering, not at every round.
    fn keep(&mut self, actor: astream_wire::sync::Actor, address: SocketAddr, now: Instant) {
        let IpAddr::V4(_) = address.ip() else {
            return;
        };
        #[cfg(test)]
        let flaps = mutant(&FLAPS);
        #[cfg(not(test))]
        let flaps = false;
        if let Some((old, _)) = self.remembered.get(&actor).copied()
            && old != address
            && !flaps
            && self.peers.get(&old).is_some_and(Peer::established)
        {
            self.remembered.insert(actor, (old, now));
            return;
        }
        let previous = self.remembered.insert(actor, (address, now));
        if previous.is_none_or(|(old, _)| old != address) {
            self.remembered_changed = true;
        }
        while self.remembered.len() > MAX_REMEMBERED {
            let oldest = self
                .remembered
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(actor, _)| *actor)
                .expect("over the bound");
            self.remembered.remove(&oldest);
        }
    }
    /// Note that a peer authenticated from `ip` at `now`, within the bounds of [`Self::vouched`].
    fn vouch_address(&mut self, ip: IpAddr, now: Instant) {
        self.spent.remove(&ip);
        self.vouched
            .retain(|_, at| now.saturating_duration_since(*at) < VERIFIED_EXPIRES);
        if !self.vouched.contains_key(&ip)
            && self.vouched.len() >= MAX_PEERS
            && let Some(oldest) = self
                .vouched
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(ip, _)| *ip)
        {
            self.vouched.remove(&oldest);
        }
        self.vouched.insert(ip, now);
    }
    /// A dial a beacon from `ip` led authenticated nobody at `now` ([`Self::spent`]).
    fn spend(&mut self, ip: IpAddr, now: Instant) {
        self.spent
            .retain(|_, at| now.saturating_duration_since(*at) < VERIFIED_EXPIRES);
        if !self.spent.contains_key(&ip)
            && self.spent.len() >= MAX_PEERS
            && let Some(oldest) = self
                .spent
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(ip, _)| *ip)
        {
            self.spent.remove(&oldest);
        }
        self.spent.insert(ip, now);
    }
    /// Whether a dial a beacon from `ip` led has authenticated nobody since a peer last
    /// authenticated from there, within [`VERIFIED_EXPIRES`] ([`Self::spent`]): a beacon from
    /// there leads nothing.
    fn is_spent(&self, ip: IpAddr, now: Instant) -> bool {
        self.spent
            .get(&ip)
            .is_some_and(|at| now.saturating_duration_since(*at) < VERIFIED_EXPIRES)
    }
    /// Whether a beacon from `ip` leads as kin ([`Lead::Kin`]): a peer authenticated from there
    /// within [`VERIFIED_EXPIRES`], or an endpoint there is verified, and the address is not
    /// [`Self::is_spent`].
    fn kin(&self, ip: IpAddr, now: Instant) -> bool {
        if self.is_spent(ip, now) {
            return false;
        }
        self.vouched
            .get(&ip)
            .is_some_and(|at| now.saturating_duration_since(*at) < VERIFIED_EXPIRES)
            || self
                .peers
                .iter()
                .any(|(address, peer)| address.ip() == ip && peer.verified)
    }
    /// A round dialled from `ip` authenticated `actor` and completed at `now`: the inbound
    /// side of what [`Self::finish`] does for a round this side dialled. The address vouches for
    /// its beacons ([`Lead::Kin`]), and each endpoint whose last authenticated round was with
    /// `actor` drops its backoff, its next dial due within the ordinary interval, as a completed
    /// round of its own would have it: once per failure streak ([`Peer::pulled`]), so the seat's
    /// endpoint that died (an old port, an address it left) backs off whatever the seat does. An
    /// endpoint bound to another seat is left alone, so an address one seat takes over from
    /// another moves nothing of the other's. Nothing is verified here: an endpoint is, only by a
    /// round dialled to it.
    ///
    /// `quiet` says the round moved nothing. The pair then has nothing to exchange that this
    /// side knows of, and the round it would dial next would find the same: each of the seat's
    /// endpoints at rest ([`Peer::resting`]) whose dial is not yet due waits the ordinary
    /// interval again from `now` instead of dialling inside it, so a quiet pair runs one round
    /// an interval where each side ran its own. Never past [`REVERIFY`] after the endpoint's
    /// last completed round: its dial is due then and goes before the ordinary scan
    /// ([`Peer::held`]), since only a round this side dials keeps the endpoint verified. An
    /// endpoint already due is left as it is: it waits for a place or for the scan's turn, and
    /// holding it back would have the scan pass it by for a whole round of the other
    /// endpoints, which on a Mac with many endpoints can outlast [`VERIFIED_EXPIRES`]. An
    /// endpoint a push, a local append or a new build made due, one whose own last round moved
    /// records, failed or left its hint a table to take, and every endpoint while a push runs,
    /// is not held back either.
    pub(super) fn vouch(
        &mut self,
        ip: IpAddr,
        actor: astream_wire::sync::Actor,
        quiet: bool,
        now: Instant,
    ) {
        self.vouch_address(ip, now);
        self.attempt = self.attempt.wrapping_add(1);
        let interval = now + retry_delay(&self.own, self.attempt);
        #[cfg(test)]
        let repulls = mutant(&REPULLS);
        #[cfg(not(test))]
        let repulls = false;
        let defers = quiet && !self.hurrying;
        for (address, peer) in &mut self.peers {
            if peer.repair.actor() == Some(actor) {
                let deferred = defers && peer.resting && !peer.busy && peer.next > now;
                if deferred {
                    peer.next = peer.next.max(interval.min(peer.seen + REVERIFY));
                    peer.held = true;
                }
                if peer.pulled && !repulls {
                    continue;
                }
                peer.pulled = true;
                peer.failures = 0;
                if !peer.busy && !deferred {
                    peer.next = peer.next.min(interval);
                }
            } else if address.ip() == ip
                && peer.repair.actor().is_none()
                && !peer.verified
                && peer.failures == 0
                && !peer.failed
            {
                // Only an endpoint no dial has yet failed: one that failed unauthenticated is not
                // this peer, and each later round from the address must not lead it again.
                peer.lead = peer.lead.max(Lead::Kin);
            }
        }
    }
    /// The seats that speak `Bus` now, replacing the last set: the mesh carries their tables, so
    /// [`Self::next_allowed`] never dials them and [`Self::hurry`] neither hurries nor counts
    /// them. They stay among the peers (no beacon, presence or progress is forgotten), and a seat
    /// that leaves the set is dialled again at its turn.
    pub(super) fn bus_speakers(&mut self, bus: BTreeSet<astream_wire::sync::Actor>) {
        self.bus = bus;
    }
    /// Draw a new token from `nonce`, keeping this node's lab ([`token`]); beacons of the last
    /// token are still known as this node's own.
    pub(super) fn rotate(&mut self, nonce: [u8; 32]) {
        self.previous = Some(self.own);
        self.own = token(self.lab.as_ref(), nonce);
    }
    /// Whose lab the sender of `token` names ([`Sender`]).
    fn sender(&self, token: &[u8]) -> Sender {
        #[cfg(test)]
        if mutant(&UNTAGGED) {
            return Sender::Unnamed;
        }
        match (&self.lab, token.strip_prefix(LAB_MARK.as_slice())) {
            (Some(key), Some(rest)) if rest.len() == 8 + LAB_NONCE => {
                if rest[..8] == tag(key, &rest[8..]) {
                    Sender::Ours
                } else {
                    Sender::Theirs
                }
            }
            _ => Sender::Unnamed,
        }
    }
    /// Where `address` falls in this node's dial order, among endpoints of one lead and in the
    /// ordinary scan: a hash of it under this node's private [`Self::seed`], so nodes that start
    /// together each dial in a different order rather than all the lowest address first, and the
    /// order holds across token rotations. Nothing a sender chooses or a listener learns. With no
    /// seed, every address falls at 0: address order.
    fn spread(&self, address: &SocketAddr) -> u64 {
        #[cfg(test)]
        if mutant(&UNSPREAD) {
            return 0;
        }
        let Some(seed) = &self.seed else {
            return 0;
        };
        let mut sha = aterm_digest::Sha256::new();
        sha.update(b"aterm/commons/discovery-spread/1\0");
        sha.update(seed);
        match address {
            SocketAddr::V4(v4) => sha.update(v4.ip().octets()),
            SocketAddr::V6(v6) => sha.update(v6.ip().octets()),
        }
        sha.update(address.port().to_be_bytes());
        u64::from_be_bytes(sha.finalize()[..8].try_into().expect("eight bytes"))
    }
    /// Only signed advertised builds from the local fleet book reach this method. A missing
    /// entry is no evidence of an update. The clock is supplied by the transport or test.
    pub(super) fn advertised_versions(
        &mut self,
        advertisements: BTreeMap<astream_wire::sync::Actor, (IpAddr, String)>,
        now: Instant,
    ) {
        // A missing cache entry does not erase the last seat bound to an address. A new
        // authenticated advertisement can move that seat or assign the address to another.
        self.addresses
            .retain(|_, actor| !advertisements.contains_key(actor));
        for (&actor, &(ip, _)) in &advertisements {
            self.addresses.insert(ip, actor);
        }
        self.needs_update.retain(|actor, blocked| {
            let version = advertisements.get(actor).map(|(_, version)| version);
            if blocked.is_none() {
                // Learning a previously unknown build is not a version change.
                *blocked = version.cloned();
                return true;
            }
            let changed = version.is_some_and(|version| blocked.as_ref() != Some(version));
            if changed {
                for (address, peer) in &mut self.peers {
                    if self.addresses.get(&address.ip()) == Some(actor)
                        || peer.repair.actor() == Some(*actor)
                    {
                        peer.next = now;
                        peer.resting = false;
                    }
                }
            }
            !changed
        });
        self.versions.extend(
            advertisements
                .into_iter()
                .map(|(actor, (_, version))| (actor, version)),
        );
    }
    pub(super) fn wire(&self, port: u16) -> [u8; WIRE_LEN] {
        let mut wire = [0u8; WIRE_LEN];
        wire[..4].copy_from_slice(b"ACH2");
        wire[4..36].copy_from_slice(&self.own);
        wire[36..38].copy_from_slice(&port.to_be_bytes());
        wire
    }
    pub(super) fn presence_wire(&self) -> [u8; 36] {
        let mut wire = [0u8; 36];
        wire[..4].copy_from_slice(b"ACH3");
        wire[4..36].copy_from_slice(&self.own);
        wire
    }
    pub(super) fn observe_presence(&mut self, wire: &[u8], source: SocketAddr, now: Instant) {
        self.presence
            .retain(|_, (_, seen)| now.saturating_duration_since(*seen) < EXPIRES);
        if wire.len() != 36 || &wire[..4] != b"ACH3" {
            return;
        }
        let token: [u8; 32] = wire[4..].try_into().unwrap();
        // Another lab's enrollee asks none of this lab's Macs for anything.
        if token == self.own
            || self.previous == Some(token)
            || self.sender(&token) == Sender::Theirs
        {
            return;
        }
        let IpAddr::V4(ip) = source.ip() else {
            return;
        };
        if !beaconable(ip) {
            return;
        }
        if let Some((address, seen)) = self.presence.get_mut(&token) {
            if *address == source.ip() {
                *seen = now;
            }
            return;
        }
        if self.presence.len() >= MAX_PEERS
            || self
                .presence
                .values()
                .filter(|(ip, _)| *ip == source.ip())
                .count()
                >= PER_IP
        {
            return;
        }
        self.presence.insert(token, (source.ip(), now));
    }
    fn expire(&mut self, now: Instant) {
        self.peers.retain(|_, p| {
            p.busy
                || (!p.verified && !p.failed)
                || now.saturating_duration_since(p.seen)
                    < if p.verified {
                        VERIFIED_EXPIRES
                    } else {
                        EXPIRES
                    }
        });
    }
    fn replacement(&self, source: Option<IpAddr>) -> Option<SocketAddr> {
        self.peers
            .iter()
            .filter(|(address, peer)| {
                source.is_none_or(|source| address.ip() == source)
                    && peer.failed
                    && !peer.busy
                    && (source.is_some() || !peer.verified)
            })
            // A new port on the same IP can replace a failed old listener.
            // Unrelated IPs cannot evict an authenticated peer with beacons.
            .min_by_key(|(_, peer)| (peer.verified, peer.seen))
            .map(|(address, _)| *address)
    }
    /// Read a beacon `wire` heard from `source`: one that is malformed, this node's own, or
    /// another lab's is dropped here, and any other is admitted ([`Self::admit`], the model's
    /// `Observe`) with the lead its sender and address give it.
    pub(super) fn observe(&mut self, wire: &[u8], source: SocketAddr, now: Instant) {
        if wire.len() != WIRE_LEN
            || &wire[..4] != b"ACH2"
            || wire[4..36] == self.own
            || self
                .previous
                .is_some_and(|previous| wire[4..36] == previous)
        {
            return;
        }
        let std::net::IpAddr::V4(ip) = source.ip() else {
            return;
        };
        if !beaconable(ip) {
            return;
        }
        let port = u16::from_be_bytes(wire[36..].try_into().unwrap());
        if port == 0 {
            return;
        }
        let address = SocketAddr::from((ip, port));
        // A peer that restarted beacons a new port from the address it authenticated from: that
        // endpoint is dialled ahead of the crowd ([`Lead::Kin`]); one that beacons this lab's tag
        // is dialled ahead of those that name none ([`Lead::Tagged`]); another lab's is never
        // dialled, since no round with it can complete. Each is still only a hint.
        let lead = match self.sender(&wire[4..36]) {
            Sender::Theirs => return,
            _ if self.kin(address.ip(), now) => Lead::Kin,
            Sender::Ours if !self.is_spent(address.ip(), now) => Lead::Tagged,
            Sender::Ours | Sender::Unnamed => Lead::None,
        };
        self.admit(address, lead, now);
    }
    /// Take `address` as a candidate leading by `lead`, within the per-IP and total bounds: a
    /// new one replaces only a failed endpoint ([`Self::replacement`]).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDiscoveryReplacement",
            action = "Observe",
            project = "aterm_db::runtime::discovery::admission_tests::replacement_transition"
        )
    )]
    fn admit(&mut self, address: SocketAddr, lead: Lead, now: Instant) {
        self.expire(now);
        // Repeated untrusted claims cannot pin either an untried or failed
        // endpoint forever. Only a successful authenticated round renews it.
        if let Some(peer) = self.peers.get_mut(&address) {
            // A remembered endpoint heard again from its vouched address is up: it leads as kin.
            // One first heard naming no lab (a forged beacon can come first) leads once its own
            // tagged beacon is heard, while no dial of it has failed unauthenticated.
            if lead > peer.lead && peer.may_lead() {
                peer.lead = lead;
            }
            return;
        }
        if self.peers.keys().filter(|p| p.ip() == address.ip()).count() >= PER_IP {
            let Some(replace) = self.replacement(Some(address.ip())) else {
                return;
            };
            self.peers.remove(&replace);
        }
        if self.peers.len() == MAX_PEERS {
            let Some(oldest) = self.replacement(None) else {
                return;
            };
            self.peers.remove(&oldest);
        }
        self.peers.insert(address, Peer::candidate(now, lead));
    }
    pub(super) fn count(&self) -> usize {
        self.peers.len()
    }
    pub(super) fn unverified_count(&self) -> usize {
        self.peers.values().filter(|peer| !peer.verified).count() + self.presence.len()
    }
    pub(super) fn expire_listen_only(&mut self, now: Instant) {
        self.peers
            .retain(|_, peer| now.saturating_duration_since(peer.seen) < EXPIRES);
    }
    pub(super) fn expire_presence(&mut self, now: Instant) {
        self.presence
            .retain(|_, (_, seen)| now.saturating_duration_since(*seen) < EXPIRES);
    }
    /// A completed authenticated round establishes an endpoint until a later
    /// attempt fails. An announcement alone never reserves worker capacity.
    pub(super) fn established(&self, address: SocketAddr) -> bool {
        self.peers.get(&address).is_some_and(Peer::established)
    }
    /// When `address` is next due (tests).
    #[cfg(test)]
    pub(super) fn next_at(&self, address: SocketAddr) -> Instant {
        self.peers[&address].next
    }
    /// Whether `address` waits in the follow-up queue (tests).
    #[cfg(test)]
    pub(super) fn follow_up_contains(&self, address: SocketAddr) -> bool {
        self.follow_up.contains(&address)
    }
    /// Whether a peer this side may dial is established; a `Bus` speaker reserves no capacity.
    pub(super) fn has_established(&self) -> bool {
        self.peers.iter().any(|(address, peer)| {
            peer.established() && !speaks_bus(&self.bus, &self.addresses, address, peer)
        })
    }
    #[cfg(test)]
    pub(super) fn next(&mut self, now: Instant) -> Option<(SocketAddr, Repair)> {
        self.next_allowed(now, true)
    }
    /// Keep a separate position when a reserved worker can serve established
    /// peers only. Those turns must not rewind the ordinary discovery scan.
    pub(super) fn next_allowed(
        &mut self,
        now: Instant,
        allow_discovery: bool,
    ) -> Option<(SocketAddr, Repair)> {
        self.expire(now);
        // A new port, a push or expired address metadata must not evade a build hold.
        self.peers.retain(|address, peer| {
            self.addresses
                .get(&address.ip())
                .copied()
                .or_else(|| peer.repair.actor())
                .is_none_or(|actor| !self.needs_update.contains_key(&actor))
        });
        self.follow_up
            .retain(|address| self.peers.contains_key(address));
        // A `Bus` speaker is never dialled; it stays known ([`Self::bus_speakers`]).
        let dialable = |a: &SocketAddr, p: &Peer| !speaks_bus(&self.bus, &self.addresses, a, p);
        let eligible = |a: &SocketAddr, p: &Peer| {
            !p.busy && p.next <= now && (allow_discovery || p.established()) && dialable(a, p)
        };
        let follow_up = if self.follow_up_left > 0 {
            self.follow_up.iter().copied().find(|address| {
                self.peers
                    .get(address)
                    .is_some_and(|peer| eligible(address, peer))
            })
        } else {
            None
        };
        // Keep capacity when every productive peer is still in its short
        // jittered wait. A ready queued peer can use it before then; an
        // ordinary slow dial could otherwise take it for a whole round.
        // A push wants every hurried peer dialled now ([`Self::hurry`]).
        if !self.hurrying
            && self.follow_up_left > 0
            && follow_up.is_none()
            && self.follow_up.iter().any(|address| {
                self.peers.get(address).is_some_and(|p| {
                    !p.busy && (allow_discovery || p.established()) && dialable(address, p)
                })
            })
        {
            return None;
        }
        let cursor = if allow_discovery {
            self.cursor
        } else {
            self.established_cursor
        };
        // A candidate that leads ([`Lead`]) goes before the ordinary scan, kin before remembered
        // before tagged, each lead in this node's own order ([`Self::spread`]), and leaves the
        // scan's place where it was.
        #[cfg(test)]
        let unled = UNLED.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let unled = false;
        let led = if follow_up.is_none() && !unled {
            self.peers
                .iter()
                .filter(|(a, p)| p.lead() > Lead::None && eligible(a, p))
                .max_by_key(|(a, p)| (p.lead(), std::cmp::Reverse((self.spread(a), **a))))
                .map(|(a, _)| *a)
        } else {
            None
        };
        // An endpoint its seat's quiet rounds held back ([`Self::vouch`]) is due for the round
        // that verifies it again once its last is [`REVERIFY`] old: it goes before the ordinary
        // scan, the longest unverified first, and leaves the scan's place where it was, so it
        // waits for a place and never for the scan to come round.
        let reverify = if follow_up.is_none() && led.is_none() {
            self.peers
                .iter()
                .filter(|(a, p)| {
                    p.held && now.saturating_duration_since(p.seen) >= REVERIFY && eligible(a, p)
                })
                .min_by_key(|(a, p)| (p.seen, **a))
                .map(|(a, _)| *a)
        } else {
            None
        };
        // The ordinary scan goes round every endpoint in this node's order ([`Self::spread`]),
        // from the one after where it last stopped.
        let address = follow_up.or(led).or(reverify).or_else(|| {
            let place = |a: &SocketAddr| (self.spread(a), *a);
            let after = cursor.as_ref().map(place);
            let mut first = None;
            let mut next = None;
            for (a, _) in self.peers.iter().filter(|(a, p)| eligible(a, p)) {
                let at = place(a);
                if first.is_none_or(|first| at < first) {
                    first = Some(at);
                }
                if after.is_none_or(|after| at > after) && next.is_none_or(|next| at < next) {
                    next = Some(at);
                }
            }
            next.or(first).map(|(_, a)| a)
        })?;
        let peer = self.peers.get_mut(&address).unwrap();
        peer.busy = true;
        peer.resting = false;
        peer.held = false;
        // Change jitter on every attempt: fixed per-owner jitter can keep two
        // peers with nearby deadlines colliding at the runtime's tick cadence.
        self.attempt = self.attempt.wrapping_add(1);
        peer.next = now + retry_delay(&self.own, self.attempt);
        if follow_up.is_some() {
            self.follow_up_left -= 1;
        } else if led.is_none() && reverify.is_none() {
            // A preferred transfer must not rewind the ordinary scan and keep
            // selecting the same neighbor after every burst.
            if allow_discovery {
                self.cursor = Some(address);
            } else {
                self.established_cursor = Some(address);
            }
            self.follow_up_left = FOLLOW_UP_BURST;
        }
        self.follow_up.retain(|queued| *queued != address);
        Some((address, std::mem::take(&mut peer.repair)))
    }
    /// When [`Self::next_allowed`] may next pick an endpoint, by the clock alone, as `now` reads
    /// it: at or before `now` where it may pick one now, else the earliest wait (`next`) of an
    /// endpoint it would pick once that wait ends, the follow-up queue's first where its hold
    /// keeps the rest back; `None` where it holds none it would ever pick by waiting (each it
    /// holds is in a round, a `Bus` speaker, held for a build, or established-only turns exclude
    /// it). It reads what [`Self::next_allowed`] reads and changes nothing; an endpoint whose
    /// expiry that call would take first can make it early, never late.
    pub(super) fn next_dial_at(&self, now: Instant, allow_discovery: bool) -> Option<Instant> {
        let candidate = |address: &SocketAddr, peer: &Peer| {
            !peer.busy
                && (allow_discovery || peer.established())
                && !speaks_bus(&self.bus, &self.addresses, address, peer)
                && self
                    .addresses
                    .get(&address.ip())
                    .copied()
                    .or_else(|| peer.repair.actor())
                    .is_none_or(|actor| !self.needs_update.contains_key(&actor))
        };
        // The follow-up hold ([`Self::next_allowed`]): while a queued endpoint waits and none is
        // due, the queue's first wait comes before every other endpoint's.
        if !self.hurrying && self.follow_up_left > 0 {
            let queued = self.follow_up.iter().filter_map(|address| {
                self.peers
                    .get(address)
                    .filter(|peer| candidate(address, peer))
                    .map(|peer| peer.next)
            });
            if let Some(first) = queued.clone().min()
                && queued.clone().all(|next| next > now)
            {
                return Some(first);
            }
        }
        self.peers
            .iter()
            .filter(|(address, peer)| candidate(address, peer))
            .map(|(_, peer)| peer.next)
            .min()
    }
    /// A push's hurry: dial every verified peer now, one whose round runs now again as that round
    /// ends; a `Bus` speaker is left to the mesh ([`Self::bus_speakers`]). Until [`Self::calm`],
    /// progress and the first quiet confirmation are followed at once, so a table first
    /// advertised in one round can be taken in the next. Further quiet rounds keep ordinary
    /// spacing. How many peers it hurried; none leaves nothing hurried.
    pub(super) fn hurry(&mut self, now: Instant) -> usize {
        let mut hurried = 0;
        let (bus, addresses) = (&self.bus, &self.addresses);
        for (_, peer) in self
            .peers
            .iter_mut()
            .filter(|(address, peer)| peer.verified && !speaks_bus(bus, addresses, address, peer))
        {
            peer.quiet_push = false;
            peer.resting = false;
            if peer.busy {
                peer.hurried = true;
            } else {
                peer.next = now;
            }
            hurried += 1;
        }
        self.hurrying = hurried > 0;
        hurried
    }
    /// This process appended to a table: dial each established peer now rather than at its
    /// turn (the endpoint's hints name the table, `crate::control::Endpoint::announce`). One
    /// whose round runs now is dialled again within the follow-up interval of that round's end,
    /// if it repairs ([`Self::finish`]): the round may have read the table before the record.
    /// A `Bus` speaker is left to the mesh, and an endpoint that is unverified or failing keeps
    /// its own schedule: the table reaches it at its turn. Only `next` moves: each dial still
    /// waits for a place and its spacing, so an append starts no dial that a due endpoint could
    /// not have started. How many endpoints it told.
    pub(super) fn announce(&mut self, now: Instant) -> usize {
        let mut told = 0;
        let (bus, addresses) = (&self.bus, &self.addresses);
        for (_, peer) in self.peers.iter_mut().filter(|(address, peer)| {
            peer.established() && !speaks_bus(bus, addresses, address, peer)
        }) {
            peer.resting = false;
            if peer.busy {
                peer.told = true;
            } else {
                peer.next = peer.next.min(now);
            }
            told += 1;
        }
        told
    }
    /// The push ended: each peer's rounds keep their ordinary spacing again.
    pub(super) fn calm(&mut self) {
        self.hurrying = false;
        for peer in self.peers.values_mut() {
            peer.hurried = false;
            peer.quiet_push = false;
        }
    }
    pub(super) fn finish(
        &mut self,
        address: SocketAddr,
        repair: Repair,
        outcome: Outcome,
        now: Instant,
    ) {
        let Some(peer) = self.peers.get_mut(&address) else {
            return;
        };
        if matches!(
            repair.application_cause(),
            Some(
                aterm_types::network_state::RepairCause::NeedsUpdate
                    | aterm_types::network_state::RepairCause::LegacyPastCrossing
            )
        ) && self.needs_update.len() < MAX_PEERS
            && let Some(actor) = repair
                .authenticated_actor()
                .or_else(|| self.addresses.get(&address.ip()).copied())
        {
            self.needs_update
                .insert(actor, self.versions.get(&actor).cloned());
        }
        // A slow exchange may have outlived the delay chosen at its start.
        // Start the quiet interval at completion so paired owners cannot keep
        // immediately redialing and colliding on their local custody locks;
        // when this round transferred new membership or application records, use
        // a short jittered follow-up so newly discovered tables sync promptly.
        // An endpoint that keeps not answering (or whose answer this side keeps
        // refusing) waits longer each time, so it cannot hold an outgoing worker
        // every few seconds.
        peer.failures = match outcome {
            Outcome::Repaired { .. } | Outcome::Deferred => 0,
            Outcome::Refused | Outcome::Busy => peer.failures.saturating_add(1),
        };
        let delay = match outcome {
            Outcome::Repaired { transferred: true } => follow_up_delay(&self.own, self.attempt),
            Outcome::Repaired { transferred: false } => retry_delay(&self.own, self.attempt),
            Outcome::Deferred if !peer.deferring => follow_up_delay(&self.own, self.attempt),
            Outcome::Deferred => retry_delay(&self.own, self.attempt),
            Outcome::Busy if peer.failures <= QUICK_BUSY_RETRIES => {
                retry_delay(&self.own, self.attempt)
            }
            Outcome::Busy => {
                backoff_delay(&self.own, self.attempt, peer.failures - QUICK_BUSY_RETRIES)
            }
            Outcome::Refused => backoff_delay(&self.own, self.attempt, peer.failures),
        };
        let success = matches!(outcome, Outcome::Repaired { .. });
        let transferred = matches!(outcome, Outcome::Repaired { transferred: true });
        let hurried = std::mem::take(&mut peer.hurried);
        let follow_push = self.hurrying && success && (transferred || !peer.quiet_push);
        if self.hurrying && success && !hurried {
            // An old round that was in flight when the push began cannot use the new push's
            // confirmation turn. A transferred page re-arms it; repeated silence does not.
            peer.quiet_push = !transferred;
        }
        let sooner = hurried || follow_push;
        peer.next = if sooner { now } else { now + delay };
        // A deferred round authenticated its peer and completed its control exchange: the
        // endpoint is established, though the round repaired nothing.
        let completed = success || outcome == Outcome::Deferred;
        peer.deferring = outcome == Outcome::Deferred;
        if completed {
            peer.pulled = false;
        }
        let verified = repair.actor().filter(|_| completed);
        if verified.is_some() {
            peer.verified = true;
            peer.seen = now;
        }
        let mut spent = false;
        if !completed && repair.authenticated_actor().is_none() {
            // A lead a beacon gave spends its address ([`Self::spent`]): what else is heard from
            // there takes its turn with the crowd. This endpoint loses its lead only where no peer
            // of this lab is there; one that stalled or went unanswered keeps it and backs off,
            // as a busy peer looks.
            spent = matches!(peer.lead(), Lead::Kin | Lead::Tagged);
            #[cfg(test)]
            let disowned = repair.disowned() || mutant(&STALL_DISOWNS);
            #[cfg(not(test))]
            let disowned = repair.disowned();
            if disowned {
                peer.lead = Lead::None;
            }
        }
        peer.busy = false;
        peer.failed = !completed;
        peer.repair = repair;
        // A local append came while this round ran, which may have read its table before the
        // record: where the round repaired, the next comes within the follow-up interval, as
        // after a transfer, and not at once, so appends that keep coming leave each round its
        // wait. A failed round keeps its backoff and a deferred one its own wait: the table
        // stays in the endpoint's hint for the round after.
        let told = std::mem::take(&mut peer.told) && success;
        if told {
            peer.next = peer
                .next
                .min(now + follow_up_delay(&self.own, self.attempt));
        }
        // An endpoint whose hint still names a table to take (announced, or one whose witness
        // differed) is not at rest: its seat's quiet rounds dialled in do not hold its dial
        // back ([`Self::vouch`]).
        peer.resting = outcome == (Outcome::Repaired { transferred: false })
            && !sooner
            && !told
            && !peer.repair.owes();
        #[cfg(test)]
        let spent = spent && !mutant(&UNSPENT);
        if spent {
            self.spend(address.ip(), now);
        }
        if let Some(actor) = verified {
            self.vouch_address(address.ip(), now);
            self.keep(actor, address, now);
        }
        self.follow_up
            .retain(|queued| self.peers.contains_key(queued));
        if matches!(outcome, Outcome::Repaired { transferred: true }) {
            // A later productive completion joins the queue; it must not erase
            // another peer's pending work. Requeue a continuing transfer after
            // the other peers while keeping the existing discovery burst cap.
            if !self.follow_up.contains(&address) {
                self.follow_up.push_back(address);
            }
        } else {
            self.follow_up.retain(|queued| *queued != address);
        }
    }
}

#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbDiscoveryReplacement",
        action = "AnotherLeft",
        reason = "Model bookkeeping: choose the first endpoint's completed or active state"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbDiscoveryReplacement",
        action = "AnotherRight",
        reason = "Model bookkeeping: choose the second endpoint's completed or active state"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbDiscoveryReplacement",
        action = "GlobalLimit",
        reason = "Model bookkeeping: choose separate or simultaneous per-IP and total capacity limits"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbDiscoveryReplacement",
        action = "RightOlder",
        reason = "Model bookkeeping: choose which endpoint was last confirmed earlier"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbDiscoveryReplacement",
        action = "Choose",
        reason = "Model bookkeeping: freeze the native endpoint fixture before Observe"
    )
)]
#[doc(hidden)]
pub fn db_discovery_replacement_environment() {}

#[cfg(test)]
mod admission_tests;
#[cfg(test)]
mod lab_tests;

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
