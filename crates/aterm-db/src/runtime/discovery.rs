// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded, volatile address hints. A beacon's token names no device: its nonce rotates within a
//! transport lifetime. A serving or enrolling node's token also carries a tag of its lab
//! ([`LabKey`]) drawn with that nonce, so a node that knows the lab's pins leaves another lab's
//! beacons undialled and dials its own lab's first (CR2), while a node that does not cannot link
//! one beacon to the next. A hint grants nothing, a tag included: only an authenticated round
//! verifies an endpoint.

use super::moved::Subject;
use crate::control::{Repair, Report};
use astream_wire::sync::{Actor, Digest};
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
/// The longest an established endpoint rests between two of this side's own dials of it,
/// counted from the round that set the rest ([`Hints::finish`]): its audit is due within this
/// ([`audit_rest`]), whatever rounds its seat dials in, pushes or beacons do. Only a round this
/// side dials verifies an endpoint, and one not verified again within [`VERIFIED_EXPIRES`] is
/// dropped, so an endpoint whose audit comes due is picked before the ordinary scan's turn
/// ([`Hints::next_allowed`]): it waits for a place and the dial spacing, behind a follow-up or
/// a led candidate that is due, and not for the scan to come round to it.
pub(super) const REVERIFY: Duration = Duration::from_secs(480);
const _: () = assert!(REVERIFY.as_secs() < VERIFIED_EXPIRES.as_secs());
/// How much of [`REVERIFY`] each audit's rest is drawn from ([`audit_rest`]): two minutes, so
/// the audits of the endpoints a start, a wake or a network change made due together spread
/// out again rather than come due in the same second at every turn.
const AUDIT_SPREAD: Duration = Duration::from_secs(120);
/// The least an audit's rest lasts ([`audit_rest`]): [`REVERIFY`] less [`AUDIT_SPREAD`], six
/// minutes. With `n` established endpoints a settled Mac dials about `n` rounds in seven minutes
/// on average, and serves as many.
pub(super) const AUDIT_FLOOR: Duration =
    Duration::from_secs(REVERIFY.as_secs() - AUDIT_SPREAD.as_secs());
// An audit's rest is the regular word on the catalog too (`Hints::catalog_due`): the oldest it
// may grow before the next audit leads it, and that audit's rest, fit in the import guard's
// quarter of an hour.
const _: () = assert!(
    (CATALOG_STALE.as_millis() + REVERIFY.as_millis()) <= crate::alab::command::FRESH_MS as u128
);
/// How long, by the wall clock, an endpoint may go without a round this side dialled to it
/// complete before the next one counts the table scan's turns from none again
/// ([`Repair::restart_cycle`]): fifteen minutes, past any audit's rest. A Mac back from a sleep
/// or a peer back from an absence ran no rounds over its tables meanwhile, so its next rounds
/// sweep each table both list again at the ordinary interval before it rests: what a
/// difference of conflict proofs alone, which no witness tells, needs to be repaired.
const ABSENT: Duration = Duration::from_secs(900);
const _: () = assert!(ABSENT.as_secs() > REVERIFY.as_secs());
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

/// How long an endpoint whose audit just completed rests before its next ([`Hints::finish`]):
/// [`AUDIT_FLOOR`] and up to [`AUDIT_SPREAD`] more, drawn afresh for each, so it is under
/// [`REVERIFY`].
fn audit_rest(own: &[u8; 32], attempt: u64) -> Duration {
    let hash = delay_hash(own, attempt);
    let spread = u64::try_from(AUDIT_SPREAD.as_millis()).expect("two minutes");
    let jitter = u64::from_be_bytes(hash[6..14].try_into().expect("eight bytes")) % spread;
    AUDIT_FLOOR + Duration::from_millis(jitter)
}

/// How many seats in a row may answer that they already held a table that moved here before the
/// walk that offers it ends ([`Hints::moved`]): what a peer already holds reached it another way,
/// and by then the rest of the lab is hearing of it from the peers this Mac did hand it to.
const COLD: usize = 2;
/// The most subjects a walk offers at once ([`Hints::moved`]): every table of a full list, the
/// control journal and the membership records.
const MAX_HOT: usize = crate::replica::MAX_TABLES + 2;
/// The least a walk ([`Hints::walk`]) leaves between an endpoint's last completed round and its
/// next dial: the follow-up interval's floor, as after a transfer. A round that began before a
/// move is walked again once it ends, and not at once, so writes that keep coming leave each
/// round its wait and two owners do not redial into each other's custody locks.
pub(super) const WALK_GAP: Duration = Duration::from_millis(200);
/// A dial handed a moved table also asks which tables differ once its endpoint's last completed
/// round ended this long ago, by this transport's own steady clock: the gap after which a round
/// asks by the wall clock (`crate::replica`), which a wall clock set back never reaches.
const ASK_GAP: Duration = Duration::from_millis(crate::replica::WITNESS_GAP_MS);

/// How long after an authenticated round with a seat this scheduler promises that seat's next
/// one ([`Hints::promise`]): eleven minutes. A settled endpoint's audit is due within
/// [`REVERIFY`] of its last round ([`Hints::finish`]) and goes before the ordinary scan, so its
/// next round comes within that and a place's wait; the three minutes past it are for that wait
/// and the round itself. A seat that leaves is found gone sooner by the dial that fails
/// ([`Standing`]), or its silence's probe ([`Hints::probe_at`]).
const PROMISE: Duration = Duration::from_secs(660);
const _: () = assert!(PROMISE.as_secs() > REVERIFY.as_secs() + 60);
// The observation's local relay carries no longer lease in the version it writes: a promise
// past it would leave every other process on this Mac with no observation to read.
const _: () = assert!(PROMISE.as_millis() <= crate::network_state::RELAYED_LEASE.as_millis());

/// How long an established endpoint that beaconed may then go unheard, by beacon or by round,
/// before one dial probes it ([`Hints::probe_at`]): six of its beacons, each sent every five
/// seconds and none of them acknowledged.
const SILENCE: Duration = Duration::from_secs(30);
/// Each endpoint's probe waits up to this much longer than [`SILENCE`], by this node's private
/// draw ([`Hints::spread`]): the Macs that all stop hearing one peer do not all dial it at once.
const SILENCE_JITTER_MS: u64 = 10_000;
/// How old an established endpoint's last round may grow once what its rest stood on moved
/// ([`Hints::unsettle`]): half of what a contact keeps of its lease then
/// (`crate::network_state`'s `CURTAILED`), so a peer still there whose last round was within
/// this long is reached again before it reads LastSeen, with as long again for its turn among
/// the dials that makes due.
const UNSETTLED: Duration = Duration::from_secs(60);
const _: () =
    assert!(UNSETTLED.as_millis() * 2 <= aterm_types::network_state::CURTAILED_MS as u128);

/// How old the newest round over the lab's catalog that left nothing to bring may grow, with
/// any seat and whichever side dialled it, before an audit leads the catalog
/// ([`Hints::catalog_due`]): five minutes. A round repairs one table, and an import into a
/// registry this Mac holds nothing of goes only on a round over the catalog from the last
/// quarter of an hour (`crate::alab`'s import guard), so a third of that leaves the next
/// audit its time to come round.
const CATALOG_STALE: Duration = Duration::from_secs(300);
const _: () = assert!(CATALOG_STALE.as_millis() * 3 <= crate::alab::command::FRESH_MS as u128);

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
    /// The peer authenticated and the control exchange completed, but the round's exchange of
    /// the membership records or of its table then failed ([`Repair::membership_error`],
    /// [`Repair::application_error`]): what the round was for is not done. The endpoint is
    /// established, as after any completed control exchange; the round completes no audit
    /// ([`Episode`]), and settles no seat for a table or the membership records
    /// ([`Hints::settle`]); and the endpoint is dialled again as after a busy round, the first
    /// [`QUICK_BUSY_RETRIES`] in a row at the ordinary interval and further ones backing off
    /// ([`Peer::unfinished`]), so a table whose exchange fails each time holds no worker, and
    /// none rests while its audit is not done.
    Unfinished,
    /// The round never reached the peer's answer: no connection, no TLS within the handshake's
    /// limit (a peer asleep, cut off, hung or saturated), a hello this side refused (its proof,
    /// witness or membership), or a round that panicked. Such a failure repeats until its cause
    /// changes, so failed rounds increase the wait ([`backoff_delay`]). Of whether the peer is
    /// still there it says nothing by itself: two in a row do ([`Standing`]).
    Refused,
    /// A [`Self::Refused`] round whose dial reached no peer at its address
    /// (`super::transport::dial_failure`'s `Unreachable`): its host refused the connection, did
    /// not answer within the connect limit, is down, or is on no route from this Mac. The
    /// endpoint waits as after any refusal, and the dial says its peer is not there
    /// ([`Standing`]).
    Unreached,
    /// A [`Self::Refused`] round in which this side never asked the peer anything: its transport
    /// stopped the round, this Mac's LAN is held for its broker, or the round's worker
    /// panicked. The endpoint waits as after any refusal; of the peer it says nothing.
    Unasked,
}
impl Outcome {
    /// What a finished round says of its endpoint. `success` is the worker's: the round
    /// completed, was not cancelled and authenticated its peer. `control` is
    /// the completed control exchange's durable acknowledgement report.
    pub(crate) fn of(success: bool, repair: &Repair, control: Option<Report>) -> Self {
        #[cfg(test)]
        let finishes = UNFINISHED_FINISHES.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let finishes = false;
        if success
            && !finishes
            && (repair.membership_error().is_some() || repair.application_error().is_some())
        {
            Self::Unfinished
        } else if success {
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
    /// This outcome, told apart by what its round's dial came to: `cause` is why the control
    /// exchange failed, if it did, and `cancelled` whether this side stopped the round. Only a
    /// round that reached nobody's answer ([`Self::Refused`]) is told apart: one this side
    /// stopped, or never sent for its held LAN, asked the peer nothing ([`Self::Unasked`]),
    /// and one whose dial reached no peer says so ([`Self::Unreached`]).
    pub(super) fn dialled(
        self,
        cause: Option<aterm_types::network_state::RepairCause>,
        cancelled: bool,
    ) -> Self {
        use aterm_types::network_state::RepairCause;
        match (self, cause) {
            (Self::Refused, _) if cancelled => Self::Unasked,
            (Self::Refused, Some(RepairCause::LanHeld)) => Self::Unasked,
            (Self::Refused, Some(RepairCause::Unreachable)) => Self::Unreached,
            (outcome, _) => outcome,
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

/// What an endpoint's dials say of whether its seat is still there: what decides when a dial
/// that fails tells the transport's owner the seat left ([`Hints::take_departures`]).
///
/// A seat is told gone once per stay, by its endpoint's own evidence and no other's: a dial
/// that reached no peer ([`Outcome::Unreached`], [`Repair::disowned`]) tells it at once, and one
/// that failed without saying whether a peer is there ([`Outcome::Refused`]: a stalled
/// handshake, a refused hello) only when it is the second in a row. A round the peer
/// authenticated in, completed or not, shows it there again, and one whose hello was taken
/// and went unanswered ([`Repair::unanswered`]) ends a doubt and shows no seat; a round this
/// side never asked ([`Outcome::Unasked`]) or could not start changes nothing. A beacon changes
/// nothing either: it is no evidence of a seat.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Standing {
    /// No round here authenticated a seat since one was last told gone, or ever: a dial that
    /// fails tells nothing more.
    #[default]
    Untold,
    /// The last dial here that said anything was answered by its seat.
    There,
    /// The seat was there, and one dial since failed without saying whether a peer is: a
    /// second in a row says it left.
    Doubted,
}

/// What the completed rounds this side dialled to an endpoint did since its last audit
/// completed ([`Peer::audit_due`]). An audit is complete once they asked the peer which tables
/// differ and the table scan took a regular turn (`crate::replica::Report::regular`), whichever
/// rounds did each: one such round sees every table whose witness differs and moves the scan on.
/// A round led to a moved table, a follow-up or a visit to a table that differed completes none
/// by itself. Until one does, the audit's clock stands ([`Hints::finish`]): an endpoint whose
/// audit is not yet due rests until it is, and one whose audit is due and not yet complete is
/// dialled again at the ordinary interval. A round whose table exchange failed
/// ([`Outcome::Unfinished`]) completes none either; its ask, where the peer answered it, still
/// counts, since the tables it found differing stay in the hints for the rounds after it.
///
/// A witness tells the operations a table holds, not its conflict proofs, so an audit's ask
/// finds no table whose difference is only of proofs. Such a difference is offered when the
/// proof is minted or received here (`super::moved`'s look counts proofs, and a walk's round
/// that sends one starts its count of cold answers again), swept with every table after a start
/// or an absence ([`ABSENT`]), and otherwise reached by the scan's turn, one table an audit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Episode {
    asked: bool,
    regular: bool,
}

impl Episode {
    /// Fold in what a round whose table exchange then failed did: `interrupted` is its table
    /// exchange as far as it went (`Repair::application_interrupted`), whose ask, if the peer
    /// answered it, is that of a completed round. It completes no audit.
    fn interrupted(&mut self, interrupted: Option<&crate::replica::Report>) {
        self.asked |= interrupted.is_some_and(|report| report.witnesses_asked);
    }
    /// Fold in what a completed round's table exchange did; `None` where it had none (a peer
    /// that answers no application exchange, or one this side repairs no tables with), whose
    /// control exchange is the whole of what an audit can do with it. Whether the audit is now
    /// complete.
    fn completes(&mut self, application: Option<&crate::replica::Report>) -> bool {
        match application {
            None => true,
            Some(report) => {
                self.asked |= report.witnesses_asked;
                // A round that found no table both list took the whole scan.
                self.regular |= report.regular || report.table.is_none();
                self.asked && self.regular
            }
        }
    }
}

/// One seat's newest exchanged lists. Equal clock readings need not describe the same
/// exchange: conflicting tied lists retain one bounded snapshot and its time, but prove no
/// table absent until a strictly newer exchange replaces them.
#[derive(Debug)]
struct Listed {
    actor: Actor,
    snapshot: crate::replica::Listing,
    ambiguous: bool,
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
    /// Consecutive rounds this side dialled whose control exchange completed and whose
    /// exchange of the membership records or a table then failed ([`Outcome::Unfinished`]):
    /// they back off as [`Self::failures`] do, and while any stands no walk dials the endpoint
    /// ([`Hints::walkable`]); its own next dial carries what it still owes. A round that
    /// finished ends the streak.
    unfinished: u32,
    /// Its last round ended [`Outcome::Deferred`].
    deferring: bool,
    /// A round its seat dialled in has pulled its next dial in since its own last completed
    /// round ([`Hints::vouch`]): once per failure streak, so a dead endpoint of a live seat
    /// still backs off.
    pulled: bool,
    /// The seat its round in flight was dialled as: what its last authenticated round bound, which
    /// `repair` does not say while the round holds it. None while it is idle.
    dialled: Option<Actor>,
    /// The moved table its round in flight was handed to lead with ([`Hints::moved`]): the round
    /// settles its seat for that table when it completes, whichever table it then took. None
    /// while it is idle.
    led: Option<Digest>,
    /// Its round in flight is an audit handed the lab's catalog to lead with
    /// ([`Hints::catalog_due`]): no other audit is handed it until this one ends. False while
    /// it is idle.
    catalog: bool,
    /// When its last dial began ([`Hints::next_allowed`]); when it became a candidate, until
    /// one did.
    began: Instant,
    /// When a beacon that named this endpoint was last heard ([`Hints::observe`]). It times
    /// the probe of an endpoint gone silent ([`Hints::probe_at`]) and nothing else: no beacon
    /// sets `seen`, `verified`, a wait or a contact.
    beacon: Option<Instant>,
    /// This silence's probe was dialled ([`Hints::probe_at`]): no other is, until a beacon is
    /// heard again.
    probed: bool,
    /// What its dials say of its seat's presence ([`Standing`]).
    standing: Standing,
    /// What its rest stood on moved while its round ran ([`Hints::unsettle`]): the round's end
    /// makes it due as that call made the others.
    unsettled: bool,
    /// When this side's next audit of the endpoint is due ([`Hints::finish`]): an
    /// [`audit_rest`] after the round that completed its last one, within [`REVERIFY`] of it.
    /// None until a first one completed. Only this side's own rounds move it: no round its seat
    /// dials in, push, walk or beacon does.
    audit_due: Option<Instant>,
    /// What the rounds this side dialled since its last audit completed did ([`Episode`]).
    episode: Episode,
    /// Its audit's dial was asked to audit ([`Repair::audit`]) and the audit has not yet
    /// completed: the rounds that finish it are not asked again, or each would ask and visit
    /// first the same table whose witness differs.
    auditing: bool,
    /// How long this Mac had slept, as the transport's owner told it ([`Hints::unsettle`]), when
    /// its last round this side dialled completed: what that round's age by the steady clock
    /// misses of its age by the wall clock ([`ABSENT`]).
    slept: Duration,
    /// The newest table lists this seat exchanged in either direction. Kept apart from
    /// `repair`, which an outgoing worker owns while it runs and replaces when it returns.
    /// The actor keeps a rebound endpoint from using its former seat's lists.
    listed: Option<Listed>,
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
            unfinished: 0,
            deferring: false,
            pulled: false,
            dialled: None,
            led: None,
            catalog: false,
            began: now,
            beacon: None,
            probed: false,
            standing: Standing::Untold,
            unsettled: false,
            audit_due: None,
            episode: Episode::default(),
            auditing: false,
            slept: Duration::ZERO,
            listed: None,
            repair: Repair::default(),
        }
    }
    /// The endpoint is at rest until its audit (tests): idle, its next dial is its audit
    /// ([`Self::audit_due`]), which [`Hints::finish`] sets only after a round that completed and
    /// moved nothing, whose hint names no table still to take ([`Repair::pending`]) and whose
    /// scan has taken its turn at each table both list ([`Repair::cycled`]). Whatever asks for
    /// a sooner dial ends the rest: a push, a seat's new build, an unsettling
    /// ([`Hints::unsettle`]), its own dial. A walk ([`Hints::walk`]) and a probe
    /// ([`Hints::probe_at`]) do not read it: neither reads the endpoint's own wait.
    #[cfg(test)]
    fn resting(&self) -> bool {
        !self.busy && self.audit_due == Some(self.next)
    }
    /// The seat its last authenticated round bound, its round in flight's included.
    fn seat(&self) -> Option<Actor> {
        self.dialled.or_else(|| self.repair.actor())
    }
    /// Keep only authenticated list evidence, without replacing scan state or requests.
    /// A round returning late keeps the time its lists were exchanged, so it cannot replace
    /// newer evidence learned from an incoming round while this endpoint was busy.
    fn remember_lists(&mut self, repair: &Repair) {
        let Some(actor) = repair.authenticated_actor() else {
            return;
        };
        if self
            .listed
            .as_ref()
            .is_some_and(|lists| lists.actor != actor)
        {
            self.listed = None;
        }
        let Some(lists) = repair.listed() else {
            return;
        };
        #[cfg(test)]
        let replace_old = mutant(&LISTS_ROLL_BACK);
        #[cfg(not(test))]
        let replace_old = false;
        if replace_old
            || self
                .listed
                .as_ref()
                .is_none_or(|previous| lists.newer_than(&previous.snapshot))
        {
            self.listed = Some(Listed {
                actor,
                snapshot: lists.clone(),
                ambiguous: false,
            });
        } else if let Some(previous) = &mut self.listed
            && lists.same_time(&previous.snapshot)
            && !lists.same_tables(&previous.snapshot)
        {
            #[cfg(test)]
            let learns_ties = !mutant(&TIED_LISTS_LEFT);
            #[cfg(not(test))]
            let learns_ties = true;
            previous.ambiguous |= learns_ties;
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
    /// MUTANT of the walk, on for one test thread: a move dials nobody ([`Hints::walk`]), so what
    /// moved waits for each endpoint's own next dial, as before the walk.
    pub(super) static NO_WALK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of the walk's end, on for one test thread: no answer is cold ([`COLD`]), so a
    /// table's walk dials every seat.
    pub(super) static WALK_UNENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of the walk, on for one test thread: the control journal and the membership
    /// records end at [`COLD`] as a table does, so a revocation stops at the second seat that
    /// already held it.
    pub(super) static CONTROL_CUTOFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of the walk, on for one test thread: the seat a move was received from is walked
    /// as any other ([`Hints::moved`]).
    pub(super) static SOURCE_WALKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of incoming list evidence: only outgoing completions refresh the scheduler's
    /// table lists, so a newly discovered table's reverse write can wait for the next audit.
    pub(super) static INCOMING_LISTS_LEFT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of list freshness: a round joined later replaces newer exchanged lists with its
    /// older cached view.
    pub(super) static LISTS_ROLL_BACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of tied list evidence: a distinct exchange at the same clock reading leaves an
    /// older negative list authoritative, so a newly discovered table is not offered back.
    pub(super) static TIED_LISTS_LEFT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of the walk's settlement, on for one test thread: a round settles its seat for a
    /// move seen after the round began ([`Hints::settle`]), though it may have read its
    /// inventory before the write.
    pub(super) static STALE_SETTLES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of the walk's settlement, on for one test thread: a seat is settled for a table
    /// only by a round over that table, so a seat whose rounds take another (a peer without the
    /// table) is dialled again at every turn.
    pub(super) static LED_UNSETTLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of the walk, on for one test thread: a seat's second endpoint is walked while a
    /// round with the seat is in flight at its first.
    pub(super) static TWIN_WALKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of the walk's hand-over, on for one test thread: a dial handed a moved table never
    /// asks which tables differ ([`ASK_GAP`]).
    pub(super) static WALK_UNASKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of [`Hints::next_dial_at`], on for one test thread: it reads the endpoints' waits
    /// alone, so a walk's dial waits for a wake by something else.
    pub(super) static WALK_UNTIMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of the walk, on for one test thread: it dials an endpoint the moment its last
    /// round ended ([`WALK_GAP`] not read).
    pub(super) static WALK_AT_ONCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of the beacon's reach, on for one test thread: a beacon that names an endpoint
    /// puts its next dial off by [`SILENCE`], so an endpoint that keeps beaconing (or whose
    /// beacons are forged) is never dialled by its own wait ([`Hints::observe`]).
    pub(super) static BEACON_DEFERS_AUDIT: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// MUTANT of the silence probe, on for one test thread: a silence is probed again after
    /// each round, [`SILENCE`] after it, however long no beacon is heard ([`Peer::probed`]).
    pub(super) static PROBE_REPEATS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of [`Hints::next_dial_at`], on for one test thread: it reads the endpoints' own
    /// waits and not their probes, so a probe's dial waits for a wake by something else.
    pub(super) static PROBE_UNTIMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of the departure rules, on for one test thread: every dial of a verified endpoint
    /// that does not complete tells its seat gone ([`Standing`]): one ambiguous failure, a
    /// round this side never asked, a busy peer, an endpoint already failed, and a dead old
    /// port beside the seat's live endpoint.
    pub(super) static ANY_FAILURE_DEPARTS: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// MUTANT of [`Hints::unsettle`], on for one test thread: it makes no endpoint due and asks
    /// no audit.
    pub(super) static STAYS_SETTLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of [`Hints::catalog_due`], on for one test thread: no audit leads the catalog,
    /// however long no round over it left nothing to bring, so the catalog waits for the scan's
    /// own turn at it.
    pub(super) static CATALOG_UNLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of [`Hints::catalog_due`], on for one test thread: every audit leads the catalog,
    /// whatever a round over it said since and whichever audit already carries it.
    pub(super) static CATALOG_EVERY_AUDIT: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// MUTANT of the rest, on for one test thread: a quiet round's endpoint is dialled again an
    /// ordinary interval after it, as before the rest ([`Hints::finish`]).
    pub(super) static QUIET_REDIALS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of the rest, on for one test thread: a quiet round rests its endpoint until its
    /// audit whatever its hint still names and however far the scan has come
    /// ([`Repair::pending`], [`Repair::cycled`]).
    pub(super) static RESTS_PENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of the audit's clock, on for one test thread: every completed round this side
    /// dialled completes an audit ([`Episode`]), a walk's and a follow-up's included, so a run
    /// of them puts the audit off for good.
    pub(super) static ANY_ROUND_AUDITS: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// MUTANT of the audit's dial, on for one test thread: each dial of an endpoint whose audit
    /// is due asks it to audit ([`Peer::auditing`]), the rounds that finish the audit included.
    pub(super) static AUDIT_REASKS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of the audit's dial, on for one test thread: the dial of an endpoint whose audit
    /// is due is not asked to audit ([`Repair::audit`]), so its round asks no peer which tables
    /// differ inside the witness gap and takes the scan's next table.
    pub(super) static AUDIT_UNASKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// MUTANT of the absence rule, on for one test thread: a round after [`ABSENT`] keeps the
    /// scan's count of turns, so a Mac back from a sleep rests at once.
    pub(super) static ABSENCE_KEEPS_CYCLE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// MUTANT of [`Outcome::Unfinished`], on for one test thread: a round whose table or
    /// membership exchange failed after its control exchange counts as one that finished, so
    /// it completes its audit and rests.
    pub(super) static UNFINISHED_FINISHES: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// MUTANT of [`Hints::settle`], on for one test thread: a round whose table or membership
    /// exchange failed settles its seat for the membership records and its led table as one
    /// that finished does.
    pub(super) static UNFINISHED_SETTLES: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// MUTANT of [`Hints::moved`], on for one test thread: every seat a move was received from
    /// is tried for it, though its round began before an earlier move of the same subject that
    /// it then could not have carried.
    pub(super) static STALE_SOURCE_TRIED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// MUTANT of [`Hints::cool`], on for one test thread: only established endpoints' seats
    /// keep a moved subject, so one whose dial just failed drops it and its own next dial leads
    /// nothing.
    pub(super) static COOLS_UNESTABLISHED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// MUTANT of the walk, on for one test thread: a seat is walked for a moved table whatever
    /// its last round's lists said ([`Repair::lists`]), so a seat that holds none of it is
    /// dialled for it, twice.
    pub(super) static WALKS_UNLISTED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// MUTANT of [`Hints::next_allowed`], on for one test thread: an endpoint whose audit is due
    /// waits for the ordinary scan's turn.
    pub(super) static AUDIT_UNPICKED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Whether the mutant `cell` is on (tests); never in a build.
#[cfg(test)]
fn mutant(cell: &'static std::thread::LocalKey<std::cell::Cell<bool>>) -> bool {
    cell.with(std::cell::Cell::get)
}

/// One subject that moved here and the walk that offers it ([`Hints::moved`]).
#[derive(Debug)]
struct Hot {
    /// When the move was seen: only a round that began at or after it settles a seat
    /// ([`Hints::settle`]).
    since: Instant,
    /// How many seats in a row answered that they already held all of it ([`COLD`]).
    cold: usize,
    /// The seats that hold it, were offered it, or were handed it to lead with: no walk dials
    /// them for it again. At most [`MAX_PEERS`].
    tried: BTreeSet<Actor>,
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
    /// What moved here and is still offered to seats that may lack it ([`Self::moved`]). At most
    /// [`MAX_HOT`].
    hot: BTreeMap<Subject, Hot>,
    /// The seats found gone since [`Self::take_departures`], each with when the dial that found
    /// it so began ([`Standing`]). At most [`MAX_PEERS`].
    departed: BTreeMap<Actor, Instant>,
    /// The lab catalog's table, where this transport's rounds repair it
    /// ([`Self::keep_catalog`]); none where they take no Commons tables.
    catalog: Option<Digest>,
    /// When the newest round over the catalog that left nothing to bring began, with any seat
    /// ([`Self::told`]); none until one did, or since one that found more.
    catalog_heard: Option<Instant>,
    /// How long this Mac slept while the transport ran, as its owner told it
    /// ([`Self::unsettle`]): the steady clock stops in a sleep, so an endpoint's age by the wall
    /// clock is its age by this one plus what this grew by since its last round ([`ABSENT`]).
    slept: Duration,
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

/// Whether a walk may offer `subject` to the seat whose hints are `repair` ([`Hints::moved`]):
/// every seat takes the control journal and the membership records, and a table, unless the last
/// round with the seat that traded table lists found it not among the tables both list
/// ([`Repair::listed`]). A seat that holds none of a table (it subscribes to none of it, its
/// catalog excludes it, or it repairs no tables) takes nothing of it from a round led to it,
/// and one that subscribes to it since is its own walk's to fill (its look finds the table's
/// store as it is enrolled); while nothing tells of its tables, it is offered each.
fn may_take(peer: &Peer, subject: &Subject) -> bool {
    #[cfg(test)]
    if mutant(&WALKS_UNLISTED) {
        return true;
    }
    match subject {
        Subject::Table(table) => peer
            .listed
            .as_ref()
            .filter(|lists| Some(lists.actor) == peer.seat())
            .is_none_or(|lists| lists.ambiguous || lists.snapshot.contains(table)),
        Subject::Control | Subject::Members => true,
    }
}

impl Hints {
    /// How long after an authenticated round with a seat its next one is promised, whichever
    /// side dialled ([`PROMISE`]): the lease that round's contact gives the seat in this Mac's
    /// network observation (`crate::network_state`). Only a round that authenticated its peer is
    /// promised anything: a beacon, a hint and a failed dial are not contacts.
    pub(super) const fn promise() -> Duration {
        PROMISE
    }
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
            hot: BTreeMap::new(),
            departed: BTreeMap::new(),
            catalog: None,
            catalog_heard: None,
            slept: Duration::ZERO,
        }
    }
    /// Keep the lab's catalog heard: `table` is its table in the rounds this transport's
    /// endpoint makes (`crate::control::Endpoint::catalog_table`), none where it repairs no
    /// Commons tables. An audit then leads it once no round over it has left nothing to bring
    /// for [`CATALOG_STALE`] ([`Self::catalog_due`]).
    pub(super) fn keep_catalog(&mut self, table: Option<Digest>) {
        self.catalog = table;
    }
    /// A round over `table` that began at `started` ended with a seat and said something of
    /// the table (`aterm_types::network_state::RepairObservation::spoke`), whichever side
    /// dialled it; `left_nothing` where it exchanged the table and brought nothing. Where the
    /// table is the lab's catalog, a round that left nothing is when the catalog was last
    /// heard ([`Self::catalog_due`]), and one that brought or deferred records, begun no
    /// earlier, leaves it unheard: the network observation keeps the latest of the two kinds
    /// (`NetworkObservation::last_catalog_repair`), so the next audit leads the catalog again
    /// and a round that brings nothing follows the one that found more. Only the transport's
    /// authenticated rounds are told here; one that failed with nothing is not.
    pub(super) fn told(&mut self, table: Digest, started: Instant, left_nothing: bool) {
        if self.catalog != Some(table) {
            return;
        }
        if left_nothing {
            self.catalog_heard = Some(
                self.catalog_heard
                    .map_or(started, |heard| heard.max(started)),
            );
        } else if self.catalog_heard.is_some_and(|heard| heard <= started) {
            self.catalog_heard = None;
        }
    }
    /// The lab catalog's table where an audit dialled at `now` leads it: no round over the
    /// catalog has left nothing to bring, with any seat, for [`CATALOG_STALE`] (or ever, on
    /// this transport, or since one that found more: [`Self::told`]), and no audit in flight
    /// already carries it.
    ///
    /// Each round repairs one table, and an audit takes the one its peer's scan has come to:
    /// the catalog once in as many audits as the two share tables. What the lab last said of
    /// the catalog is what lets an import into a registry this Mac holds nothing of go on
    /// (`crate::alab`'s import guard), and only for a quarter of an hour. So the catalog rides
    /// the audits: one at a time, with whichever seat's audit comes next, and none while a
    /// round over it is that fresh. An ordinary dial leads nothing, so no endpoint is dialled
    /// sooner for this and the scan keeps its turns. A lead orders tables and grants nothing:
    /// the round's own lists and inventories decide what is repaired, and it sets no contact,
    /// lease, `verified` or `seen`.
    fn catalog_due(&self, now: Instant) -> Option<Digest> {
        #[cfg(test)]
        if mutant(&CATALOG_UNLED) {
            return None;
        }
        #[cfg(test)]
        if mutant(&CATALOG_EVERY_AUDIT) {
            return self.catalog;
        }
        let fresh = self
            .catalog_heard
            .is_some_and(|heard| now.saturating_duration_since(heard) < CATALOG_STALE);
        let carried = self.peers.values().any(|peer| peer.catalog);
        self.catalog.filter(|_| !fresh && !carried)
    }
    /// When a round over the lab's catalog last left nothing to bring, if none since found
    /// more (tests).
    #[cfg(test)]
    pub(super) fn catalog_heard(&self) -> Option<Instant> {
        self.catalog_heard
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
    /// `actor` and whose own last dial failed drops its backoff, its next dial due within the
    /// ordinary interval, as a completed round of its own would have it: once per failure
    /// streak ([`Peer::pulled`]), so the seat's endpoint that died (an old port, an address it
    /// left) backs off whatever the seat does. An endpoint bound to another seat is left alone,
    /// so an address one seat takes over from another moves nothing of the other's. Nothing is
    /// verified here: an endpoint is, only by a round dialled to it.
    ///
    /// A healthy endpoint of the seat is left as it is, at rest or due, whatever the round
    /// moved: its audit runs on this side's own clock ([`Peer::audit_due`]), which no round
    /// dialled in moves either way. What moved here is offered by a walk ([`Self::walk`]), which
    /// does not read an endpoint's wait.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDialCadence",
            action = "Vouch",
            project = "aterm_db::runtime::discovery::cadence_tests::dial_cadence_transition"
        )
    )]
    pub(super) fn vouch(&mut self, ip: IpAddr, actor: astream_wire::sync::Actor, now: Instant) {
        self.vouch_address(ip, now);
        self.attempt = self.attempt.wrapping_add(1);
        let interval = now + retry_delay(&self.own, self.attempt);
        #[cfg(test)]
        let repulls = mutant(&REPULLS);
        #[cfg(not(test))]
        let repulls = false;
        for (address, peer) in &mut self.peers {
            if peer.repair.actor() == Some(actor) {
                let failing = peer.failed || peer.failures > 0;
                if !repulls && (!failing || peer.pulled) {
                    continue;
                }
                peer.pulled = true;
                peer.failures = 0;
                if !peer.busy {
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
    /// `subject` moved here, as a look at `now` found (`super::moved::Look`); `sources` are the
    /// seats it was received from since the look before, each with when the round that brought
    /// it began. A walk offers it: [`Self::next_allowed`] dials, one at a time and in this
    /// node's order ([`Self::spread`]), each established endpoint whose seat has not been tried
    /// for it and may take it ([`may_take`]), whether or not that endpoint's own wait is over,
    /// and hands the dial a moved table to lead with. Each completed round settles its seat
    /// ([`Self::settle`]), so a walk dials a seat once per move. A table's walk ends once
    /// [`COLD`] seats in a row already held it; the control journal's and the membership
    /// records' reach every established seat, so a revocation is never left to that cutoff. A
    /// seat the walk does not reach keeps its own wait: a move makes no endpoint due. Past
    /// [`MAX_HOT`] subjects a new one is not kept, and each endpoint's own dials carry it.
    ///
    /// A subject that moves again starts again: what moved since may be what every seat lacks.
    /// A source holds what it sent, and is tried for the move, only where it could also hold
    /// what moved before: where the subject's earlier move is still offered, only a source whose
    /// round began once that move was seen, which repaired it both ways, is tried. One whose
    /// round began before (a long round that brought a control node while a revocation was
    /// written here) is walked like any other seat. A move seen in the same look as what a
    /// source sent is the residue: the seats this walk reaches offer it on in their own walks,
    /// and the next audit between the source and this Mac brings it at the latest.
    ///
    /// A walk orders dials and grants nothing: it sets no contact, lease, `verified` or `seen`,
    /// and each round's own lists and inventories decide what is repaired.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDialCadence",
            action = "Move",
            project = "aterm_db::runtime::discovery::cadence_tests::dial_cadence_transition"
        )
    )]
    pub(super) fn moved(
        &mut self,
        subject: Subject,
        sources: impl IntoIterator<Item = (Actor, Instant)>,
        now: Instant,
    ) {
        if !self.hot.contains_key(&subject) && self.hot.len() >= MAX_HOT {
            return;
        }
        let earlier = self.hot.get(&subject).map(|hot| hot.since);
        #[cfg(test)]
        let earlier = earlier.filter(|_| !mutant(&STALE_SOURCE_TRIED));
        #[cfg(test)]
        let sources = sources.into_iter().filter(|_| !mutant(&SOURCE_WALKED));
        let tried = sources
            .into_iter()
            .filter(|(_, began)| earlier.is_none_or(|since| *began >= since))
            .map(|(actor, _)| actor)
            .take(MAX_PEERS)
            .collect();
        self.hot.insert(
            subject,
            Hot {
                since: now,
                cold: 0,
                tried,
            },
        );
    }
    /// A round with the seat `actor` that began at `started` completed its control exchange,
    /// leaving `repair`: one this side dialled to `dialled`, or one the seat dialled in. Its
    /// exchanged lists refresh matching endpoints, including busy ones, by their exchange time.
    /// This leaves the endpoints' own clocks, requests and repair snapshots alone. The round
    /// settles the seat for each subject whose move was seen by the time the round began and
    /// that the round exchanged: the control journal, which every such round repairs; the
    /// membership records, which every round that negotiates them exchanges; a table the round
    /// was over (its application report's); and the table its dial was handed to lead with
    /// ([`Peer::led`]), whichever table the round then took (a push's, a follow-up, or none where
    /// the peer lacks the table). The lead stays with the peer's hints for its next round, and a
    /// seat left untried there would be dialled again at every turn. A round that began before a
    /// move was seen settles nothing for it: it may have read its inventory before the write. A
    /// round whose exchange of the membership records or of its table failed after its control
    /// exchange ([`Outcome::Unfinished`]) settles the control journal alone: the records and the
    /// table may not have reached the seat, which its own next dial still offers them to.
    ///
    /// A round over a moved table also counts toward its walk's end. One that sent the peer
    /// anything shows the peer lacked it, and the count of cold answers starts again; one that
    /// caught the peer up and sent nothing is a cold answer, counted once per seat. At [`COLD`]
    /// the table leaves the walk, as a subject does whose seats outgrow [`MAX_PEERS`].
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDialCadence",
            action = "WalkSends",
            project = "aterm_db::runtime::discovery::cadence_tests::dial_cadence_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDialCadence",
            action = "WalkCold",
            project = "aterm_db::runtime::discovery::cadence_tests::dial_cadence_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDialCadence",
            action = "WalkLacks",
            project = "aterm_db::runtime::discovery::cadence_tests::dial_cadence_transition"
        )
    )]
    pub(super) fn settle(
        &mut self,
        actor: Actor,
        dialled: Option<SocketAddr>,
        started: Instant,
        repair: &Repair,
    ) {
        #[cfg(test)]
        let learns = !mutant(&INCOMING_LISTS_LEFT);
        #[cfg(not(test))]
        let learns = true;
        if learns && repair.authenticated_actor() == Some(actor) {
            for peer in self
                .peers
                .values_mut()
                .filter(|peer| peer.seat() == Some(actor))
            {
                peer.remember_lists(repair);
            }
        }
        let led = dialled
            .and_then(|address| self.peers.get(&address))
            .and_then(|peer| peer.led);
        let application = repair.application();
        let unfinished =
            repair.membership_error().is_some() || repair.application_error().is_some();
        #[cfg(test)]
        let (led, stale, unending, cutoff, unfinished) = (
            led.filter(|_| !mutant(&LED_UNSETTLED)),
            mutant(&STALE_SETTLES),
            mutant(&WALK_UNENDING),
            mutant(&CONTROL_CUTOFF),
            unfinished && !mutant(&UNFINISHED_SETTLES),
        );
        #[cfg(not(test))]
        let (stale, unending, cutoff) = (false, false, false);
        self.hot.retain(|subject, hot| {
            if started < hot.since && !stale {
                return true;
            }
            let over = match subject {
                Subject::Control => None,
                Subject::Members | Subject::Table(_) if unfinished => return true,
                Subject::Table(table) => {
                    let over = application.filter(|report| report.table == Some(*table));
                    if over.is_none() && led != Some(*table) {
                        return true;
                    }
                    over
                }
                Subject::Members => None,
            };
            if !hot.tried.contains(&actor) && hot.tried.len() >= MAX_PEERS {
                return false;
            }
            let new = hot.tried.insert(actor);
            match over {
                Some(report) if report.sent + report.conflicts_sent > 0 => hot.cold = 0,
                Some(report) if report.caught_up && new && !unending => hot.cold += 1,
                None if cutoff && new && !matches!(subject, Subject::Table(_)) => hot.cold += 1,
                _ => {}
            }
            hot.cold < COLD
        });
    }
    /// When the silence of the endpoint at `address` calls for its probe, if it does: an
    /// established endpoint that beaconed and has since gone [`SILENCE`] (and this node's
    /// jitter for it, [`SILENCE_JITTER_MS`]) with neither a beacon nor a completed round is
    /// dialled once, whatever its own wait, and that dial is an audit ([`Repair::audit`]). A
    /// peer that left this way is found gone by that dial ([`Standing`]) and not at its lease's
    /// end. One probe a silence: the next waits for a beacon to be heard again
    /// ([`Peer::probed`]), so a peer whose beacons stop reaching this Mac costs one round, and
    /// its own wait still dials it.
    ///
    /// A beacon grants nothing here. It only puts off this one early dial: an endpoint whose
    /// beacons keep coming, its own or forged, is dialled by its own wait as if none came, and
    /// forged beacons that stop can cost one round per endpoint per silence.
    fn probe_at(&self, address: &SocketAddr, peer: &Peer) -> Option<Instant> {
        #[cfg(test)]
        let probed = peer.probed && !mutant(&PROBE_REPEATS);
        #[cfg(not(test))]
        let probed = peer.probed;
        if probed || !peer.established() {
            return None;
        }
        let heard = peer.beacon?.max(peer.seen);
        let jitter = Duration::from_millis(self.spread(address) % SILENCE_JITTER_MS);
        Some(heard + SILENCE + jitter)
    }
    /// Whether `peer`'s audit is due at `now` ([`Peer::audit_due`]): one completed, and its rest
    /// is over.
    fn audit_due(peer: &Peer, now: Instant) -> bool {
        peer.audit_due.is_some_and(|due| due <= now)
    }
    /// When the endpoint at `address` is next due a dial: when its own wait ends, or its
    /// silence calls for its probe ([`Self::probe_at`]), whichever is first.
    fn due(&self, address: &SocketAddr, peer: &Peer) -> Instant {
        self.probe_at(address, peer)
            .map_or(peer.next, |probe| probe.min(peer.next))
    }
    /// What the established endpoints' rests stood on moved at `now`: a link of this Mac's
    /// opened, or it woke from sleep, so a peer last reached before then may be on a network
    /// this Mac left. Each established endpoint is due once its last round is [`UNSETTLED`]
    /// old, now where it already is, and its next round is an audit ([`Repair::audit`]); one
    /// whose round is in flight is, counted from that round's end ([`Peer::unsettled`]). An
    /// endpoint that is due sooner keeps its wait, and one that is not established keeps its
    /// backoff. The caps on dials spread the rounds this makes due. `slept` is how long this Mac
    /// slept, if that is what moved (zero for a link): an endpoint's age by the wall clock
    /// counts it ([`ABSENT`]).
    pub(super) fn unsettle(&mut self, now: Instant, slept: Duration) {
        self.slept = self.slept.saturating_add(slept);
        #[cfg(test)]
        if mutant(&STAYS_SETTLED) {
            return;
        }
        for peer in self.peers.values_mut().filter(|peer| peer.established()) {
            if peer.busy {
                peer.unsettled = true;
            } else {
                peer.next = peer.next.min(now.max(peer.seen + UNSETTLED));
                peer.repair.audit();
            }
        }
    }
    /// The seats found gone since the last call, each with when the dial that found it so
    /// began ([`Standing`]): a round with the seat that authenticated at or after then shows it
    /// there. Only a dial of an endpoint its seat authenticated at tells one, and never while
    /// the seat has another established endpoint (a Mac's old port beside its new one, a
    /// second attachment).
    pub(super) fn take_departures(&mut self) -> Vec<(Actor, Instant)> {
        std::mem::take(&mut self.departed).into_iter().collect()
    }
    /// The dial of `seat`'s endpoint at `address` that began at `at` found it gone: tell it,
    /// unless the seat has another established endpoint.
    fn depart(&mut self, address: SocketAddr, seat: Actor, at: Instant) {
        #[cfg(test)]
        let alone = mutant(&ANY_FAILURE_DEPARTS);
        #[cfg(not(test))]
        let alone = false;
        let elsewhere = self.peers.iter().any(|(other, peer)| {
            *other != address && peer.established() && peer.seat() == Some(seat)
        });
        if elsewhere && !alone {
            return;
        }
        if let Some(told) = self.departed.get_mut(&seat) {
            *told = (*told).max(at);
        } else if self.departed.len() < MAX_PEERS {
            self.departed.insert(seat, at);
        }
    }
    /// Drop each moved subject nothing can still offer: every verified endpoint this side may
    /// dial has a seat tried for it, its round in flight's included, or one that does not take
    /// it ([`may_take`]). An endpoint whose last dial failed keeps the subjects its seat was not
    /// tried for: a walk passes it by while it is not established ([`Self::walkable`]), and its
    /// own next dial, at its retry or its backoff, leads with what it is still owed
    /// ([`Self::next_allowed`]). One that stops answering keeps them until it expires
    /// ([`VERIFIED_EXPIRES`]), which dials nothing more.
    fn cool(&mut self) {
        if self.hot.is_empty() {
            return;
        }
        #[cfg(test)]
        let established_only = mutant(&COOLS_UNESTABLISHED);
        #[cfg(not(test))]
        let established_only = false;
        let (bus, addresses) = (&self.bus, &self.addresses);
        let seats: Vec<(Actor, &Peer)> = self
            .peers
            .iter()
            .filter(|(address, peer)| {
                peer.verified
                    && (!established_only || peer.established())
                    && !speaks_bus(bus, addresses, address, peer)
            })
            .filter_map(|(_, peer)| peer.seat().map(|seat| (seat, peer)))
            .collect();
        self.hot.retain(|subject, hot| {
            seats
                .iter()
                .any(|(seat, peer)| !hot.tried.contains(seat) && may_take(peer, subject))
        });
    }
    /// The endpoints a walk may dial ([`Self::moved`]), each with when it may: established,
    /// idle, one this side may dial, with no unfinished round standing ([`Peer::unfinished`]),
    /// of a seat not tried for something that moved and may take ([`may_take`]) and with no
    /// round in flight at another of its endpoints (a second round with one seat would only
    /// contend with the first), from [`WALK_GAP`] after its last completed round. Its own wait
    /// is not read.
    fn walkable(&self) -> impl Iterator<Item = (SocketAddr, Instant)> + '_ {
        #[cfg(test)]
        let off = mutant(&NO_WALK);
        #[cfg(not(test))]
        let off = false;
        let flying: BTreeSet<Actor> = if self.hot.is_empty() || off {
            BTreeSet::new()
        } else {
            self.peers
                .values()
                .filter(|peer| peer.busy)
                .filter_map(|peer| peer.dialled)
                .collect()
        };
        #[cfg(test)]
        let flying = if mutant(&TWIN_WALKED) {
            BTreeSet::new()
        } else {
            flying
        };
        #[cfg(test)]
        let gap = if mutant(&WALK_AT_ONCE) {
            Duration::ZERO
        } else {
            WALK_GAP
        };
        #[cfg(not(test))]
        let gap = WALK_GAP;
        self.peers
            .iter()
            .filter(move |(address, peer)| {
                !self.hot.is_empty()
                    && !off
                    && !peer.busy
                    && peer.established()
                    && peer.unfinished == 0
                    && !speaks_bus(&self.bus, &self.addresses, address, peer)
                    && self
                        .addresses
                        .get(&address.ip())
                        .copied()
                        .or_else(|| peer.repair.actor())
                        .is_none_or(|actor| !self.needs_update.contains_key(&actor))
                    && peer.repair.actor().is_some_and(|seat| {
                        !flying.contains(&seat)
                            && self.hot.iter().any(|(subject, hot)| {
                                !hot.tried.contains(&seat) && may_take(peer, subject)
                            })
                    })
            })
            .map(move |(address, peer)| (*address, peer.seen + gap))
    }
    /// The walk's next endpoint at `now` ([`Self::walkable`]): the first in this node's order
    /// ([`Self::spread`]) whose gap after its last round is over.
    fn walk(&self, now: Instant) -> Option<SocketAddr> {
        self.walkable()
            .filter(|(_, from)| *from <= now)
            .min_by_key(|(address, _)| (self.spread(address), *address))
            .map(|(address, _)| address)
    }
    /// How many subjects that moved here a walk still offers (tests).
    #[cfg(test)]
    pub(super) fn hot_count(&self) -> usize {
        self.hot.len()
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
    /// `Observe`) with the lead its sender and address give it. The endpoint it names, if one
    /// is held, notes when it was heard, which only times that endpoint's probe
    /// ([`Self::probe_at`]): a beacon dials nothing, puts off no endpoint's own wait, and sets
    /// no `seen`, `verified` or contact.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDialCadence",
            action = "Beacon",
            project = "aterm_db::runtime::discovery::cadence_tests::dial_cadence_transition"
        )
    )]
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
        if let Some(peer) = self.peers.get_mut(&address) {
            peer.beacon = Some(now);
            peer.probed = false;
            #[cfg(test)]
            if mutant(&BEACON_DEFERS_AUDIT) {
                peer.next = peer.next.max(now + SILENCE);
            }
        }
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
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDialCadence",
            action = "Beacon",
            project = "aterm_db::runtime::discovery::cadence_tests::dial_cadence_transition"
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
    /// Whether the endpoint at `address` rests until its audit ([`Peer::resting`]) (tests).
    #[cfg(test)]
    pub(super) fn rests(&self, address: SocketAddr) -> bool {
        self.peers.get(&address).is_some_and(Peer::resting)
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
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDialCadence",
            action = "Dial",
            project = "aterm_db::runtime::discovery::cadence_tests::dial_cadence_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDialCadence",
            action = "WalkSends",
            project = "aterm_db::runtime::discovery::cadence_tests::dial_cadence_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDialCadence",
            action = "WalkCold",
            project = "aterm_db::runtime::discovery::cadence_tests::dial_cadence_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDialCadence",
            action = "WalkLacks",
            project = "aterm_db::runtime::discovery::cadence_tests::dial_cadence_transition"
        )
    )]
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
        self.cool();
        // A `Bus` speaker is never dialled; it stays known ([`Self::bus_speakers`]).
        let dialable = |a: &SocketAddr, p: &Peer| !speaks_bus(&self.bus, &self.addresses, a, p);
        // An endpoint is due when its own wait ends, or sooner when its silence calls for its
        // probe ([`Self::due`]).
        let eligible = |a: &SocketAddr, p: &Peer| {
            !p.busy
                && self.due(a, p) <= now
                && (allow_discovery || p.established())
                && dialable(a, p)
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
        // An endpoint whose audit is due ([`Peer::audit_due`]) goes before the ordinary scan,
        // the longest unverified first, and leaves the scan's place where it was, so it waits
        // for a place and never for the scan to come round: its audit is what verifies it again
        // within [`REVERIFY`].
        #[cfg(test)]
        let unpicked = mutant(&AUDIT_UNPICKED);
        #[cfg(not(test))]
        let unpicked = false;
        let reverify = if follow_up.is_none() && led.is_none() && !unpicked {
            self.peers
                .iter()
                .filter(|(a, p)| Self::audit_due(p, now) && !p.auditing && eligible(a, p))
                .min_by_key(|(a, p)| (p.seen, **a))
                .map(|(a, _)| *a)
        } else {
            None
        };
        // The ordinary scan goes round every endpoint in this node's order ([`Self::spread`]),
        // from the one after where it last stopped.
        let due = follow_up.or(led).or(reverify).or_else(|| {
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
        });
        // With no endpoint due, what moved here is offered to the next seat not yet tried for it
        // ([`Self::walk`]), whose own wait is not over: after the follow-ups, the leads and
        // every endpoint that is due, so a run of moves cannot keep those waiting.
        let walked = due.is_none();
        let address = due.or_else(|| self.walk(now))?;
        // Whichever turn chose it, a dial of an endpoint whose silence called for a probe is
        // that probe ([`Self::probe_at`]).
        let probing = self
            .probe_at(&address, &self.peers[&address])
            .is_some_and(|probe| probe <= now);
        let catalog = self.catalog_due(now);
        let slept = self.slept;
        let peer = self.peers.get_mut(&address).unwrap();
        // Whichever turn chose it, the dial of a verified endpoint whose audit is due is that
        // audit, asked once ([`Peer::auditing`]). The dial of an endpoint not yet verified (a
        // seat first met, or met again at a new port or after its endpoint expired) or of one
        // last reached [`ABSENT`] ago by the wall clock sweeps the tables both list again
        // ([`Repair::restart_cycle`]): the seat's hints this side's endpoint kept say the scan
        // went round before the absence, not since.
        #[cfg(test)]
        let (reasks, unasked) = (mutant(&AUDIT_REASKS), mutant(&AUDIT_UNASKED));
        #[cfg(not(test))]
        let (reasks, unasked) = (false, false);
        let audits =
            !unasked && peer.verified && Self::audit_due(peer, now) && (!peer.auditing || reasks);
        #[cfg(test)]
        let keeps_cycle = mutant(&ABSENCE_KEEPS_CYCLE);
        #[cfg(not(test))]
        let keeps_cycle = false;
        let absent = !keeps_cycle
            && (!peer.verified
                || now.saturating_duration_since(peer.seen) + slept.saturating_sub(peer.slept)
                    > ABSENT);
        peer.auditing |= audits;
        peer.busy = true;
        peer.began = now;
        peer.probed |= probing;
        // Change jitter on every attempt: fixed per-owner jitter can keep two
        // peers with nearby deadlines colliding at the runtime's tick cadence.
        self.attempt = self.attempt.wrapping_add(1);
        peer.next = now + retry_delay(&self.own, self.attempt);
        if follow_up.is_some() {
            self.follow_up_left -= 1;
        } else if led.is_none() && reverify.is_none() && !walked {
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
        let mut repair = std::mem::take(&mut peer.repair);
        if probing || audits {
            repair.audit();
        }
        if absent {
            repair.restart_cycle();
            repair.ask();
        }
        peer.dialled = repair.actor();
        // Whichever turn chose it, a dial to a seat not yet tried for a table that moved here
        // leads with that table, one table a dial, and never with one the seat's last listing
        // left out ([`may_take`]): the request is asked of the state handed out, where the
        // round's lease keeps it (`Repair::lead`). A request past that state's bound is not
        // kept; the round still settles its seat for the table, which the peer's own scan then
        // reaches.
        peer.led = peer.dialled.filter(|_| peer.verified).and_then(|seat| {
            self.hot.iter().find_map(|(subject, hot)| match subject {
                Subject::Table(table) if !hot.tried.contains(&seat) && may_take(peer, subject) => {
                    Some(*table)
                }
                _ => None,
            })
        });
        if let Some(table) = peer.led {
            repair.lead(table);
            #[cfg(test)]
            let asks = !mutant(&WALK_UNASKED);
            #[cfg(not(test))]
            let asks = true;
            if asks && now.saturating_duration_since(peer.seen) >= ASK_GAP {
                repair.ask();
            }
        }
        // An audit leads the lab's catalog once no round over it has left nothing to bring for
        // too long ([`Self::catalog_due`]): the audit this dial is (a probe), or one asked of
        // the endpoint before it ([`Self::unsettle`]). A dial handed a moved table keeps to
        // that one table, and the catalog rides the audit after it.
        peer.catalog = peer.led.is_none()
            && repair.audited()
            && catalog.is_some_and(|table| repair.lead(table));
        Some((address, repair))
    }
    /// When [`Self::next_allowed`] may next pick an endpoint, by the clock alone, as `now` reads
    /// it: at or before `now` where it may pick one now, else the earliest time an endpoint it
    /// would pick is due ([`Self::due`]: its own wait's end, or its probe) or a walk may dial
    /// one ([`Self::walkable`]), the follow-up queue's first where its hold keeps the rest back;
    /// `None` where
    /// it holds none it would ever pick by waiting (each it holds is in a round, a `Bus`
    /// speaker, held for a build, or established-only turns exclude it). It reads what
    /// [`Self::next_allowed`] reads and changes nothing; an endpoint whose expiry that call
    /// would take first, or a moved subject it would drop first ([`Self::cool`]), can make it
    /// early, never late.
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
        #[cfg(test)]
        let untimed = mutant(&PROBE_UNTIMED);
        #[cfg(not(test))]
        let untimed = false;
        let due = |address: &SocketAddr, peer: &Peer| {
            if untimed {
                peer.next
            } else {
                self.due(address, peer)
            }
        };
        if !self.hurrying && self.follow_up_left > 0 {
            let queued = self.follow_up.iter().filter_map(|address| {
                self.peers
                    .get(address)
                    .filter(|peer| candidate(address, peer))
                    .map(|peer| due(address, peer))
            });
            if let Some(first) = queued.clone().min()
                && queued.clone().all(|next| next > now)
            {
                return Some(first);
            }
        }
        let waits = self
            .peers
            .iter()
            .filter(|(address, peer)| candidate(address, peer))
            .map(|(address, peer)| due(address, peer))
            .min();
        #[cfg(test)]
        if mutant(&WALK_UNTIMED) {
            return waits;
        }
        // A walk's dial waits for no endpoint's wait, only its gap ([`Self::walkable`]).
        if let Some(walk) = self.walkable().map(|(_, from)| from).min() {
            return Some(waits.map_or(walk, |at| at.min(walk)));
        }
        waits
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
    /// The push ended: each peer's rounds keep their ordinary spacing again.
    pub(super) fn calm(&mut self) {
        self.hurrying = false;
        for peer in self.peers.values_mut() {
            peer.hurried = false;
            peer.quiet_push = false;
        }
    }
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbDialCadence",
            action = "Finish",
            project = "aterm_db::runtime::discovery::cadence_tests::dial_cadence_transition"
        )
    )]
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
            Outcome::Repaired { .. } | Outcome::Deferred | Outcome::Unfinished => 0,
            Outcome::Refused | Outcome::Unreached | Outcome::Unasked | Outcome::Busy => {
                peer.failures.saturating_add(1)
            }
        };
        match outcome {
            Outcome::Repaired { .. } | Outcome::Deferred => peer.unfinished = 0,
            Outcome::Unfinished => peer.unfinished = peer.unfinished.saturating_add(1),
            Outcome::Refused | Outcome::Unreached | Outcome::Unasked | Outcome::Busy => {}
        }
        // A completed round this side dialled counts toward its endpoint's audit ([`Episode`]);
        // the one that completes it sets when the next is due, on this side's own clock. One
        // whose table exchange failed completes none, though the ask it had answered counts.
        match outcome {
            Outcome::Repaired { .. } | Outcome::Deferred => {
                #[cfg(test)]
                let any = mutant(&ANY_ROUND_AUDITS);
                #[cfg(not(test))]
                let any = false;
                if peer.episode.completes(repair.application().as_ref()) || any {
                    peer.episode = Episode::default();
                    peer.auditing = false;
                    peer.audit_due = Some(now + audit_rest(&self.own, self.attempt));
                }
            }
            Outcome::Unfinished => peer
                .episode
                .interrupted(repair.application_interrupted().as_ref()),
            Outcome::Refused | Outcome::Unreached | Outcome::Unasked | Outcome::Busy => {}
        }
        // A round that moved nothing rests its endpoint until its audit's clock says, unless it
        // left the pair something to do: a table its hint still names is taken at the follow-up
        // interval, and a scan still short of its turn at each table both list (its first sweep,
        // or one after an absence) goes on at the ordinary interval. The audit's clock moves only
        // as an audit completes ([`Episode`]): where it is not yet due the endpoint rests until
        // it is, however many rounds a walk or a probe made since, and where it is due and not
        // yet complete the endpoint goes on at the ordinary interval until it is.
        #[cfg(test)]
        let (redials, rests_pending) = (mutant(&QUIET_REDIALS), mutant(&RESTS_PENDING));
        #[cfg(not(test))]
        let (redials, rests_pending) = (false, false);
        let owes = repair.pending() && !rests_pending;
        let sweeping = repair.application().is_some() && !repair.cycled() && !rests_pending;
        let rest = peer
            .audit_due
            .filter(|due| *due > now && !owes && !sweeping && !redials)
            .filter(|_| outcome == (Outcome::Repaired { transferred: false }));
        let delay = match outcome {
            Outcome::Repaired { transferred: true } => follow_up_delay(&self.own, self.attempt),
            Outcome::Repaired { transferred: false } if owes => {
                follow_up_delay(&self.own, self.attempt)
            }
            Outcome::Repaired { transferred: false } => rest.map_or_else(
                || retry_delay(&self.own, self.attempt),
                |due| due.saturating_duration_since(now),
            ),
            Outcome::Deferred if !peer.deferring => follow_up_delay(&self.own, self.attempt),
            Outcome::Deferred => retry_delay(&self.own, self.attempt),
            Outcome::Busy if peer.failures <= QUICK_BUSY_RETRIES => {
                retry_delay(&self.own, self.attempt)
            }
            Outcome::Busy => {
                backoff_delay(&self.own, self.attempt, peer.failures - QUICK_BUSY_RETRIES)
            }
            Outcome::Unfinished if peer.unfinished <= QUICK_BUSY_RETRIES => {
                retry_delay(&self.own, self.attempt)
            }
            Outcome::Unfinished => backoff_delay(
                &self.own,
                self.attempt,
                peer.unfinished - QUICK_BUSY_RETRIES,
            ),
            Outcome::Refused | Outcome::Unreached | Outcome::Unasked => {
                backoff_delay(&self.own, self.attempt, peer.failures)
            }
        };
        // What this dial says of its seat's presence ([`Standing`]).
        let gone = if repair.authenticated_actor().is_some() {
            peer.standing = Standing::There;
            false
        } else {
            match (peer.standing, outcome) {
                (Standing::Doubted, Outcome::Busy) if repair.unanswered() => {
                    // Something of this lab took the hello at the endpoint: the two failures
                    // around it are not two in a row.
                    peer.standing = Standing::There;
                    false
                }
                (Standing::There | Standing::Doubted, Outcome::Unreached) => true,
                (Standing::There | Standing::Doubted, Outcome::Refused) if repair.disowned() => {
                    true
                }
                (Standing::Doubted, Outcome::Refused) => true,
                (Standing::There, Outcome::Refused) => {
                    peer.standing = Standing::Doubted;
                    false
                }
                _ => false,
            }
        };
        if gone {
            peer.standing = Standing::Untold;
        }
        #[cfg(test)]
        let gone = gone
            || (mutant(&ANY_FAILURE_DEPARTS)
                && peer.verified
                && !matches!(
                    outcome,
                    Outcome::Repaired { .. } | Outcome::Deferred | Outcome::Unfinished
                ));
        let departed = peer.dialled.filter(|_| gone).map(|seat| (seat, peer.began));
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
        // A deferred round, and one whose later exchange failed, authenticated its peer and
        // completed its control exchange: the endpoint is established, though the round
        // repaired nothing.
        let completed = success || matches!(outcome, Outcome::Deferred | Outcome::Unfinished);
        peer.deferring = outcome == Outcome::Deferred;
        if completed {
            peer.pulled = false;
        }
        let unsettled = std::mem::take(&mut peer.unsettled) && completed;
        if unsettled {
            // What its rest stood on moved while this round ran ([`Self::unsettle`]).
            peer.next = peer.next.min(now + UNSETTLED);
        }
        let verified = repair.actor().filter(|_| completed);
        if verified.is_some() {
            peer.verified = true;
            peer.seen = now;
            peer.slept = self.slept;
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
        peer.dialled = None;
        peer.led = None;
        peer.catalog = false;
        peer.failed = !completed;
        peer.remember_lists(&repair);
        peer.repair = repair;
        if unsettled {
            peer.repair.audit();
        }
        if let Some((seat, began)) = departed {
            self.depart(address, seat, began);
        }
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

#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbDialCadence",
        action = "Tick",
        reason = "The clock: one unit passes while the endpoint waits; a due endpoint is dialled before it passes, the caps' place wait being bounded by a_full_macs_endpoint_is_verified_again_before_it_expires_whoever_dials_in"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbDialCadence",
        action = "Silence",
        reason = "The peer: a beacon goes unheard for SILENCE, under one unit"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbDialCadence",
        action = "Owes",
        reason = "The peer's answer: the round leaves its hint a table still to take (Repair::pending)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbDialCadence",
        action = "Sweeps",
        reason = "The peer's answer: the scan has not yet taken its turn at every table both list, or the audit is not yet complete (Repair::cycled, Episode)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbDialCadence",
        action = "Moves",
        reason = "The peer's answer: the round moves records"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbDialCadence",
        action = "Fails",
        reason = "The peer's answer: the dial fails before the round completes"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbDialCadence",
        action = "Breaks",
        reason = "The peer's answer: the round is established but its membership or application exchange breaks off (Outcome::Unfinished)"
    )
)]
#[doc(hidden)]
pub fn db_dial_cadence_environment() {}

#[cfg(test)]
mod admission_tests;
#[cfg(test)]
mod cadence_tests;
#[cfg(test)]
mod lab_tests;

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
