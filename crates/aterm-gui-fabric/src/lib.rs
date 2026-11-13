// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session fabric endpoint ([`SessionFabric`]): bounded [`Inbox`] ring, outbound post queue,
//! [`Holds`] gate, the verb handlers (`inbox`, `post`, `topic`, `await inbox`, `hold`, and the
//! bridge's `deliver`, `outbox`, `link`, `hello`), and instance `LINK` state.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock, Weak};

use astream_wire::logid::LogIdentity;
use aterm_control::wire::pct_encode;
use aterm_session::SessionId;
pub use aterm_types::control_verbs::Requester;
use aterm_types::control_verbs::{POST_OPTIONS_END, is_post_option, post_options};
use aterm_types::fabric_grammar::{
    DeadReason, KINDS, MirrorState, VERDICTS, is_principal, is_topic, parse_bus_offset, via_ok,
};

use aterm_gui_timing::now_ms;

pub mod fleet_mail;
pub mod handler_tally;
mod hold_clock;
pub mod report;

pub use hold_clock::HoldObservation;

/// What the fabric needs of a session.
pub trait FabricSession: Send + Sync {
    fn self_id(&self) -> &SessionId;
    fn fabric(&self) -> &SessionFabric;
    /// Record one `kind` row on the session's timeline, its typed [`FabricFact`] for the session
    /// log's mark; returns whether the log's readers were owed a wake.
    fn record_fact(&self, kind: &'static str, fact: FabricFact<'_>) -> bool;
    /// Tell the presence band this session's mail or hold moved.
    fn fabric_changed(&self);
    /// Tell whoever judges this session by its hold that the hold moved.
    fn hold_moved(&self) {}
    fn holder_token(&self) -> Option<String>;
    /// Stable private storage for this session's direct fleet mail; absent in inert contexts.
    fn fleet_mail_path(&self) -> Option<std::path::PathBuf> {
        None
    }
    /// Whether this process has committed ownership of its session's direct mailbox.
    /// An overlap-handoff candidate must remain inactive until Commit. Implementations that
    /// own direct storage must opt in; an endpoint alone grants no mailbox ownership.
    fn fleet_mail_ready(&self) -> bool {
        false
    }
    /// How `node`, another Mac, takes this session's mail with the mesh on (§11 C6). A context
    /// that cannot say answers [`fleet_mail::PeerMail::Mesh`], the bus route C3 built.
    fn peer_mail(&self, _node: &str) -> fleet_mail::PeerMail {
        fleet_mail::PeerMail::Mesh
    }
    /// Send `request`, one direct-mail `post` line (no newline) for a session on `node`, to that
    /// Mac over a fleet drive, and return the one line it answered (§11 C6).
    /// # Errors
    /// No drive reached `node`, or it hung up before it answered. A context with no fleet drive
    /// reaches none.
    fn fleet_drive(&self, _node: &str, _request: &str) -> Result<String, String> {
        Err("this session reaches no fleet drive".to_owned())
    }
}

impl<T: FabricSession + ?Sized> FabricSession for Arc<T> {
    fn self_id(&self) -> &SessionId {
        (**self).self_id()
    }
    fn fabric(&self) -> &SessionFabric {
        (**self).fabric()
    }
    fn record_fact(&self, kind: &'static str, fact: FabricFact<'_>) -> bool {
        (**self).record_fact(kind, fact)
    }
    fn fabric_changed(&self) {
        (**self).fabric_changed();
    }
    fn hold_moved(&self) {
        (**self).hold_moved();
    }
    fn holder_token(&self) -> Option<String> {
        (**self).holder_token()
    }
    fn fleet_mail_path(&self) -> Option<std::path::PathBuf> {
        (**self).fleet_mail_path()
    }
    fn fleet_mail_ready(&self) -> bool {
        (**self).fleet_mail_ready()
    }
    fn peer_mail(&self, node: &str) -> fleet_mail::PeerMail {
        (**self).peer_mail(node)
    }
    fn fleet_drive(&self, node: &str, request: &str) -> Result<String, String> {
        (**self).fleet_drive(node, request)
    }
}

/// A session's timeline, as the fabric writes it.
pub trait Timeline {
    /// One `kind` row, its typed [`FabricFact`] (see [`FabricSession::record_fact`]).
    fn record_fact(&mut self, kind: &'static str, fact: FabricFact<'_>) -> bool;
    /// Whether this session's log is the one a seamless update's predecessor wrote, carried and
    /// read (not one that started over after a gap), so every [`FabricFact::MailIn`] it recorded is
    /// already in it ([`FabricCarry::marked`]).
    fn log_carried(&self) -> bool {
        false
    }
}

/// What a fabric record says, typed, for the session log's mark beside it
/// ([`FabricSession::record_fact`]), so nothing parses a timeline payload back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FabricFact<'a> {
    /// A newly persisted direct arrival. `id` is in the independent `fN` namespace.
    FleetMailIn {
        id: u64,
        kind: &'a str,
        from: &'a str,
    },
    /// A direct arrival was answered or explicitly retired by its recipient.
    FleetMailSeen { id: u64 },
    /// A post carried by direct fleet mail to a session on a pre-R3 Mac (§11 C6): `id` is in
    /// that Mac's `fN` namespace, `idem` the key its reply names.
    FleetMailOut {
        id: u64,
        kind: &'a str,
        to: &'a str,
        idem: &'a str,
    },
    /// A row delivered to this session's inbox.
    MailIn {
        kind: &'a str,
        from: &'a str,
        off: u64,
        inbox: u64,
    },
    /// `inbox seen <id>`, every one. `off` is none for a direct fleet message, which names no
    /// bus record.
    MailSeen {
        inbox: u64,
        off: Option<u64>,
        floor: Option<u64>,
        seen: u64,
        verdict: Option<&'a str>,
        kind: &'a str,
        from: &'a str,
        moved: bool,
    },
    /// A post queued for the bridge.
    MailOut {
        kind: &'a str,
        to: &'a str,
        post: u64,
        re: Option<u64>,
        dl: Option<u64>,
        via: Option<&'a str>,
        idem: Option<&'a str>,
    },
    /// An outbound post retired: landed at `off`, or undelivered (`off: None`) for `why`.
    MailLanded {
        post: u64,
        off: Option<u64>,
        why: Option<DeadReason>,
    },
    /// The session's hold moved.
    Hold {
        held: bool,
        bridge: bool,
        origin: &'a str,
        hold_origin: &'a str,
        reason: Option<&'a str>,
        carried: bool,
    },
    /// A broadcast opt-in moved (`topic add <t> since=<s>`, `topic drop <t>`).
    Topic {
        add: bool,
        name: &'a str,
        since: Option<&'a str>,
    },
    /// A lost bridge's advisory on a session it governed ([`bridge_lost`]'s default).
    Advisory {
        origin: &'a str,
        reason: &'a str,
        generation: u64,
    },
    /// A parked `inbox get @<off>` asked the bridge for the record at `off` (the `fetch` row the
    /// bridge drains).
    Fetch { off: u64 },
    /// The broker's retention gave up this node's unread mail at offsets `from..to`, `records` of
    /// it where the broker counted ([`GivenUp`], `link gap`), or (`link gap … sid=<sid>`) this
    /// session's own lane's records there before it handled them.
    MailGap {
        from: u64,
        to: u64,
        records: Option<u64>,
    },
    /// The broker's retention gave up the broadcast topic `topic`'s records at offsets `from..to`
    /// before the bridge replayed them to this session (`link gap … sid=<sid> topic=<t>`).
    TopicGap { from: u64, to: u64, topic: &'a str },
}

/// What the two bridge verbs need of the registry.
pub trait Registry: Send + Sync + 'static {
    fn sessions(&self) -> Vec<Arc<dyn FabricSession>>;
    fn session(&self, sid: &SessionId) -> Option<Arc<dyn FabricSession>>;
    /// The roster's generation: it moves on every change a bridge's roster read could see, a
    /// session's arrival, its exit and a replace that changes its row (`outbox`'s `roster=`).
    fn roster_seq(&self) -> u64;
    fn bridge_lost_advised(&self, sessions: usize);
    fn torn_state_set_aside(&self, files: u64, told_when_empty: bool);
    fn torn_state_held(&self, files: u64);
}

/// The registry as the fabric holds it.
pub type Store = Arc<RwLock<dyn Registry>>;

/// A registry handle the bridge verbs take.
pub trait AsStore {
    fn as_store(&self) -> Store;
}

impl<R: Registry> AsStore for Arc<RwLock<R>> {
    fn as_store(&self) -> Store {
        self.clone()
    }
}

impl AsStore for Store {
    fn as_store(&self) -> Store {
        self.clone()
    }
}

/// How many message rows one session's inbox retains.
pub const RING_CAP: usize = 512;

/// Unlisted rows one attested peer may hold in a session's ring.
pub const SENDER_QUOTA: usize = 64;

const TEXT_PREVIEW_MAX: usize = 512;

/// The largest body `deliver`/`post` accept, and the largest `inbox get` returns.
pub const BODY_MAX: usize = 256 * 1024;

/// The largest inline `post` text (the argument form).
pub const POST_INLINE_MAX: usize = 4 * 1024;

/// How many outbound posts one session may hold waiting for the bridge, and how many bytes of
/// retained body they may hold together.
pub const OUTBOX_CAP: usize = 128;
pub const OUTBOX_BYTES_MAX: usize = 4 * 1024 * 1024;

const OUTBOX_DRAIN_BYTES_MAX: usize = OUTBOX_BYTES_MAX;

const RETIRED_KEEP: usize = 512;

const DEDUP_CAP: usize = 2 * RING_CAP;

const REASON_MAX: usize = 128;

/// The seven session-timeline record kinds the fabric writes that the `events` digest carries,
/// under these same wire names (`EVENT <local> inbox <id> …`); its `hold-advisory` row stays off.
pub const FABRIC_EVENT_KINDS: &[&str] = &[
    "inbox",
    "inbox-seen",
    "post",
    "fetch",
    "post-landed",
    "hold",
    "topic",
];

/// The `events` wire name for a timeline record kind the fabric wrote, or `None`.
#[inline]
pub fn fabric_event_wire_kind(kind: &str) -> Option<&'static str> {
    FABRIC_EVENT_KINDS.iter().copied().find(|k| *k == kind)
}

/// Return `true` iff a decoded message body length is within [`BODY_MAX`] (`256 KiB`).
#[must_use]
#[inline]
pub const fn is_within_body_max(len: usize) -> bool {
    len <= BODY_MAX
}

/// Return `true` iff an inline `post` text length is within [`POST_INLINE_MAX`] (`4 KiB`).
#[must_use]
#[inline]
pub const fn is_within_post_inline_max(len: usize) -> bool {
    len <= POST_INLINE_MAX
}

/// Return `true` iff a peer with `unlisted_from_peer` unlisted inbox rows has room under
/// [`SENDER_QUOTA`].
#[must_use]
#[inline]
pub const fn sender_within_quota(unlisted_from_peer: usize) -> bool {
    unlisted_from_peer < SENDER_QUOTA
}

/// Return `true` iff adding `add_bytes` to an outbox with `(queued_posts, queued_bytes)` stays
/// within [`OUTBOX_CAP`], [`BODY_MAX`], and [`OUTBOX_BYTES_MAX`].
#[must_use]
#[inline]
pub const fn outbox_has_room(queued_posts: usize, queued_bytes: usize, add_bytes: usize) -> bool {
    queued_posts < OUTBOX_CAP
        && add_bytes <= BODY_MAX
        && queued_bytes.saturating_add(add_bytes) <= OUTBOX_BYTES_MAX
}

/// Encode `(seen_id, listed_id)` inbox watermarks into 16 little-endian bytes.
#[must_use]
#[inline]
pub const fn encode_fabric_watermarks_le(seen_id: u64, listed_id: u64) -> [u8; 16] {
    [
        seen_id as u8,
        (seen_id >> 8) as u8,
        (seen_id >> 16) as u8,
        (seen_id >> 24) as u8,
        (seen_id >> 32) as u8,
        (seen_id >> 40) as u8,
        (seen_id >> 48) as u8,
        (seen_id >> 56) as u8,
        listed_id as u8,
        (listed_id >> 8) as u8,
        (listed_id >> 16) as u8,
        (listed_id >> 24) as u8,
        (listed_id >> 32) as u8,
        (listed_id >> 40) as u8,
        (listed_id >> 48) as u8,
        (listed_id >> 56) as u8,
    ]
}

/// Decode 16 little-endian bytes into `(seen_id, listed_id)` inbox watermarks.
#[must_use]
#[inline]
pub const fn decode_fabric_watermarks_le(bytes: [u8; 16]) -> (u64, u64) {
    let seen_id = (bytes[0] as u64)
        | ((bytes[1] as u64) << 8)
        | ((bytes[2] as u64) << 16)
        | ((bytes[3] as u64) << 24)
        | ((bytes[4] as u64) << 32)
        | ((bytes[5] as u64) << 40)
        | ((bytes[6] as u64) << 48)
        | ((bytes[7] as u64) << 56);
    let listed_id = (bytes[8] as u64)
        | ((bytes[9] as u64) << 8)
        | ((bytes[10] as u64) << 16)
        | ((bytes[11] as u64) << 24)
        | ((bytes[12] as u64) << 32)
        | ((bytes[13] as u64) << 40)
        | ((bytes[14] as u64) << 48)
        | ((bytes[15] as u64) << 56);
    (seen_id, listed_id)
}

const POSTABLE: &[&str] = &["ask", "answer", "task", "report", "note", "ack", "control"];

const TRUSTS: &[&str] = &["human", "agent", "relayed", "screen"];

const AWAIT_DEFAULT_SKIP: &str = "note";

const FETCH_SLOTS: usize = 8;
const FETCH_WAIT_MS: u64 = 10_000;

const RECEIPTS_OWED_MAX: usize = RING_CAP;

const EXPIRY_GRACE_MS: u64 = 5_000;

#[cfg(any(test, debug_assertions))]
thread_local! {
    static TEST_EXPIRY_GRACE_MS: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

fn expiry_grace_ms() -> u64 {
    #[cfg(any(test, debug_assertions))]
    if let Some(ms) = TEST_EXPIRY_GRACE_MS.with(std::cell::Cell::get) {
        return ms;
    }
    EXPIRY_GRACE_MS
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// A test thread's own fetch bound in place of [`FETCH_WAIT_MS`] (None: the product's).
    static TEST_FETCH_WAIT: std::cell::Cell<Option<std::time::Duration>> =
        const { std::cell::Cell::new(None) };
}

fn fetch_wait() -> std::time::Duration {
    #[cfg(any(test, debug_assertions))]
    if let Some(wait) = TEST_FETCH_WAIT.with(std::cell::Cell::get) {
        return wait;
    }
    std::time::Duration::from_millis(FETCH_WAIT_MS)
}

/// One delivered message.
#[derive(Clone, Debug)]
pub struct InboxRow {
    /// Per-session monotone row id (1-based).
    pub id: u64,
    /// The broker offset this record landed at.
    pub off: u64,
    /// Milliseconds on the process clock ([`now_ms`]) at delivery.
    pub t_ms: u64,
    /// The sender, as the delivering bridge renders it.
    pub from: String,
    /// One of [`KINDS`].
    pub kind: String,
    /// `human`, `agent`, `relayed` or `screen`: the receiver's verdict on what the content is.
    pub trust: String,
    /// The offset this message answers, when it answers one.
    pub re: Option<u64>,
    /// The local post id `re` resolves to through the post table, when the answer is to something
    /// this session sent.
    pub re_id: Option<u64>,
    /// Advisory deadline in ms.
    pub dl: Option<u64>,
    /// The answer arrived after the deadline had already been recorded expired.
    pub late: bool,
    /// On a `kind=ack` row: the recipient's verdict on the `ask`/`task` at `re=`.
    pub verdict: Option<String>,
    /// This session's verdict on the row, as `inbox seen <id> <verdict>` last recorded it.
    pub decision: Option<String>,
    /// The kind this message claimed before the receiver demoted it (a relayed row is always
    /// `note`).
    pub demoted: Option<String>,
    /// The relay chain, comma-joined, when the message came through one.
    pub via: Option<String>,
    /// Body length in bytes, before truncation.
    pub len: usize,
    /// The body, as delivered (pct-decoded, so already lossy-valid UTF-8).
    pub body: String,
    /// The broadcast topic this row arrived on, when it is a broadcast.
    pub topic: Option<String>,
    /// Whether a non-peek `inbox` has listed this row.
    pub listed: bool,
    /// A broadcast the delivering bridge offers again, while it is unhandled, to a successor that
    /// keeps this sid.
    pub refill: bool,
    /// The direct mailbox id (`f<id>`) of a fleet message the mesh imported into this ring
    /// ([`fleet_mail`], §11 C6), which `post re=f<id>` answers. Such a row names no bus record:
    /// its `off` is 0 and it is never on the log.
    pub fleet: Option<u64>,
    on_log: bool,
}

impl InboxRow {
    #[inline]
    fn is_human(&self) -> bool {
        self.from.starts_with("h-")
    }
}

/// The sender class of a delivered row, read from the prefix the subject's `<src>` segment carries.
#[inline]
pub fn sender_class(from: &str) -> &'static str {
    if from.starts_with("h-") {
        "human"
    } else if from.starts_with("s-") || from.starts_with("fleet:") {
        "agent"
    } else if from.starts_with("a-") {
        "service"
    } else {
        "other"
    }
}

/// Which delivered rows a `subscribe … mail` subscriber asked for.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct MailFilter {
    /// `kinds=<k,..>`: empty means every kind.
    pub kinds: Vec<String>,
    /// `from=<class|principal>`.
    pub from: Option<String>,
    /// `topic=<t>`: only broadcast rows that arrived on that topic.
    pub topic: Option<String>,
}

impl MailFilter {
    fn wants_direct(&self, row: &fleet_mail::Notice) -> bool {
        self.topic.is_none()
            && (self.kinds.is_empty() || self.kinds.contains(&row.kind))
            && self
                .from
                .as_ref()
                .is_none_or(|f| f == &row.from || f == "agent")
    }

    fn wants(&self, row: &InboxRow) -> bool {
        if !self.kinds.is_empty() && !self.kinds.contains(&row.kind) {
            return false;
        }
        if self.topic.is_some() && self.topic != row.topic {
            return false;
        }
        self.from
            .as_ref()
            .is_none_or(|f| f == &row.from || f == sender_class(&row.from))
    }
}

/// One `MAIL` frame's fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailLine {
    pub id: u64,
    pub off: u64,
    pub from: String,
    pub kind: String,
    pub re: Option<u64>,
    /// The broadcast topic, when the row is one.
    pub topic: Option<String>,
}

/// What [`SessionFabric::mail_since`] answers.
pub struct MailSince {
    /// The matching rows newer than the cursor, oldest first.
    pub rows: Vec<MailLine>,
    /// The highest row id ever delivered here, matched or not.
    pub high: u64,
    /// How many rows with id above the cursor the ring still holds.
    pub present: u64,
}

/// One outbound message this session sent.
#[derive(Clone, Debug)]
pub struct PostRow {
    /// Per-session monotone post id (1-based), the `--wait` and `landed=` key.
    pub id: u64,
    /// The address as typed, pct-encoded.
    pub to: String,
    /// One of [`KINDS`].
    pub kind: String,
    /// The offset the record landed at, once the bridge says so.
    pub off: Option<u64>,
    landed_on_log: bool,
    /// The offset this post answers, carried to the bridge so the published record can say `re=`.
    pub re: Option<u64>,
    /// Advisory deadline in ms, carried through the same way.
    pub dl: Option<u64>,
    /// The relay chain, when the sender declared one.
    pub via: Option<String>,
    /// The caller's idempotency key (`post key=<token>`), carried on every `outbox` drain.
    pub key: Option<String>,
    /// The bridge retired this post `dup=1`.
    pub dup: bool,
    /// Retired as permanently undeliverable (`outbox sent … off=-`).
    pub dead: bool,
    /// Why it is dead, from the bridge's `reason=` (default `undeliverable`).
    pub dead_reason: String,
    /// The message body, kept only until the bridge retires the post.
    pub body: String,
}

/// One standing halt on one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hold {
    /// Pct-encoded reason, or `-`.
    pub reason: String,
    /// `person` (Take Over at the window, [`HoldIssuer::Person`]), `remote` (another Mac's hold
    /// over a drive connection, [`HoldIssuer::Remote`]), `fleet` (a human's halt through the
    /// bridge, or a lost bridge) or `local` (the instance Owner's halt over the local socket).
    pub origin: String,
}

/// The origin word of the person's own slot ([`Holds`]).
pub const PERSON_ORIGIN: &str = "person";

/// The reason the person's hold carries.
pub const TAKEN_OVER: &str = "taken-over";

/// The origin word of a remote node's slot ([`Holds`]).
pub const REMOTE_ORIGIN: &str = "remote";

/// A remote node's hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteHold {
    /// Pct-encoded reason, or `-`.
    pub reason: String,
    /// The node that holds it.
    pub node: String,
    /// That node as a person reads it ([`Driver::host`]).
    pub host: String,
}

/// The node of another Mac a drive connection serves for ([`DrivingAs`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Driver {
    /// Its node id, proved by its node key.
    pub node: String,
    /// It as a person reads it.
    pub host: String,
}

thread_local! {
    static DRIVING: std::cell::RefCell<Option<(Driver, Requester)>> =
        const { std::cell::RefCell::new(None) };
}

/// Marks the connection this thread serves as a remote node's drive connection until dropped.
pub struct DrivingAs(Option<(Driver, Requester)>);

impl DrivingAs {
    /// Serve as `driver` for no named requester until the guard drops.
    #[must_use]
    pub fn enter(driver: Driver) -> Self {
        Self::requested(driver, Requester::default())
    }

    /// Serve as `driver` until the guard drops, for `requester`, whom the drive's first line
    /// names: a claim within the node `driver` proved, kept apart from it ([`serving_requester`]).
    #[must_use]
    pub fn requested(driver: Driver, requester: Requester) -> Self {
        Self(DRIVING.with(|driving| driving.replace(Some((driver, requester)))))
    }
}

impl Drop for DrivingAs {
    fn drop(&mut self) {
        let was = self.0.take();
        DRIVING.with(|driving| *driving.borrow_mut() = was);
    }
}

/// The remote node this thread serves a drive connection for, if any.
#[must_use]
pub fn serving_driver() -> Option<Driver> {
    DRIVING.with(|driving| driving.borrow().as_ref().map(|(driver, _)| driver.clone()))
}

/// Who on the driving Mac asked for the drive this thread serves, as its first line claims
/// ([`DrivingAs::requested`]): attribution within [`serving_driver`]'s proven node, never
/// authorization. `None` where this thread serves no drive.
#[must_use]
pub fn serving_requester() -> Option<Requester> {
    DRIVING.with(|driving| {
        driving
            .borrow()
            .as_ref()
            .map(|(_, requester)| requester.clone())
    })
}

/// The standing halts on one session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Holds {
    person: Option<String>,
    fleet: Option<String>,
    local: Option<String>,
    remote: Option<RemoteHold>,
}

impl Holds {
    fn slot_mut(&mut self, origin: &str) -> &mut Option<String> {
        match origin {
            PERSON_ORIGIN => &mut self.person,
            "fleet" => &mut self.fleet,
            _ => &mut self.local,
        }
    }

    /// Whether a hold of `origin` stands.
    pub fn has(&self, origin: &str) -> bool {
        match origin {
            PERSON_ORIGIN => self.person.is_some(),
            REMOTE_ORIGIN => self.remote.is_some(),
            "fleet" => self.fleet.is_some(),
            _ => self.local.is_some(),
        }
    }

    /// Whether any hold stands.
    pub const fn is_held(&self) -> bool {
        self.person.is_some()
            || self.fleet.is_some()
            || self.local.is_some()
            || self.remote.is_some()
    }

    /// The person's hold's reason, when the tab is taken over.
    pub fn person(&self) -> Option<&str> {
        self.person.as_deref()
    }

    /// The remote node's hold, when another Mac holds the tab.
    pub const fn remote(&self) -> Option<&RemoteHold> {
        self.remote.as_ref()
    }

    /// The slot hand back lifts ([`person_hold`]).
    pub const fn hand_back_origin(&self) -> Option<&'static str> {
        if self.person.is_some() {
            Some(PERSON_ORIGIN)
        } else if self.local.is_some() {
            Some("local")
        } else if self.remote.is_some() {
            Some(REMOTE_ORIGIN)
        } else {
            None
        }
    }

    /// Whose hold the session shows (`status hold_by=`, the survey's `hold_by=`, the need
    /// classifier's [`aterm_types::needs::HoldWord`]).
    pub const fn word(&self) -> Option<aterm_types::needs::HoldWord> {
        use aterm_types::needs::HoldWord;
        if self.person.is_some() {
            Some(HoldWord::Person)
        } else if self.remote.is_some() {
            Some(HoldWord::Remote)
        } else if self.local.is_some() {
            Some(HoldWord::Local)
        } else if self.fleet.is_some() {
            Some(HoldWord::Bridge)
        } else {
            None
        }
    }

    /// The hold of `origin` alone, the other slots empty.
    pub fn of_origin(&self, origin: &str) -> Holds {
        match origin {
            PERSON_ORIGIN => Holds {
                person: self.person.clone(),
                ..Holds::default()
            },
            REMOTE_ORIGIN => Holds {
                remote: self.remote.clone(),
                ..Holds::default()
            },
            "fleet" => Holds {
                fleet: self.fleet.clone(),
                ..Holds::default()
            },
            _ => Holds {
                local: self.local.clone(),
                ..Holds::default()
            },
        }
    }

    /// Every standing hold in order: the person's, a remote node's, the fleet's, then the local.
    pub fn standing(&self) -> impl Iterator<Item = Hold> + '_ {
        [
            (PERSON_ORIGIN, self.person.as_ref()),
            (
                REMOTE_ORIGIN,
                self.remote.as_ref().map(|remote| &remote.reason),
            ),
            ("fleet", self.fleet.as_ref()),
            ("local", self.local.as_ref()),
        ]
        .into_iter()
        .filter_map(|(origin, reason)| {
            reason.map(|reason| Hold {
                reason: reason.clone(),
                origin: origin.to_string(),
            })
        })
    }

    /// The hold that refuses a write from the remote node `driver` (`None`: any other writer).
    pub fn first_against(&self, driver: Option<&str>) -> Option<Hold> {
        let own = |hold: &Hold| {
            hold.origin == REMOTE_ORIGIN
                && driver.is_some_and(|node| self.remote.as_ref().is_some_and(|r| r.node == node))
        };
        self.standing().find(|hold| !own(hold))
    }

    /// The hold a one-hold surface names.
    pub fn first(&self) -> Option<Hold> {
        self.standing().next()
    }

    /// The standing origins as `hold_origin=` prints them.
    pub fn origins(&self) -> String {
        let words: Vec<String> = self.standing().map(|h| h.origin).collect();
        if words.is_empty() {
            "-".to_string()
        } else {
            words.join(",")
        }
    }

    fn seed(&mut self, hold: Hold) {
        let slot = self.slot_mut(&hold.origin);
        if slot.is_none() {
            *slot = Some(hold.reason);
        }
    }

    fn event_fields(&self, lifted: &str) -> (String, Option<String>, String) {
        match self.first() {
            Some(first) => (first.origin, Some(first.reason), self.origins()),
            None => (lifted.to_string(), None, self.origins()),
        }
    }
}

/// The `reason=` of the fleet hold a lost bridge leaves under
/// `[fabric] hold_on_bridge_lost = true` ([`bridge_lost`]).
pub const FABRIC_LOST: &str = "fabric-lost";

impl PostRow {
    fn caller_sized_bytes(&self) -> usize {
        caller_sized_bytes(&self.body, self.via.as_deref())
    }
}

fn caller_sized_bytes(body: &str, via: Option<&str>) -> usize {
    body.len() + via.map_or(0, str::len)
}

/// The mutable half of [`SessionFabric`], behind one leaf mutex.
#[derive(Default)]
#[doc(hidden)]
pub struct Inbox {
    /// A body-free copy of the durable direct mailbox, maintained on worker lanes.
    direct_mail: fleet_mail::Metadata,
    /// The mailbox file [`Self::direct_mail`] was taken from, as the transaction that cached it
    /// saw it under the mailbox's lock: `None` before one, or when it found no file.
    #[cfg(unix)]
    direct_stamp: Option<fleet_mail::Stamp>,
    /// The direct messages this process imported into [`Self::rows`] with the mesh on, by their
    /// mailbox identity, so none enters twice, whether or not the ring still holds it.
    #[cfg(unix)]
    fleet_imported: BTreeSet<fleet_mail::Imported>,
    pub rows: VecDeque<InboxRow>,
    pub posts: VecDeque<PostRow>,
    outbox_turn: u64,
    pub last_msg_id: u64,
    next_post_id: u64,
    seen: u64,
    /// Unhandled rows the ring has evicted, ever.
    pub dropped: u64,
    bus_head: u64,
    addressed_head: Option<u64>,
    oldest_on_bus: Option<u64>,
    /// The offsets this session took or refused, oldest first, each with whether the mail at
    /// that offset has a [`FabricFact::MailIn`] in this session's log: that delivery recorded it,
    /// an earlier delivery of the same mail did (a moved copy's offset and the one its `was=`
    /// names, §11 C3b), or the carried log holds it (`carried_marks`); a quota refusal
    /// records none.
    dedup: VecDeque<(u64, bool)>,
    dedup_set: HashSet<u64>,
    /// The marks a seamless update carried ([`FabricCarry::marked`]): offsets the carried log
    /// already marks delivered, which this successor's bridge offers again. Each is taken back into
    /// the ring once, with no second mark.
    carried_marks: HashSet<u64>,
    holds: Holds,
    hold_history: hold_clock::HoldHistory,
    advisory: Option<(Hold, u64)>,
    hold_fenced: bool,
    retirements_fenced: bool,
    fetches: VecDeque<(u64, FetchSlot)>,
    watches: Vec<Weak<OffsetWatch>>,
    /// The bus log this session's offsets name (its clock), as the bridge last told it.
    bus_log: Option<LogIdentity>,
    receipts: VecDeque<OwedReceipt>,
    next_receipt_id: u64,
    outbox_moves: u64,
    msg_id_ceiling: Option<u64>,
    /// The newest give-up of this node's unread mail the bridge told of (`link gap`, no `sid=`),
    /// which the `inbox` header names (`given_up=`).
    given_up: Option<GivenUp>,
    /// The newest give-up of this session's own lane the bridge told of (`link gap … sid=<sid>`
    /// with no `topic=`), which the `inbox` header names apart (`lane_given_up=`): kept and
    /// checked for repeats apart from [`Self::given_up`], so a re-tell of either, after the other
    /// was told, is still a repeat.
    lane_given_up: Option<GivenUp>,
    /// The newest give-up of each broadcast topic this session holds that the bridge told of
    /// (`link gap … topic=<t>`), which `topic ls` names. Not carried by a seamless update: the
    /// log's `mail-gap` mark is the record that lasts.
    topic_given_up: BTreeMap<String, GivenUp>,
    closed: bool,
    /// When the newest row arrived, for the handler tally (a parked `await inbox`'s wake).
    arrived: handler_tally::Stamp,
}

/// Records the broker's retention gave up: offsets `from..to` of the log, and `records` of them
/// where the broker counted (`None` after a broker restart, when its ledger could not say, and
/// always for a range a `Fetch` refusal named). Either unread mail given up before this node's
/// bridge read it (astream's `Gap`, A9) or one session's lane or topic range the bridge's own
/// read met (`link gap … sid=<sid>`, B12).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GivenUp {
    pub from: u64,
    pub to: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub records: Option<u64>,
}

impl GivenUp {
    /// Whether `later` only says again what `self` said: the same range, with the same count or
    /// none (a re-subscription is told the range again, and after a broker restart without its
    /// count).
    fn repeated_by(&self, later: &Self) -> bool {
        self.from == later.from
            && self.to == later.to
            && (later.records.is_none() || later.records == self.records)
    }

    fn carriable(&self) -> bool {
        self.from <= self.to
            && self.to <= CARRY_INT_MAX
            && self.records.is_none_or(|n| n <= CARRY_INT_MAX)
    }
}

#[derive(Debug)]
struct OffsetWatch {
    off: u64,
    on_log: AtomicBool,
}

impl OffsetWatch {
    fn on_log(&self) -> bool {
        self.on_log.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Debug)]
struct OwedReceipt {
    rid: u64,
    off: u64,
    verdict: String,
    kind: String,
    from: String,
    earlier_log: bool,
}

fn owe_receipt(
    inbox: &mut Inbox,
    off: u64,
    verdict: &str,
    kind: &str,
    from: &str,
    earlier_log: bool,
) {
    inbox.outbox_moves += 1;
    inbox.next_receipt_id += 1;
    let rid = inbox.next_receipt_id;
    inbox.receipts.push_back(OwedReceipt {
        rid,
        off,
        verdict: verdict.to_string(),
        kind: kind.to_string(),
        from: from.to_string(),
        earlier_log,
    });
    while inbox.receipts.len() > RECEIPTS_OWED_MAX {
        inbox.receipts.pop_front();
    }
}

/// The mailbox a seamless update carries beside the topics and the hold.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FabricCarry {
    /// The log the carried receipts and reply references name: a [`LogIdentity`] in its
    /// `Display` form, which [`LogIdentity::parse`] reads back (anything else is no clock).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bus_log: Option<String>,
    /// The last inbox row id this session handed out ([`Inbox::last_msg_id`]).
    #[serde(default)]
    pub last_msg_id: u64,
    /// The last post id ([`Inbox::next_post_id`]).
    #[serde(default)]
    pub last_post_id: u64,
    /// The last receipt id ([`Inbox::next_receipt_id`]).
    #[serde(default)]
    pub last_receipt_id: u64,
    /// The posts still waiting for a bridge, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub posts: Vec<PostCarry>,
    /// The receipts still owed, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub receipts: Vec<ReceiptCarry>,
    /// Receipts whose offsets name an earlier log.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub earlier_receipts: Vec<ReceiptCarry>,
    /// Waiting posts this handoff could not carry, by id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncarried: Vec<u64>,
    /// How many owed receipts this handoff could not carry (their asks' senders wait out their own
    /// deadlines).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub receipts_uncarried: u64,
    /// The session's `dropped=` at the draw ([`Inbox::dropped`]).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dropped: u64,
    /// The broadcasts this session held unhandled at the draw (`topic=`, id past `seen`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub broadcasts_uncarried: u64,
    /// Of [`Self::broadcasts_uncarried`], the ones the successor's bridge offers again (not lost).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub broadcasts_refilled: u64,
    /// The offsets, on [`Self::bus_log`], of the mail whose delivery this session's log marked
    /// (`mail dir=in`), a moved copy's offset and its first offset among them, oldest first, as
    /// far as the budget reached. The ring is not carried, so the
    /// successor's bridge offers the unhandled ones again; a successor whose log is the carried one
    /// ([`Timeline::log_carried`]) takes each back into its ring without a second mark, or its fold
    /// would hold the mail twice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marked: Vec<u64>,
    /// The newest give-up of this node's unread mail on [`Self::bus_log`] the bridge told of
    /// ([`GivenUp`], `link gap` with no `sid=`), so the successor's `inbox` still names it; a
    /// successor whose log is not the carried one ([`Timeline::log_carried`]) marks it in its own
    /// log, once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub given_up: Option<GivenUp>,
    /// The newest give-up of this session's own lane on [`Self::bus_log`] the bridge told of
    /// (`link gap … sid=<sid>`: mail the inbox group may have read, given up before this session
    /// handled it), carried apart from [`Self::given_up`] and seeded the same way, so the
    /// successor's bridge telling it again is a repeat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane_given_up: Option<GivenUp>,
}

fn mark_cost(off: u64) -> usize {
    off.checked_ilog10().map_or(1, |d| d as usize + 1) + 2
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// One waiting post on the handoff record.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PostCarry {
    pub id: u64,
    pub to: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub re: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dl: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub body: String,
}

/// One owed receipt on the handoff record.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReceiptCarry {
    pub rid: u64,
    pub off: u64,
    pub verdict: String,
    pub kind: String,
    pub from: String,
}

/// The most mailbox bytes one handoff manifest carries across every session ([`FabricCarry`]):
/// carried posts and owed receipts, each at what the manifest's TOML spends on it
/// ([`PostCarry::cost`], [`ReceiptCarry::cost`]).
pub const CARRY_MAIL_BYTES: usize = 256 * 1024;

/// What one carried row costs the manifest beyond its escaped strings.
pub const CARRY_ROW_OVERHEAD: usize = 256;

/// What `s` costs the manifest as TOML writes it.
pub fn toml_cost(s: &str) -> usize {
    s.bytes()
        .map(|b| match b {
            b'"' | b'\\' => 2,
            0..=0x1f | 0x7f => 6,
            _ => 1,
        })
        .fold(0, usize::saturating_add)
}

/// The reason a post the handoff could not carry is retired with in the successor
/// ([`FabricCarry::uncarried`]).
pub const NOT_CARRIED: &str = DeadReason::NotCarried.word();

/// The reason a queued reply is retired with when its bus log was replaced by one that does not
/// hold the record its `re=` names (`Inbox::forget_bus_offsets`).
pub const LOG_REPLACED: &str = DeadReason::LogReplaced.word();

const CARRY_INT_MAX: u64 = i64::MAX as u64;

/// How many inbox row ids a seamless update leaves this process between the park's draw and its
/// `_exit` ([`SessionFabric::handoff_carry`]).
pub const INBOX_ID_RESERVE: u64 = 1 << 12;

/// How often a delivery waiting on a committing update (one past [`INBOX_ID_RESERVE`], or a
/// broadcast) logs that it still waits ([`SessionFabric::wait_fenced_delivery`]): a cadence, never
/// a bound.
pub const MSG_ROOM_NOTICE_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

/// The production bound every fenced verb waits for a committing update before it answers
/// [`HOLD_BUSY`] ([`SessionFabric::fence_wait`] reads it through the process's timing value).
pub const HOLD_FENCE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

impl FabricCarry {
    /// Nothing to write: every counter at zero and no row.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// What this carry's rows cost the manifest ([`CARRY_MAIL_BYTES`]'s measure).
    pub fn rows_cost(&self) -> usize {
        self.posts
            .iter()
            .map(PostCarry::cost)
            .chain(
                self.receipts
                    .iter()
                    .chain(&self.earlier_receipts)
                    .map(ReceiptCarry::cost),
            )
            .chain(self.marked.iter().copied().map(mark_cost))
            .fold(0, usize::saturating_add)
    }

    /// Give up the rows, keep the counters. A shed mark costs only a second `mail dir=in` mark for
    /// that mail in the successor's log.
    pub fn shed(&mut self) -> bool {
        if self.posts.is_empty()
            && self.receipts.is_empty()
            && self.earlier_receipts.is_empty()
            && self.marked.is_empty()
        {
            return false;
        }
        self.marked.clear();
        self.uncarried.extend(self.posts.drain(..).map(|p| p.id));
        self.uncarried.sort_unstable();
        self.uncarried.dedup();
        let dropped = u64::try_from(
            self.receipts
                .len()
                .saturating_add(self.earlier_receipts.len()),
        )
        .unwrap_or(u64::MAX);
        self.receipts.clear();
        self.earlier_receipts.clear();
        self.receipts_uncarried = self
            .receipts_uncarried
            .saturating_add(dropped)
            .min(CARRY_INT_MAX);
        true
    }

    /// The counters a PTY keeper recovery starts from.
    #[cfg(unix)]
    pub fn leased([msg, post, rid]: [u64; 3]) -> Self {
        Self {
            last_msg_id: msg.min(CARRY_INT_MAX),
            last_post_id: post.min(CARRY_INT_MAX),
            last_receipt_id: rid.min(CARRY_INT_MAX),
            ..Self::default()
        }
    }

    /// The fence for a kept sid with no lease.
    #[cfg(unix)]
    pub fn fenced() -> Self {
        let hex = aterm_session::LaunchNonce::generate().to_hex();
        let draw = hex
            .get(..5)
            .and_then(|digits| u64::from_str_radix(digits, 16).ok())
            .unwrap_or(0);
        let base = (draw + 1) << 32;
        Self {
            last_msg_id: base,
            last_post_id: base,
            last_receipt_id: base,
            ..Self::default()
        }
    }
}

/// The fabric a PTY keeper recovery carries.
#[cfg(unix)]
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct KeeperCarry {
    /// The first standing hold, as [`render_hold`] writes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold: Option<String>,
    /// Every other standing hold, one row each (the fleet's and the owner's beside the person's,
    /// the owner's beside the fleet's).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub more_holds: Vec<String>,
    /// The id lease: inbox row, post, receipt.
    pub ids: [u64; 3],
    /// The broadcast opt-ins, as [`render_topics`] writes them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<String>,
    /// Opt-ins the meta had no room to name ([`Self::rungs`]).
    #[serde(skip_serializing_if = "is_zero")]
    pub topics_lost: u64,
    /// The posts still waiting for a bridge, by id, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub queued: Vec<u64>,
    /// Waiting posts the meta had no room to name.
    #[serde(skip_serializing_if = "is_zero")]
    pub queued_lost: u64,
    /// The receipts still owed.
    #[serde(skip_serializing_if = "is_zero")]
    pub owed: u64,
    /// The session's `dropped=` at the sample ([`Inbox::dropped`]), which the recovered session
    /// goes on counting from.
    #[serde(skip_serializing_if = "is_zero")]
    pub dropped: u64,
    /// The newest give-up of the node's unread mail the bridge told of ([`GivenUp`]), which the
    /// recovered session's `inbox` goes on naming and its new log marks: the broker tells it again
    /// only while the group's cursor is still below it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub given_up: Option<GivenUp>,
    /// The newest give-up of the session's own lane the bridge told of (`link gap … sid=<sid>`,
    /// [`FabricCarry::lane_given_up`]), named and marked the same way: a bridge that kept running
    /// over the recovery does not tell it again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane_given_up: Option<GivenUp>,
    /// The log the offsets of [`Self::given_up`] and [`Self::lane_given_up`] are on, beside them
    /// and never alone: a recovered session whose bridge reports another log forgets the ranges,
    /// as a moved one does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub given_up_log: Option<String>,
}

#[cfg(unix)]
impl KeeperCarry {
    /// The carry as the meta's cap tries it, most first.
    pub fn rungs(&self) -> [Self; 5] {
        let count = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
        let mut unnamed = self.clone();
        unnamed.queued_lost = unnamed.queued_lost.saturating_add(count(self.queued.len()));
        unnamed.queued.clear();
        unnamed.given_up = None;
        unnamed.lane_given_up = None;
        unnamed.given_up_log = None;
        let mut bare = unnamed.clone();
        bare.topics_lost = bare.topics_lost.saturating_add(count(self.topics.len()));
        bare.topics.clear();
        let shed = |row: &str| format!("{} {REASON_NOT_CARRIED}", parse_hold(row).origin);
        let mut terse = bare.clone();
        terse.more_holds = bare.more_holds.iter().map(|row| shed(row)).collect();
        let mut tersest = terse.clone();
        tersest.hold = terse.hold.as_deref().map(shed);
        [self.clone(), unnamed, bare, terse, tersest]
    }
}

#[cfg(unix)]
fn id_lease(n: u64) -> u64 {
    (n / INBOX_ID_RESERVE)
        .saturating_add(2)
        .saturating_mul(INBOX_ID_RESERVE)
        .min(CARRY_INT_MAX)
}

impl PostCarry {
    /// The bytes this post costs the carry's budget.
    pub fn cost(&self) -> usize {
        [
            Some(self.body.as_str()),
            Some(self.to.as_str()),
            Some(self.kind.as_str()),
            self.via.as_deref(),
            self.key.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(toml_cost)
        .fold(CARRY_ROW_OVERHEAD, usize::saturating_add)
    }

    fn valid(&self, last_post_id: u64) -> bool {
        (1..=last_post_id).contains(&self.id)
            && !self.to.is_empty()
            && !self
                .to
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
            && POSTABLE.contains(&self.kind.as_str())
            && self.via.as_deref().is_none_or(via_ok)
            && self.key.as_deref().is_none_or(valid_key)
            && self.body.len() <= BODY_MAX.saturating_mul(3)
    }
}

impl ReceiptCarry {
    /// The bytes this receipt costs the carry's budget (see [`PostCarry::cost`]).
    pub fn cost(&self) -> usize {
        [&self.verdict, &self.kind, &self.from]
            .into_iter()
            .map(|s| toml_cost(s))
            .fold(CARRY_ROW_OVERHEAD, usize::saturating_add)
    }

    fn valid(&self, last_receipt_id: u64) -> bool {
        (1..=last_receipt_id).contains(&self.rid)
            && VERDICTS.contains(&self.verdict.as_str())
            && matches!(self.kind.as_str(), "ask" | "task")
            && !self.from.is_empty()
            && !self
                .from
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    }
}

#[derive(Clone, Debug)]
enum FetchSlot {
    Pending,
    /// The bridge is answering in chunks (`more=1`) and this much has come.
    Partial(Fetched),
    Done(Fetched),
    /// The bridge's one-token reason.
    Failed(String),
}

#[derive(Clone, Debug)]
struct Fetched {
    off: u64,
    from: String,
    kind: String,
    trust: String,
    re: Option<u64>,
    dl: Option<u64>,
    demoted: Option<String>,
    via: Option<String>,
    verdict: Option<String>,
    len: usize,
    body: String,
}

impl Fetched {
    fn from_row(row: &InboxRow) -> Self {
        Self {
            off: row.off,
            from: row.from.clone(),
            kind: row.kind.clone(),
            trust: row.trust.clone(),
            re: row.re,
            dl: row.dl,
            demoted: row.demoted.clone(),
            via: row.via.clone(),
            verdict: row.verdict.clone(),
            len: row.len,
            body: row.body.clone(),
        }
    }

    fn render(&self) -> String {
        let mut s = format!(
            "OK {} off={} from={} kind={} trust={}",
            self.body.len(),
            self.off,
            self.from,
            self.kind,
            self.trust
        );
        if let Some(re) = self.re {
            s.push_str(&format!(" re={re}"));
        }
        if let Some(dl) = self.dl {
            s.push_str(&format!(" dl={dl}"));
        }
        if let Some(d) = &self.demoted {
            s.push_str(&format!(" demoted={d}"));
        }
        if let Some(v) = &self.via {
            s.push_str(&format!(" via={v}"));
        }
        if let Some(v) = &self.verdict {
            s.push_str(&format!(" verdict={v}"));
        }
        if self.len > self.body.len() {
            s.push_str(&format!(" truncated=1 len={}", self.len));
        }
        s.push('\n');
        s.push_str(&self.body);
        s
    }
}

impl Inbox {
    fn forget_bus_offsets(&mut self, keep_below: Option<u64>) -> Vec<u64> {
        let fork = keep_below.unwrap_or(0);
        let kept = |off: u64| off < fork;
        for r in &mut self.rows {
            r.on_log &= kept(r.off);
        }
        for p in &mut self.posts {
            p.landed_on_log &= p.off.is_some_and(kept);
        }
        self.watches.retain(|w| {
            let Some(w) = w.upgrade() else {
                return false;
            };
            if !kept(w.off) {
                w.on_log.store(false, Ordering::Relaxed);
            }
            true
        });
        for r in &mut self.receipts {
            r.earlier_log |= !kept(r.off);
        }
        self.dedup.retain(|(off, _)| kept(*off));
        self.dedup_set.retain(|off| kept(*off));
        self.carried_marks.retain(|off| kept(*off));
        self.oldest_on_bus = self.oldest_on_bus.filter(|off| kept(*off));
        self.given_up = self.given_up.filter(|g| g.to <= fork);
        self.lane_given_up = self.lane_given_up.filter(|g| g.to <= fork);
        self.topic_given_up.retain(|_, g| g.to <= fork);
        self.addressed_head = self.addressed_head.and_then(|h| {
            if kept(h) {
                Some(h)
            } else {
                fork.checked_sub(1)
            }
        });
        if !kept(self.bus_head) {
            let rows = self.rows.iter().filter(|r| r.on_log).map(|r| r.off);
            self.bus_head = rows
                .chain(self.dedup.iter().map(|(off, _)| *off))
                .chain(self.oldest_on_bus)
                .max()
                .unwrap_or(0);
        }
        let mut retired = Vec::new();
        for post in &mut self.posts {
            if post.off.is_none() && !post.dead && post.re.is_some_and(|re| !kept(re)) {
                post.dead = true;
                post.dead_reason = LOG_REPLACED.to_string();
                post.body.clear();
                post.body.shrink_to_fit();
                post.via = None;
                post.key = None;
                retired.push(post.id);
            }
        }
        self.outbox_moves += 1;
        self.trim_retired_posts();
        self.fetches.retain(|(off, slot)| {
            kept(*off) || matches!(slot, FetchSlot::Pending | FetchSlot::Partial(_))
        });
        for (off, slot) in &mut self.fetches {
            if !kept(*off) {
                *slot = FetchSlot::Failed("log-replaced".to_string());
            }
        }
        retired
    }

    fn watch(&mut self, off: u64, on_log: bool) -> Arc<OffsetWatch> {
        self.watches.retain(|w| w.strong_count() > 0);
        let watch = Arc::new(OffsetWatch {
            off,
            on_log: AtomicBool::new(on_log),
        });
        self.watches.push(Arc::downgrade(&watch));
        watch
    }

    fn holds_bus_references(&self) -> bool {
        self.oldest_on_bus.is_some()
            || !self.receipts.is_empty()
            || !self.fetches.is_empty()
            || !self.carried_marks.is_empty()
            || self.given_up.is_some()
            || self.lane_given_up.is_some()
            || !self.topic_given_up.is_empty()
            || self
                .posts
                .iter()
                .any(|p| p.off.is_some() || (!p.dead && p.re.is_some()))
    }

    fn row_at(&self, off: u64) -> Option<&InboxRow> {
        self.rows.iter().find(|r| r.off == off && r.on_log)
    }

    /// Take the direct message `id`, which `node` sent, into the ring as an unlisted row
    /// (§11 C6): `from=fleet:<node>`, `trust=agent` (its sender's node key proved the drive it
    /// came by), no bus record. The caller waited out the update fence ([`InboxRow::fleet`]).
    #[cfg(unix)]
    fn import_fleet(&mut self, id: u64, node: &str, kind: &str, text: &str) {
        self.last_msg_id += 1;
        let row = InboxRow {
            id: self.last_msg_id,
            off: 0,
            t_ms: now_ms(),
            from: format!("fleet:{node}"),
            kind: kind.to_owned(),
            trust: "agent".to_owned(),
            re: None,
            re_id: None,
            dl: None,
            late: false,
            verdict: None,
            decision: None,
            demoted: None,
            via: None,
            len: text.len(),
            body: text.to_owned(),
            topic: None,
            listed: false,
            refill: false,
            fleet: Some(id),
            on_log: false,
        };
        evict_for(self);
        self.rows.push_back(row);
        self.arrived = handler_tally::stamp();
    }

    /// The outbound load `post` is refused against.
    pub fn queued_load(&self) -> (usize, usize) {
        self.posts
            .iter()
            .filter(|p| p.off.is_none() && !p.dead)
            .fold((0, 0), |(n, b), p| (n + 1, b + p.caller_sized_bytes()))
    }

    fn trim_retired_posts(&mut self) {
        let retired = self
            .posts
            .iter()
            .filter(|p| p.off.is_some() || p.dead)
            .count();
        let Some(mut over) = retired.checked_sub(RETIRED_KEEP).filter(|n| *n > 0) else {
            return;
        };
        self.posts.retain(|p| {
            if over > 0 && (p.off.is_some() || p.dead) {
                over -= 1;
                false
            } else {
                true
            }
        });
    }
}

/// One broadcast opt-in as the session holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopicEntry {
    /// The `since=` token the add carried.
    pub since: String,
}

/// The newest broadcast this session posted on a topic channel (`post to=say[:<topic>]`), as
/// [`SessionFabric::last_topic_post`] reads it: what a worker's row shows as its step when it is
/// newer than the words on its screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopicPost {
    /// The topic it went out on (`say` for a bare `to=say`, as the bridge names it).
    pub topic: String,
    /// Its text, at most [`TOPIC_POST_KEPT`] bytes of it, cut at a character's end.
    pub text: String,
    /// When `post` queued it, on the wall clock ([`aterm_gui_timing::wall_ms`]), which an agent's
    /// transcript rows are stamped on.
    pub at_ms: u64,
}

/// The most bytes of a topic post [`SessionFabric::last_topic_post`] keeps: an inline post's
/// whole text, and the head of a longer one.
pub const TOPIC_POST_KEPT: usize = POST_INLINE_MAX;

/// The per-session fabric state, hung on aterm-gui's `SessionCtx` ([`FabricSession`]).
#[derive(Default)]
pub struct SessionFabric {
    #[cfg(unix)]
    mesh: fleet_mail::MeshPolicy,
    inbox: Mutex<Inbox>,
    changed: aterm_control::park::Changed<Inbox>,
    direct_epoch: AtomicU64,
    topics: Mutex<std::collections::BTreeMap<String, TopicEntry>>,
    /// [`Self::last_topic_post`]. Not carried by a seamless update: the successor's rows read the
    /// screen until the session posts on a topic again.
    last_topic: Mutex<Option<TopicPost>>,
    hold_serial: std::sync::atomic::AtomicU64,
    person_refusals: AtomicU64,
    #[cfg(any(test, debug_assertions))]
    fence_wait: Mutex<Option<std::time::Duration>>,
    #[cfg(any(test, debug_assertions))]
    fence_waits: AtomicU64,
    #[cfg(any(test, debug_assertions))]
    fence_bound: Mutex<Option<Option<std::time::Duration>>>,
    /// How often an `await inbox` parked on [`Self::changed`]: counted under the ring's lock
    /// before the park, so a test that reads it and then takes the lock knows the waiter waits.
    #[cfg(test)]
    inbox_parks: AtomicU64,
    /// The bound the latest of those parks was given ([`Self::park_inbox`]).
    #[cfg(test)]
    inbox_park_until: Mutex<Option<std::time::Instant>>,
}

impl SessionFabric {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inbox> {
        self.inbox.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Whether this session's direct fleet mail enters its ring (§11 C6): the mesh switch as the
    /// session read it when it was made. Direct fleet mail is Unix's alone.
    #[must_use]
    pub fn mesh_imports(&self) -> bool {
        #[cfg(unix)]
        {
            self.mesh.enabled
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    /// Run `f` on this session's ring under its lock.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn with_inbox_for_test<T>(&self, f: impl FnOnce(&mut Inbox) -> T) -> T {
        f(&mut self.lock())
    }

    /// The bound every fenced verb on this session waits for a committing update before it
    /// answers [`HOLD_BUSY`]; a test may set its own ([`Self::set_fence_wait`]).
    pub fn fence_wait(&self) -> std::time::Duration {
        #[cfg(any(test, debug_assertions))]
        if let Some(wait) = *self.fence_wait.lock().unwrap_or_else(|p| p.into_inner()) {
            return wait;
        }
        aterm_gui_timing::current().of(HOLD_FENCE_WAIT)
    }

    /// Set this session's [`Self::fence_wait`].
    #[cfg(any(test, debug_assertions))]
    pub fn set_fence_wait(&self, wait: std::time::Duration) {
        *self.fence_wait.lock().unwrap_or_else(|p| p.into_inner()) = Some(wait);
    }

    /// How often waiters on any thread blocked on this session's fence ([`note_fence_block`]).
    #[cfg(any(test, debug_assertions))]
    pub fn fence_waits(&self) -> u64 {
        self.fence_waits.load(Ordering::SeqCst)
    }

    /// The bound the latest waiter blocked on this session's fence waits ([`note_fence_block`]).
    #[cfg(any(test, debug_assertions))]
    pub fn fence_bound(&self) -> Option<Option<std::time::Duration>> {
        *self.fence_bound.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Park an `await inbox` on [`Self::changed`] until `until`, unless a ring wakes it first.
    fn park_inbox<'a>(
        &'a self,
        guard: std::sync::MutexGuard<'a, Inbox>,
        until: std::time::Instant,
    ) -> std::sync::MutexGuard<'a, Inbox> {
        #[cfg(test)]
        {
            self.inbox_parks.fetch_add(1, Ordering::SeqCst);
            *self
                .inbox_park_until
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = Some(until);
        }
        self.changed
            .park(guard, until, aterm_control::park::Lane::SERVED)
    }

    /// Wake every waiter parked on this session's condvar without changing anything.
    #[cfg(any(test, debug_assertions))]
    pub fn wake_waiters_for_test(&self) {
        self.changed.ring(self.lock());
    }

    /// The session ended: its program exited or its tab closed.
    pub fn close(&self) {
        let mut inbox = self.lock();
        inbox.closed = true;
        self.changed.ring(inbox);
    }

    /// Whether [`Self::close`] has run.
    pub fn endpoint_closed(&self) -> bool {
        self.lock().closed
    }

    /// The delivered rows newer than `after` that `want` asks for, the highest id ever delivered
    /// here, and how many rows above `after` the ring still holds ([`MailSince`]). A row the mesh
    /// imported from direct fleet mail ([`InboxRow::fleet`]) names no bus record, and is not
    /// among them: [`Self::direct_mail_since`] tells of its arrival by its mailbox id.
    pub fn mail_since(&self, after: u64, want: &MailFilter) -> MailSince {
        let inbox = self.lock();
        if after >= inbox.last_msg_id {
            return MailSince {
                rows: Vec::new(),
                high: inbox.last_msg_id,
                present: 0,
            };
        }
        MailSince {
            rows: inbox
                .rows
                .iter()
                .filter(|r| r.id > after && r.fleet.is_none() && want.wants(r))
                .map(|r| MailLine {
                    id: r.id,
                    off: r.off,
                    from: r.from.clone(),
                    kind: r.kind.clone(),
                    re: r.re,
                    topic: r.topic.clone(),
                })
                .collect(),
            high: inbox.last_msg_id,
            present: inbox.rows.iter().filter(|r| r.id > after).count() as u64,
        }
    }

    /// The id of the newest row ever delivered here, 0 before the first.
    #[must_use]
    pub fn last_msg_id(&self) -> u64 {
        self.lock().last_msg_id
    }

    /// Cached direct arrivals newer than the independent direct cursor. No storage I/O.
    pub fn direct_mail_since(
        &self,
        after: u64,
        want: &MailFilter,
    ) -> (u64, Vec<fleet_mail::Notice>) {
        let inbox = self.lock();
        (
            inbox.direct_mail.high,
            inbox
                .direct_mail
                .rows
                .iter()
                .filter(|row| row.id > after && want.wants_direct(row))
                .cloned()
                .collect(),
        )
    }

    /// The direct mailbox's cached high-water mark, independently of broker ids.
    pub fn last_direct_mail_id(&self) -> u64 {
        self.lock().direct_mail.high
    }

    /// The topics this session opted into with each one's `since=` token, in topic order.
    pub fn topics(&self) -> Vec<(String, TopicEntry)> {
        self.topics
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(t, e)| (t.clone(), e.clone()))
            .collect()
    }

    /// The newest post this session queued on a topic channel ([`TopicPost`]), if any.
    pub fn last_topic_post(&self) -> Option<TopicPost> {
        self.last_topic
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Note `text`, queued on `topic` just now, as [`Self::last_topic_post`].
    fn note_topic_post(&self, topic: &str, text: &str) {
        let mut end = text.len().min(TOPIC_POST_KEPT);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        *self.last_topic.lock().unwrap_or_else(|p| p.into_inner()) = Some(TopicPost {
            topic: topic.to_owned(),
            text: text[..end].to_owned(),
            at_ms: aterm_gui_timing::wall_ms(),
        });
    }

    /// Whether this session holds `topic` in its opt-in set.
    pub fn opted_into(&self, topic: &str) -> bool {
        self.topics
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(topic)
    }

    /// Add a topic with the `since=` token to resume it from.
    pub fn topic_add(
        &self,
        t: &str,
        since: &str,
        timeline: &Mutex<impl Timeline>,
    ) -> Result<bool, TopicsFull> {
        self.topic_add_recording(t, since, &|kind, fact| {
            let _ = timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record_fact(kind, fact);
        })
    }

    /// [`Self::topic_add`], recording the move's [`FabricFact::Topic`] through `record` (a
    /// session's own [`FabricSession::record_fact`]).
    pub fn topic_add_recording(
        &self,
        t: &str,
        since: &str,
        record: &dyn Fn(&'static str, FabricFact<'_>),
    ) -> Result<bool, TopicsFull> {
        let mut set = self.topics.lock().unwrap_or_else(|p| p.into_inner());
        if set.contains_key(t) {
            return Ok(false);
        }
        if set.len() >= MAX_TOPICS {
            return Err(TopicsFull);
        }
        set.insert(
            t.to_string(),
            TopicEntry {
                since: since.to_string(),
            },
        );
        let fact = FabricFact::Topic {
            add: true,
            name: t,
            since: Some(since),
        };
        record("topic", fact);
        Ok(true)
    }

    /// Drop a topic.
    pub fn topic_drop(&self, t: &str, timeline: &Mutex<impl Timeline>) -> bool {
        self.topic_drop_recording(t, &|kind, fact| {
            let _ = timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record_fact(kind, fact);
        })
    }

    /// [`Self::topic_drop`], recording through `record`, as [`Self::topic_add_recording`] does.
    pub fn topic_drop_recording(
        &self,
        t: &str,
        record: &dyn Fn(&'static str, FabricFact<'_>),
    ) -> bool {
        let mut set = self.topics.lock().unwrap_or_else(|p| p.into_inner());
        let dropped = set.remove(t).is_some();
        if dropped {
            let fact = FabricFact::Topic {
                add: false,
                name: t,
                since: None,
            };
            record("topic", fact);
        }
        dropped
    }

    /// Replace the whole set.
    pub fn set_topics(&self, topics: impl IntoIterator<Item = (String, TopicEntry)>) {
        let mut set = self.topics.lock().unwrap_or_else(|p| p.into_inner());
        *set = topics.into_iter().collect();
    }

    /// Whether broker offset `off` names mail this session holds (what `cause=@<off>` may name).
    pub fn holds_offset(&self, off: u64) -> bool {
        let inbox = self.lock();
        inbox.rows.iter().any(|r| r.off == off)
            || inbox.dedup_set.contains(&off)
            || inbox.posts.iter().any(|p| p.off == Some(off) && !p.dead)
    }

    /// The session's standing halt, if any.
    pub fn hold(&self) -> Option<Hold> {
        self.lock().holds.first()
    }

    /// The aggregate hold and its release history, read in one transaction.
    #[must_use]
    pub fn hold_observation(&self) -> HoldObservation {
        let inbox = self.lock();
        inbox.hold_history.observation(inbox.holds.is_held())
    }

    /// Every standing halt on the session, one slot per origin.
    pub fn holds(&self) -> Holds {
        self.lock().holds.clone()
    }

    /// How many acts the person's hold refused since it was set ([`halt_refusal`]).
    pub fn person_refusals(&self) -> u64 {
        self.person_refusals.load(Ordering::Relaxed)
    }

    /// The halt a lost bridge did not make on this session, while it still speaks for the link
    /// (`status hold_advisory=`).
    pub fn hold_advisory(&self) -> Option<Hold> {
        let current = *LINK.generation.lock().unwrap_or_else(|p| p.into_inner());
        let inbox = self.lock();
        match &inbox.advisory {
            Some((hold, lost_under)) if *lost_under == current && !inbox.holds.is_held() => {
                Some(hold.clone())
            }
            _ => None,
        }
    }

    /// How many times this session's hold has moved, a direct reset counted too
    /// ([`Self::note_direct_reset`]).
    #[cfg(any(test, debug_assertions))]
    pub fn hold_serial(&self) -> u64 {
        self.hold_serial.load(Ordering::Acquire)
    }

    /// The commit's reading of this session's hold, and the fence that keeps it true until the
    /// process exits.
    #[cfg(any(unix, test, debug_assertions))]
    pub fn fence_hold(&self, successor_reads_carry: bool) -> u64 {
        let mut inbox = self.lock();
        inbox.hold_fenced = true;
        inbox.retirements_fenced = successor_reads_carry;
        Self::serial_of(&self.hold_serial, &inbox)
    }

    /// A serial of what a handoff carries of this session's fabric that can move after the park
    /// drew the manifest.
    #[cfg(any(unix, test, debug_assertions))]
    pub fn carry_serial(&self) -> u64 {
        let inbox = self.lock();
        Self::serial_of(&self.hold_serial, &inbox)
    }

    #[cfg(any(unix, test, debug_assertions))]
    fn serial_of(hold_serial: &AtomicU64, inbox: &Inbox) -> u64 {
        hold_serial
            .load(Ordering::Acquire)
            .wrapping_add(inbox.outbox_moves)
    }

    /// Whether this session has numbered an inbox row past the id the last handoff manifest
    /// carried ([`Inbox::msg_id_ceiling`]).
    #[cfg(any(unix, test, debug_assertions))]
    pub fn past_handoff_ceiling(&self) -> bool {
        let inbox = self.lock();
        inbox
            .msg_id_ceiling
            .is_some_and(|ceiling| inbox.last_msg_id > ceiling)
    }

    fn wait_fenced_delivery<'a>(
        &'a self,
        mut inbox: std::sync::MutexGuard<'a, Inbox>,
        sid: &str,
        topic: Option<&str>,
    ) -> std::sync::MutexGuard<'a, Inbox> {
        let every = aterm_gui_timing::current().of(MSG_ROOM_NOTICE_EVERY);
        let mut waited = std::time::Duration::ZERO;
        while inbox.hold_fenced
            && (topic.is_some()
                || inbox
                    .msg_id_ceiling
                    .is_some_and(|ceiling| inbox.last_msg_id >= ceiling))
        {
            let (guard, timeout) = self.changed.wait_timeout(inbox, every);
            inbox = guard;
            if timeout.timed_out() && inbox.hold_fenced {
                waited = waited.saturating_add(every);
                let what = match topic {
                    Some(topic) => format!("a broadcast on topic {topic}"),
                    None => format!("a message past the {INBOX_ID_RESERVE}-id overlap reserve"),
                };
                aterm_log::warn!(
                    "update apply: {what} for session {sid} has waited {:.1} s on a \
                     committing update, unanswered; the bridge delivers it again to the new \
                     aterm when the update lands, or here when it stands down",
                    waited.as_secs_f64()
                );
            }
        }
        inbox
    }

    fn wait_unfenced<'a>(
        &'a self,
        mut inbox: std::sync::MutexGuard<'a, Inbox>,
        wait: std::time::Duration,
    ) -> Option<std::sync::MutexGuard<'a, Inbox>> {
        let deadline = deadline_after(wait);
        while inbox.hold_fenced {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            note_fence_block(self, Some(wait));
            inbox = self.changed.wait_timeout(inbox, left).0;
        }
        Some(inbox)
    }

    /// Run `change`, a change the handoff carries that lives off this endpoint (a task mark,
    /// `task_record::mark_session_task`), with its locked aggregate hold observation once no
    /// update fences the session; `None` past `wait`.
    pub fn run_unfenced<T>(
        &self,
        wait: std::time::Duration,
        change: impl FnOnce(HoldObservation) -> T,
    ) -> Option<T> {
        let unfenced = self.wait_unfenced(self.lock(), wait)?;
        let out = change(unfenced.hold_history.observation(unfenced.holds.is_held()));
        drop(unfenced);
        Some(out)
    }

    /// A manual reset that ran directly on this session's engine (`manual_reset::run_direct`),
    /// outside the byte stream, made known to a seamless Commit before it is answered.
    pub fn note_direct_reset(&self, fence_wait: Option<std::time::Duration>) -> bool {
        let inbox = self.lock();
        let inbox = match fence_wait {
            Some(wait) => match self.wait_unfenced(inbox, wait) {
                Some(inbox) => inbox,
                None => return false,
            },
            None => inbox,
        };
        self.hold_serial.fetch_add(1, Ordering::AcqRel);
        drop(inbox);
        true
    }

    fn wait_unfenced_retirement<'a>(
        &'a self,
        inbox: std::sync::MutexGuard<'a, Inbox>,
        wait: std::time::Duration,
    ) -> Option<std::sync::MutexGuard<'a, Inbox>> {
        if inbox.retirements_fenced {
            self.wait_unfenced(inbox, wait)
        } else {
            Some(inbox)
        }
    }

    /// The outbound half the handoff carries ([`FabricCarry`]).
    #[cfg(any(unix, test, debug_assertions))]
    pub fn handoff_carry(&self, budget: &mut usize) -> FabricCarry {
        let mut inbox = self.lock();
        let carried_msg_id = inbox
            .last_msg_id
            .saturating_add(INBOX_ID_RESERVE)
            .min(CARRY_INT_MAX);
        inbox.msg_id_ceiling = Some(carried_msg_id);
        let unread_broadcasts = inbox
            .rows
            .iter()
            .filter(|r| r.topic.is_some() && r.id > inbox.seen)
            .count();
        let remembered = |r: &InboxRow| {
            inbox
                .last_msg_id
                .checked_sub(r.id)
                .and_then(|n| usize::try_from(n).ok())
                .is_some_and(|n| n < RING_CAP)
        };
        let refilled_broadcasts = inbox
            .rows
            .iter()
            .filter(|r| r.refill && r.id > inbox.seen && r.on_log)
            .filter(|r| remembered(r))
            .filter(|r| r.topic.as_deref().is_some_and(|t| self.opted_into(t)))
            .count();
        let count = |n: usize| u64::try_from(n).unwrap_or(u64::MAX).min(CARRY_INT_MAX);
        let mut carry = FabricCarry {
            bus_log: inbox.bus_log.map(|log| log.to_string()),
            last_msg_id: carried_msg_id,
            last_post_id: inbox.next_post_id.min(CARRY_INT_MAX),
            last_receipt_id: inbox.next_receipt_id.min(CARRY_INT_MAX),
            dropped: inbox.dropped.min(CARRY_INT_MAX),
            broadcasts_uncarried: count(unread_broadcasts),
            broadcasts_refilled: count(refilled_broadcasts),
            given_up: inbox.given_up.filter(GivenUp::carriable),
            lane_given_up: inbox.lane_given_up.filter(GivenUp::carriable),
            ..FabricCarry::default()
        };
        for post in inbox.posts.iter().filter(|p| p.off.is_none() && !p.dead) {
            let row = PostCarry {
                id: post.id,
                to: post.to.clone(),
                kind: post.kind.clone(),
                re: post.re,
                dl: post.dl,
                via: post.via.clone(),
                key: post.key.clone(),
                body: post.body.clone(),
            };
            let writable = row.id <= CARRY_INT_MAX
                && row.re.is_none_or(|n| n <= CARRY_INT_MAX)
                && row.dl.is_none_or(|n| n <= CARRY_INT_MAX);
            if writable && row.cost() <= *budget {
                *budget -= row.cost();
                carry.posts.push(row);
            } else if post.id <= CARRY_INT_MAX {
                carry.uncarried.push(post.id);
            }
        }
        for r in &inbox.receipts {
            let row = ReceiptCarry {
                rid: r.rid,
                off: r.off,
                verdict: r.verdict.clone(),
                kind: r.kind.clone(),
                from: r.from.clone(),
            };
            let writable = row.rid <= CARRY_INT_MAX && row.off <= CARRY_INT_MAX;
            if writable && row.cost() <= *budget {
                *budget -= row.cost();
                if r.earlier_log || inbox.bus_log.is_none() {
                    carry.earlier_receipts.push(row);
                } else {
                    carry.receipts.push(row);
                }
            } else {
                carry.receipts_uncarried = carry.receipts_uncarried.saturating_add(1);
            }
        }
        for off in inbox
            .dedup
            .iter()
            .rev()
            .filter(|(off, marked)| *marked && *off <= CARRY_INT_MAX)
            .map(|(off, _)| *off)
        {
            if mark_cost(off) > *budget {
                break;
            }
            *budget -= mark_cost(off);
            carry.marked.push(off);
        }
        carry.marked.reverse();
        carry
    }

    /// What this session's keeper meta carries of its fabric ([`KeeperCarry`]).
    #[cfg(unix)]
    pub fn keeper_carry(&self) -> KeeperCarry {
        let topics = render_topics(&self.topics());
        let inbox = self.lock();
        let mut holds = render_holds(&inbox.holds).into_iter();
        let on_log = |g: Option<GivenUp>| inbox.bus_log.as_ref().and(g.filter(GivenUp::carriable));
        let (given_up, lane_given_up) = (on_log(inbox.given_up), on_log(inbox.lane_given_up));
        let given_up_log = inbox
            .bus_log
            .filter(|_| given_up.is_some() || lane_given_up.is_some())
            .map(|log| log.to_string());
        KeeperCarry {
            hold: holds.next(),
            more_holds: holds.collect(),
            ids: [
                id_lease(inbox.last_msg_id),
                id_lease(inbox.next_post_id),
                id_lease(inbox.next_receipt_id),
            ],
            topics,
            topics_lost: 0,
            queued: inbox
                .posts
                .iter()
                .filter(|p| p.off.is_none() && !p.dead)
                .map(|p| p.id)
                .collect(),
            queued_lost: 0,
            owed: u64::try_from(inbox.receipts.len()).unwrap_or(u64::MAX),
            dropped: inbox.dropped.min(CARRY_INT_MAX),
            given_up,
            lane_given_up,
            given_up_log,
        }
    }

    /// Put back what a seamless update carried of this session's mailbox.
    pub fn seed_carry(&self, carry: &FabricCarry, timeline: &Mutex<impl Timeline>) {
        if carry.is_empty() {
            return;
        }
        let mut inbox = self.lock();
        inbox.bus_log = carry.bus_log.as_deref().and_then(LogIdentity::parse);
        inbox.last_msg_id = inbox.last_msg_id.max(carry.last_msg_id);
        inbox.next_post_id = inbox.next_post_id.max(carry.last_post_id);
        inbox.next_receipt_id = inbox.next_receipt_id.max(carry.last_receipt_id);
        let lost_broadcasts = carry
            .broadcasts_uncarried
            .saturating_sub(carry.broadcasts_refilled);
        inbox.dropped = inbox
            .dropped
            .saturating_add(carry.dropped)
            .saturating_add(lost_broadcasts)
            .min(CARRY_INT_MAX);
        let mut dead: Vec<u64> = carry.uncarried.clone();
        let mut seen_ids = HashSet::new();
        for post in &carry.posts {
            if !post.valid(carry.last_post_id)
                || !seen_ids.insert(post.id)
                || inbox.posts.len() >= OUTBOX_CAP
            {
                dead.push(post.id);
                continue;
            }
            inbox.posts.push_back(PostRow {
                id: post.id,
                to: post.to.clone(),
                kind: post.kind.clone(),
                off: None,
                landed_on_log: false,
                re: post.re,
                dl: post.dl,
                via: post.via.clone(),
                key: post.key.clone(),
                dup: false,
                dead: false,
                dead_reason: String::new(),
                body: post.body.clone(),
            });
        }
        let mut tl = timeline.lock().unwrap_or_else(|p| p.into_inner());
        let on_log = inbox.bus_log.is_some();
        let log_carried = tl.log_carried();
        let told = &mut *inbox;
        for (carried, slot) in [
            (carry.given_up, &mut told.given_up),
            (carry.lane_given_up, &mut told.lane_given_up),
        ] {
            let Some(g) = carried
                .filter(|g| on_log && g.carriable())
                .filter(|g| !slot.is_some_and(|told| told.repeated_by(g)))
            else {
                continue;
            };
            *slot = Some(g);
            if !log_carried {
                let fact = FabricFact::MailGap {
                    from: g.from,
                    to: g.to,
                    records: g.records,
                };
                let _ = tl.record_fact("inbox-gap", fact);
            }
        }
        if tl.log_carried() {
            inbox.carried_marks = carry
                .marked
                .iter()
                .rev()
                .copied()
                .filter(|off| *off <= CARRY_INT_MAX)
                .take(DEDUP_CAP)
                .collect();
        }
        dead.sort_unstable();
        dead.dedup();
        for id in dead
            .into_iter()
            .filter(|id| (1..=carry.last_post_id).contains(id) && !seen_ids.contains(id))
        {
            inbox.posts.push_back(PostRow {
                id,
                to: "-".to_string(),
                kind: "note".to_string(),
                off: None,
                landed_on_log: false,
                re: None,
                dl: None,
                via: None,
                key: None,
                dup: false,
                dead: true,
                dead_reason: NOT_CARRIED.to_string(),
                body: String::new(),
            });
            let fact = FabricFact::MailLanded {
                post: id,
                off: None,
                why: Some(DeadReason::NotCarried),
            };
            let _ = tl.record_fact("post-landed", fact);
        }
        let mut rids = HashSet::new();
        let unknown_log = inbox.bus_log.is_none();
        for (r, earlier_log) in carry
            .earlier_receipts
            .iter()
            .map(|r| (r, true))
            .chain(carry.receipts.iter().map(|r| (r, false)))
        {
            if r.valid(carry.last_receipt_id) && rids.insert(r.rid) {
                inbox.receipts.push_back(OwedReceipt {
                    rid: r.rid,
                    off: r.off,
                    verdict: r.verdict.clone(),
                    kind: r.kind.clone(),
                    from: r.from.clone(),
                    earlier_log: earlier_log || unknown_log,
                });
            }
        }
        inbox.receipts.make_contiguous().sort_by_key(|r| r.rid);
        while inbox.receipts.len() > RECEIPTS_OWED_MAX {
            inbox.receipts.pop_front();
        }
        if carry.receipts_uncarried > 0 {
            aterm_log::warn!(
                "fabric: {} receipt(s) this session owed were not carried (the handoff's \
                 mailbox budget, or a crash the PTY keeper recovered the session from); their \
                 senders wait out their own deadlines",
                carry.receipts_uncarried
            );
        }
        if lost_broadcasts > 0 {
            aterm_log::warn!(
                "seamless update: {lost_broadcasts} broadcast(s) this session had not handled \
                 when the update was drawn were not carried (its bridge does not offer \
                 broadcasts again, or the session dropped their topic); counted in its \
                 inbox's dropped="
            );
        }
        if carry.broadcasts_refilled > 0 {
            aterm_log::info!(
                "seamless update: {} broadcast(s) this session had not handled when the \
                 update was drawn are offered again by its bridge",
                carry.broadcasts_refilled.min(carry.broadcasts_uncarried)
            );
        }
        drop(tl);
        inbox.trim_retired_posts();
        self.changed.ring(inbox);
    }

    /// The attempt that fenced this session stood down.
    #[cfg(any(unix, test, debug_assertions))]
    pub fn lift_hold_fence(&self) {
        let mut inbox = self.lock();
        inbox.hold_fenced = false;
        inbox.retirements_fenced = false;
        self.changed.ring(inbox);
    }

    /// Put back the holds another process carried ([`parse_holds`] already made them holds).
    pub fn seed_holds(&self, carried: Holds, timeline: &Mutex<impl Timeline>) {
        if !carried.is_held() {
            return;
        }
        let mut inbox = self.lock();
        let before = inbox.holds.clone();
        for hold in carried.standing() {
            inbox.holds.seed(hold);
        }
        if inbox.holds == before {
            return;
        }
        let held = inbox.holds.is_held();
        inbox
            .hold_history
            .moved(before.is_held(), held, std::time::Instant::now());
        let (origin, reason, hold_origin) = inbox.holds.event_fields("local");
        let fact = FabricFact::Hold {
            held: true,
            bridge: false,
            origin: &origin,
            hold_origin: &hold_origin,
            reason: reason.as_deref(),
            carried: true,
        };
        let _ = timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record_fact("hold", fact);
        self.hold_serial.fetch_add(1, Ordering::AcqRel);
        self.changed.ring(inbox);
    }

    /// The mail half of the presence facts (aterm-gui's `App::sense`).
    pub fn mail_facts(&self) -> aterm_messages::presence::MailFacts {
        let inbox = self.lock();
        let seen = inbox.seen;
        // With the mesh on, the ring holds every unread direct message (§11 C6), counted there.
        let direct = if self.mesh_imports() {
            &[][..]
        } else {
            &inbox.direct_mail.rows[..]
        };
        let mut direct_unread = direct.iter().filter(|r| !r.replied);
        let unread = inbox.rows.iter().filter(|r| r.id > seen).count() as u64
            + direct_unread.clone().count() as u64;
        let pending = inbox
            .rows
            .iter()
            .filter(|r| r.id > seen && r.listed)
            .count() as u64
            + direct_unread.clone().filter(|r| r.listed).count() as u64;
        let queued = inbox
            .posts
            .iter()
            .filter(|p| p.off.is_none() && !p.dead)
            .count() as u64;
        let is_unread = |r: &&InboxRow| r.id > seen;
        let direct_last = direct_unread
            .clone()
            .rev()
            .find(|r| r.kind == "task" || r.kind == "ask")
            .or_else(|| direct_unread.next_back())
            .map(|r| aterm_messages::presence::MailLast {
                kind: r.kind.clone(),
                from: r.from.clone(),
                trust: "agent".to_owned(),
            });
        let last = direct_last.or_else(|| {
            inbox
                .rows
                .iter()
                .rev()
                .filter(is_unread)
                .find(|r| r.kind == "task" || r.kind == "ask")
                .or_else(|| inbox.rows.iter().rev().find(is_unread))
                .map(|r| aterm_messages::presence::MailLast {
                    kind: r.kind.clone(),
                    from: r.from.clone(),
                    trust: r.trust.clone(),
                })
        });
        aterm_messages::presence::MailFacts {
            unread,
            pending,
            dropped: inbox.dropped,
            queued,
            last,
            head: inbox.last_msg_id.saturating_add(inbox.direct_mail.high),
        }
    }

    /// What the resident pet's room reads: the newest row id when it is unseen (else 0), whether an
    /// unseen row is past its `dl=`, and whether a hold stands.
    pub fn room_facts(&self, now_ms: u64) -> (u64, bool, bool) {
        let inbox = self.lock();
        let seen = inbox.seen;
        let unseen = inbox
            .rows
            .back()
            .map_or(0, |r| if r.id > seen { r.id } else { 0 });
        let overdue = inbox
            .rows
            .iter()
            .any(|r| r.id > seen && r.dl.is_some_and(|dl| now_ms > r.t_ms.saturating_add(dl)));
        (unseen, overdue, inbox.holds.is_held())
    }
}

const FABRIC_ABSENT: u8 = 0;
const FABRIC_CONNECTED: u8 = 1;
const FABRIC_DISCONNECTED: u8 = 2;
const FABRIC_STALLED: u8 = 3;

const LINK_REFRESH: std::time::Duration = std::time::Duration::from_secs(2);

const STALE_AFTER: std::time::Duration = LINK_REFRESH.saturating_mul(3);

const STALE_AFTER_SEAM: &str = "ATERM_FABRIC_STALE_AFTER_MS";

/// A link test's own stale horizon in ms (0: none); the link reset clears it.
#[cfg(test)]
static STALE_AFTER_IN_TEST: AtomicU64 = AtomicU64::new(0);

fn stale_after() -> std::time::Duration {
    #[cfg(test)]
    match STALE_AFTER_IN_TEST.load(Ordering::Relaxed) {
        0 => {}
        ms => return std::time::Duration::from_millis(ms),
    }
    static AT: std::sync::OnceLock<std::time::Duration> = std::sync::OnceLock::new();
    *AT.get_or_init(|| {
        aterm_types::dev_seam!(STALE_AFTER_SEAM)
            .and_then(|ms| {
                ms.to_str()
                    .filter(|s| !s.starts_with('+'))?
                    .parse::<u64>()
                    .ok()
            })
            .map_or(STALE_AFTER, std::time::Duration::from_millis)
    })
}

const LINK_STARTING: &str = "starting";
#[cfg(any(unix, test, debug_assertions))]
const LINK_BRIDGE_LOST: &str = "bridge-lost";
const LINK_REASON_MAX: usize = 32;

struct LinkReport {
    up: bool,
    reason: String,
    rtt_ms: Option<u64>,
    acked_at: Option<std::time::Instant>,
    stalled_at: Option<std::time::Instant>,
    reported: bool,
    owed_since: Option<std::time::Instant>,
    hello: Option<BridgeHello>,
    mirror: Option<MirrorReport>,
    lane_faults: u64,
}

#[derive(Clone)]
struct BridgeHello {
    lane: u32,
    build: Option<String>,
    refills_broadcasts: bool,
    log: HelloLog,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HelloLog {
    /// No `log=`, or a word this endpoint does not read.
    Off,
    /// `log=auto`: a `[fabric] log = "auto"` bridge (the default) whose last attach did not find
    /// the broker retaining; over a retaining broker that bridge says `log=turns`.
    Auto,
    /// `log=turns`: it asks this endpoint to serve `log-take`/`log-landed`.
    Turns,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MirrorReport {
    state: MirrorState,
    reason: String,
}

/// The bus log the bridge last told (`link log`), and the move it came by: `continues` is
/// `(was, fork)` when the bridge judged `ident` to continue `was` below `fork`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LogClock {
    ident: LogIdentity,
    continues: Option<(LogIdentity, u64)>,
}

impl LogClock {
    /// Below which offset a session whose clock is `held` keeps what it holds: the fork, for
    /// `was` itself or a generation between `was` and `ident`; `None` (keep nothing) for any
    /// other clock, or none.
    fn keep_below(&self, held: Option<LogIdentity>) -> Option<u64> {
        let (was, fork) = self.continues?;
        let held = held?;
        let in_chain = held == was || (held.continues_from(was) && self.ident.continues_from(held));
        in_chain.then_some(fork)
    }
}

struct FabricLink {
    state: AtomicU8,
    generation: Mutex<u64>,
    touched: Mutex<BTreeMap<usize, TouchedRegistry>>,
    supervised: AtomicBool,
    link: Mutex<LinkReport>,
    log: Mutex<Option<LogClock>>,
    log_transition: Mutex<()>,
    node_gaps: Mutex<NodeGaps>,
    #[cfg(any(unix, test, debug_assertions))]
    hold_on_loss: AtomicBool,
}

static LINK: FabricLink = FabricLink {
    state: AtomicU8::new(FABRIC_ABSENT),
    generation: Mutex::new(0),
    touched: Mutex::new(BTreeMap::new()),
    supervised: AtomicBool::new(false),
    #[cfg(any(unix, test, debug_assertions))]
    hold_on_loss: AtomicBool::new(false),
    log: Mutex::new(None),
    log_transition: Mutex::new(()),
    node_gaps: Mutex::new(NodeGaps {
        ranges: 0,
        newest: None,
    }),
    link: Mutex::new(LinkReport {
        up: false,
        reason: String::new(),
        rtt_ms: None,
        acked_at: None,
        stalled_at: None,
        reported: false,
        owed_since: None,
        hello: None,
        mirror: None,
        lane_faults: 0,
    }),
};

/// How a stored link transition came about ([`LinkMove`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkSaid {
    /// A new bridge incarnation attached ([`bridge_attached`]).
    Attached,
    /// A bridge that never reports was read by what it did ([`link_evidence`]).
    Evidence,
    /// The bridge's own report of its broker link ([`link_report`]).
    Report,
    /// The owning incarnation's lane closed ([`bridge_lost`]).
    Lost,
}

/// One stored transition of the link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkMove {
    /// The stored state's [`fabric_state`] word.
    pub state: &'static str,
    /// The bridge incarnation it speaks for.
    pub generation: u64,
    /// How it came about.
    pub said: LinkSaid,
}

/// What hears each [`LinkMove`].
pub type LinkMoved = fn(LinkMove);

static LINK_MOVED: std::sync::OnceLock<LinkMoved> = std::sync::OnceLock::new();

/// Have every stored link transition call `hook`.
pub fn on_link_moved(hook: LinkMoved) {
    let _ = LINK_MOVED.set(hook);
}

fn link_moved(state: u8, generation: u64, said: LinkSaid) {
    if let Some(hook) = LINK_MOVED.get() {
        hook(LinkMove {
            state: state_word(state),
            generation,
            said,
        });
    }
}

const fn state_word(state: u8) -> &'static str {
    match state {
        FABRIC_CONNECTED => "connected",
        FABRIC_DISCONNECTED => "disconnected",
        FABRIC_STALLED => "stalled",
        _ => "absent",
    }
}

/// Publish a newly created or adopted session into the registry.
pub fn register_on_log<T>(ctx: &dyn FabricSession, publish: impl FnOnce() -> T) -> T {
    let _transition = LINK
        .log_transition
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let current =
        unless_another_link_section(|| *LINK.log.lock().unwrap_or_else(|p| p.into_inner()))
            .flatten();
    if let Some(clock) = current {
        let mut inbox = ctx.fabric().lock();
        let forget = inbox
            .bus_log
            .map_or_else(|| inbox.holds_bus_references(), |was| was != clock.ident);
        if forget {
            let keep_below = clock.keep_below(inbox.bus_log);
            for id in inbox.forget_bus_offsets(keep_below) {
                let fact = FabricFact::MailLanded {
                    post: id,
                    off: None,
                    why: Some(DeadReason::LogReplaced),
                };
                let _ = ctx.record_fact("post-landed", fact);
            }
        }
        inbox.bus_log = Some(clock.ident);
        ctx.fabric().changed.ring(inbox);
    }
    publish()
}

thread_local! {
    static LANE_GENERATION: std::cell::Cell<Option<BridgeGeneration>> =
        const { std::cell::Cell::new(None) };
}

/// Record, for this thread, which bridge incarnation it is serving a lane of.
#[cfg(any(unix, test, debug_assertions))]
pub fn set_lane_generation(generation: BridgeGeneration) {
    LANE_GENERATION.with(|g| g.set(Some(generation)));
}

/// The generation the calling thread's bridge lane belongs to, if it is one.
pub fn lane_generation() -> Option<BridgeGeneration> {
    LANE_GENERATION.with(std::cell::Cell::get)
}

struct TouchedRegistry {
    registry: Weak<RwLock<dyn Registry>>,
    sids: BTreeSet<String>,
}

fn registry_key(store: &Store) -> usize {
    Arc::as_ptr(store) as *const () as usize
}

const TOUCHED_PRUNE_AT: usize = 1024;

/// One bridge incarnation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BridgeGeneration(u64);

/// Mint the identity of one bridge incarnation.
#[cfg(any(unix, test, debug_assertions))]
pub fn next_bridge_generation() -> BridgeGeneration {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    BridgeGeneration(NEXT.fetch_add(1, Ordering::Relaxed).wrapping_add(1))
}

/// A bridge supervisor has started in this process.
#[cfg(any(unix, test, debug_assertions))]
pub fn note_bridge_supervised() {
    LINK.supervised.store(true, Ordering::Relaxed);
}

/// Whether a bridge supervisor is running in this process.
#[cfg(unix)]
pub fn bridge_supervised() -> bool {
    LINK.supervised.load(Ordering::Relaxed)
}

fn bridge_reachable() -> bool {
    LINK.supervised.load(Ordering::Relaxed) || LINK.state.load(Ordering::Relaxed) != FABRIC_ABSENT
}

fn link_is_stale() -> bool {
    let link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
    link.owed_since
        .is_some_and(|t| t.elapsed() >= stale_after())
}

fn stale_in() -> Option<std::time::Duration> {
    let link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
    link.owed_since
        .map(|t| stale_after().saturating_sub(t.elapsed()))
}

fn note_work_owed() {
    unless_another_link_section(|| {
        // Only a connected bridge can owe anything: the gate `bridge_attached`, `link_report`
        // and `link_evidence` keep too, so a caller with no live bridge cannot move the link's
        // state.
        if LINK.state.load(Ordering::Relaxed) != FABRIC_CONNECTED {
            return;
        }
        let mut link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
        if link.owed_since.is_none() {
            link.owed_since = Some(std::time::Instant::now());
        }
    });
}

fn posts_outstanding(store: &Store) -> bool {
    let ctxs: Vec<_> = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.sessions()
    };
    ctxs.iter().any(|ctx| {
        let inbox = ctx.fabric().lock();
        inbox.posts.iter().any(|p| p.off.is_none() && !p.dead)
    })
}

fn settle_work_owed(store: &Store) {
    // Read before the link section's turn: it takes each session's fabric lock, which
    // `note_work_owed`'s callers hold while they take that turn.
    let outstanding = posts_outstanding(store);
    unless_another_link_section(|| {
        let mut link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
        link.owed_since = outstanding.then(std::time::Instant::now);
    });
}

#[cfg(test)]
fn backdate_work_owed(by: std::time::Duration) {
    let mut link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
    link.owed_since = link.owed_since.map(|t| t.checked_sub(by).unwrap_or(t));
}

/// The `status` reply's `fabric=` token.
pub fn fabric_state() -> &'static str {
    state_token(LINK.state.load(Ordering::Relaxed), link_is_stale)
}

/// The `fabric=` token of the stored state `state`, `stale` asked only of a connected one.
fn state_token(state: u8, stale: impl FnOnce() -> bool) -> &'static str {
    match state {
        FABRIC_CONNECTED if stale() => "stale",
        FABRIC_CONNECTED => "connected",
        FABRIC_DISCONNECTED => "disconnected",
        FABRIC_STALLED => "stalled",
        _ => "absent",
    }
}

/// `fabric status`'s state and link facts (reason, rtt, ack age) as one moment holds them: read
/// under the link's lock, which every writer of the state holds while it writes, so the line
/// never pairs a `stalled` with the `-` of the link that was up a moment before (a bridge that
/// redialed a broker shutting down, its link up, then down again, between the two reads).
pub fn fabric_link_status() -> (&'static str, String, Option<u64>, Option<u64>) {
    let link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
    let state = state_token(LINK.state.load(Ordering::Relaxed), || {
        link.owed_since
            .is_some_and(|t| t.elapsed() >= stale_after())
    });
    let (reason, rtt, age) = link_facts(&link);
    (state, reason, rtt, age)
}

/// [`fabric_state`] as an App's presence facts read it.
pub fn fabric_state_seen() -> &'static str {
    unless_another_link_section(fabric_state).unwrap_or("absent")
}

/// The link's three reported facts (reason, rtt, ack age), as `fabric status` prints them;
/// `status` prints the last two ([`fabric_status_tail`]).
pub fn fabric_link_facts() -> (String, Option<u64>, Option<u64>) {
    link_facts(&LINK.link.lock().unwrap_or_else(|p| p.into_inner()))
}

/// [`fabric_link_facts`] of the link report `link`.
fn link_facts(link: &LinkReport) -> (String, Option<u64>, Option<u64>) {
    let reason = if link.up || link.reason.is_empty() {
        "-".to_string()
    } else {
        link.reason.clone()
    };
    let age = link
        .acked_at
        .map(|t| u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX));
    (reason, link.rtt_ms, age)
}

/// How long the link has been down, in ms.
pub fn fabric_stalled_ms() -> Option<u64> {
    let link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
    if link.up {
        return None;
    }
    link.stalled_at
        .map(|t| u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX))
}

/// The tail `status` appends after `fabric=`.
pub fn fabric_status_tail() -> String {
    let (_, rtt, age) = fabric_link_facts();
    let n = |v: Option<u64>| v.map_or_else(|| "-".to_string(), |n| n.to_string());
    format!("fabric_rtt_ms={} fabric_link_age_ms={}", n(rtt), n(age))
}

/// A bridge connection has been served.
#[cfg(any(unix, test, debug_assertions))]
pub fn bridge_attached(generation: BridgeGeneration) {
    let _transition = LINK
        .log_transition
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let mut owner = LINK.generation.lock().unwrap_or_else(|p| p.into_inner());
    if generation.0 <= *owner {
        return;
    }
    *owner = generation.0;
    {
        let mut link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
        link.up = false;
        link.reason = LINK_STARTING.to_string();
        link.rtt_ms = None;
        link.acked_at = None;
        link.stalled_at = None;
        link.reported = false;
        link.owed_since = None;
        link.hello = None;
        link.mirror = None;
        link.lane_faults = 0;
        LINK.state.store(FABRIC_STALLED, Ordering::Relaxed);
    }
    link_moved(FABRIC_STALLED, generation.0, LinkSaid::Attached);
}

/// A bridge that never reports its link is read by what it does.
pub fn link_evidence(generation: Option<BridgeGeneration>) {
    unless_another_link_section(|| {
        let owner = LINK.generation.lock().unwrap_or_else(|p| p.into_inner());
        if !generation.is_some_and(|g| g.0 == *owner) || *owner == 0 {
            return;
        }
        if LINK.state.load(Ordering::Relaxed) != FABRIC_STALLED {
            return;
        }
        let mut link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
        if link.reported || link.reason != LINK_STARTING {
            return;
        }
        link.up = true;
        link.reason.clear();
        link.rtt_ms = None;
        link.acked_at = Some(std::time::Instant::now());
        link.stalled_at = None;
        LINK.state.store(FABRIC_CONNECTED, Ordering::Relaxed);
        link_moved(FABRIC_CONNECTED, *owner, LinkSaid::Evidence);
    });
}

/// The bridge's report of its broker link.
pub fn link_report(
    store: &impl AsStore,
    generation: Option<BridgeGeneration>,
    up: bool,
    reason: &str,
    rtt_ms: Option<u64>,
) -> bool {
    let store = &store.as_store();
    let outstanding = up && posts_outstanding(store);
    let (went_down, went_up) = {
        let owner = LINK.generation.lock().unwrap_or_else(|p| p.into_inner());
        if !generation.is_some_and(|g| g.0 == *owner) || *owner == 0 {
            return false;
        }
        if LINK.state.load(Ordering::Relaxed) == FABRIC_DISCONNECTED {
            return false;
        }
        let mut link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
        let was_up = link.up;
        let was_starting = link.reason == LINK_STARTING;
        link.up = up;
        link.reported = true;
        if up {
            link.reason.clear();
            link.rtt_ms = rtt_ms;
            link.acked_at = Some(std::time::Instant::now());
            link.stalled_at = None;
            link.owed_since = outstanding.then(std::time::Instant::now);
        } else {
            link.reason = reason.to_string();
            if rtt_ms.is_some() {
                link.rtt_ms = rtt_ms;
            }
            if was_up || link.stalled_at.is_none() {
                link.stalled_at = Some(std::time::Instant::now());
            }
        }
        let stored = if up { FABRIC_CONNECTED } else { FABRIC_STALLED };
        if LINK.state.swap(stored, Ordering::Relaxed) != stored {
            link_moved(stored, *owner, LinkSaid::Report);
        }
        (
            (was_up || was_starting) && !up,
            (!was_up || was_starting) && up,
        )
    };
    if went_down {
        wake_parked(store);
    } else if went_up {
        wake_presence(store);
    }
    true
}

fn wake_presence(store: &Store) {
    let live = store.read().unwrap_or_else(|p| p.into_inner()).sessions();
    for session in &live {
        session.fabric_changed();
    }
}

/// Whether the owning bridge said in its `hello` that it offers a kept session's unhandled
/// broadcasts again after a relaunch (`refill=…say`).
pub fn bridge_refills_broadcasts() -> bool {
    unless_another_link_section(|| {
        LINK.link
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .hello
            .as_ref()
            .is_some_and(|h| h.refills_broadcasts)
    })
    .unwrap_or(false)
}

const HELLO_USAGE: &str =
    "ERR usage: hello lane=<n> [build=<token>] [refill=<lane,...>] [log=auto|turns]\n";

const BUILD_TOKEN_MAX: usize = 64;

/// `hello lane=<n> [build=<token>] [refill=<lane,...>] [log=auto|turns]`: answers
/// `OK lane=<n> build=<b>` (` log=turns` when asked), or `OK stale=1 …` to a stale incarnation.
pub fn cmd_hello(rest: &str, generation: Option<BridgeGeneration>) -> String {
    let mut lane: Option<u32> = None;
    let mut build: Option<String> = None;
    let mut refills_broadcasts = false;
    let mut log = HelloLog::Off;
    for tok in rest.split_whitespace() {
        if let Some(v) = kv(tok, "log") {
            log = match v {
                "turns" => HelloLog::Turns,
                "auto" => HelloLog::Auto,
                _ => HelloLog::Off,
            };
        } else if let Some(v) = kv(tok, "refill") {
            refills_broadcasts = v.split(',').any(|lane| lane == "say");
        } else if let Some(v) = kv(tok, "lane") {
            match parse_uint::<u32>(v) {
                Some(n) => lane = Some(n),
                None => return HELLO_USAGE.to_string(),
            }
        } else if let Some(v) = kv(tok, "build") {
            let token = !v.is_empty()
                && v.len() <= BUILD_TOKEN_MAX
                && v.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'));
            if !token {
                return HELLO_USAGE.to_string();
            }
            build = Some(v.to_string());
        } else if !tok.split_once('=').is_some_and(|(k, _)| !k.is_empty()) {
            return HELLO_USAGE.to_string();
        }
    }
    let Some(lane) = lane else {
        return HELLO_USAGE.to_string();
    };
    let own = aterm_stamp::build_id();
    let answer = format!(
        "lane={} build={own}",
        aterm_types::control_verbs::BRIDGE_LANE
    );
    let recorded = {
        let owner = LINK.generation.lock().unwrap_or_else(|p| p.into_inner());
        if !generation.is_some_and(|g| g.0 == *owner) || *owner == 0 {
            false
        } else {
            let mut link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
            link.hello = Some(BridgeHello {
                lane,
                build: build.clone(),
                refills_broadcasts,
                log,
            });
            true
        }
    };
    if !recorded {
        return format!("OK stale=1 {answer}\n");
    }
    if build.as_deref() != Some(own.as_str()) {
        aterm_log::warn!(
            "fabric bridge is build {} under this instance's {own} (lane {lane}, this instance's \
             {}): [fabric] command names another aterm than this one",
            build.as_deref().unwrap_or("-"),
            aterm_types::control_verbs::BRIDGE_LANE
        );
    }
    if log == HelloLog::Turns {
        format!("OK {answer} log=turns\n")
    } else {
        format!("OK {answer}\n")
    }
}

/// Whether `generation` is the bridge incarnation that owns the link.
#[must_use]
pub fn lane_owns(generation: Option<BridgeGeneration>) -> bool {
    let owner = LINK.generation.lock().unwrap_or_else(|p| p.into_inner());
    *owner != 0 && generation.is_some_and(|g| g.0 == *owner)
}

/// Run an owning bridge's operation while attach, log replacement and registration cannot cross it.
pub fn with_current_bridge<T>(
    generation: Option<BridgeGeneration>,
    operation: impl FnOnce() -> T,
) -> Option<T> {
    let _transition = LINK
        .log_transition
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    lane_owns(generation).then(operation)
}

/// Whether the owning bridge asked for the bus mirror in its recorded `hello` (`log=turns`).
#[must_use]
pub fn bridge_mirrors_logs() -> bool {
    unless_another_link_section(|| {
        LINK.link
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .hello
            .as_ref()
            .is_some_and(|h| h.log == HelloLog::Turns)
    })
    .unwrap_or(false)
}

/// Whether the node mirrors, as the owning bridge last said it; `None` for an `auto` bridge whose
/// mirror has not reported `unretained`. aterm-gui's `session_log_mirror::Shared::served` keeps
/// the word.
#[must_use]
pub fn bridge_mirror_word() -> Option<bool> {
    unless_another_link_section(|| {
        let link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
        match link.hello.as_ref().map(|h| h.log) {
            Some(HelloLog::Turns) => Some(true),
            Some(HelloLog::Auto) => link
                .mirror
                .as_ref()
                .is_some_and(|m| m.state == MirrorState::Unretained)
                .then_some(false),
            Some(HelloLog::Off) | None => Some(false),
        }
    })
    .unwrap_or(Some(false))
}

/// `fabric status`'s bus-mirror tail.
#[must_use]
pub fn fabric_mirror_facts() -> String {
    let link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
    let word = match link.hello.as_ref().map(|h| h.log) {
        Some(HelloLog::Turns) => "turns",
        Some(HelloLog::Auto) => "auto",
        Some(HelloLog::Off) | None => return String::new(),
    };
    let state = link.mirror.as_ref().map_or_else(
        || MirrorState::Starting.word().to_string(),
        |m| {
            if m.reason.is_empty() {
                m.state.word().to_string()
            } else {
                format!("{}:{}", m.state.word(), m.reason)
            }
        },
    );
    format!(" bridge_log={word} mirror={state}")
}

/// The words of `link mirror <state> [reason=<token>]` after `mirror`: the report the endpoint
/// keeps, or `None`, which it answers with [`LINK_USAGE`].
fn parse_link_mirror<'a>(mut toks: impl Iterator<Item = &'a str>) -> Option<MirrorReport> {
    let state = toks.next().and_then(MirrorState::reported)?;
    let mut reason = String::new();
    for tok in toks {
        reason = link_reason_token(kv(tok, "reason")?);
    }
    Some(MirrorReport { state, reason })
}

fn cmd_link_mirror<'a>(
    toks: impl Iterator<Item = &'a str>,
    generation: Option<BridgeGeneration>,
) -> String {
    let Some(report) = parse_link_mirror(toks) else {
        return LINK_USAGE.to_string();
    };
    with_current_bridge(generation, || {
        LINK.link.lock().unwrap_or_else(|p| p.into_inner()).mirror = Some(report);
        "OK\n".to_string()
    })
    .unwrap_or_else(|| "OK stale=1\n".to_string())
}

fn cmd_link_set_aside<'a>(
    store: &Store,
    mut toks: impl Iterator<Item = &'a str>,
    generation: Option<BridgeGeneration>,
) -> String {
    let files = toks
        .next()
        .and_then(|tok| kv(tok, "files"))
        .and_then(parse_uint::<u64>)
        .filter(|n| *n > 0);
    let (Some(files), None) = (files, toks.next()) else {
        return LINK_USAGE.to_string();
    };
    with_current_bridge(generation, || {
        let told_when_empty = LINK
            .link
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .hello
            .as_ref()
            .is_some_and(|h| h.lane >= aterm_types::control_verbs::LINK_QUARANTINE_LANE);
        store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .torn_state_set_aside(files, told_when_empty);
        "OK\n".to_string()
    })
    .unwrap_or_else(|| "OK stale=1\n".to_string())
}

fn cmd_link_quarantine<'a>(
    store: &Store,
    mut toks: impl Iterator<Item = &'a str>,
    generation: Option<BridgeGeneration>,
) -> String {
    let torn = toks
        .next()
        .and_then(|tok| kv(tok, "torn"))
        .and_then(parse_uint::<u64>);
    let (Some(torn), None) = (torn, toks.next()) else {
        return LINK_USAGE.to_string();
    };
    with_current_bridge(generation, || {
        store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .torn_state_held(torn);
        "OK\n".to_string()
    })
    .unwrap_or_else(|| "OK stale=1\n".to_string())
}

/// `link gap from=<a> to=<b> [records=<n>] [sid=<sid> [topic=<t>]]` (bridge lanes
/// [`LINK_GAP_LANE`] and, for `sid=`, [`LINK_GAP_SCOPED_LANE`]): the broker's retention gave up
/// records at offsets `a..b`, `n` of them where the broker counted.
///
/// - With no `sid=`, this node's unread mail before the bridge read it. The subjects went with
///   the records, so every session this node serves is told: its `inbox` header names the range
///   and count (`given_up=`, `given_up_count=`) and its log marks it (`mail-gap`), once per
///   range, a re-tell of the same range being a repeat.
/// - With `sid=`, the bridge's own read of that session's lane met the floor (its refill from
///   the session's `seen`, its deadline sweep), so that session alone is told: its header names
///   the range in a field of its own (`lane_given_up=`) and its log marks it (`mail-gap`), once
///   per range. The node's range and the lane's are kept apart, so a re-tell of either after the
///   other was told is still a repeat. Nothing is told when the ring still holds mail of the
///   range: then the bridge read behind what this aterm was delivered, and the ring has what the
///   bus let go of (or counted it in `dropped=`).
/// - With `sid=` and `topic=`, the range is of the broadcast topic the bridge replays to that
///   session: its `topic ls` row names it (`given_up=`) and its log marks it (`mail-gap` with
///   the topic), unless the session no longer holds the topic or its ring holds a broadcast of
///   the topic in the range.
///
/// [`LINK_GAP_LANE`]: aterm_types::control_verbs::LINK_GAP_LANE
/// [`LINK_GAP_SCOPED_LANE`]: aterm_types::control_verbs::LINK_GAP_SCOPED_LANE
fn cmd_link_gap<'a>(
    store: &Store,
    toks: impl Iterator<Item = &'a str>,
    generation: Option<BridgeGeneration>,
    wake: &dyn Fn(&SessionId),
) -> String {
    let (mut from, mut to, mut records) = (None, None, None);
    let (mut sid, mut topic) = (None, None);
    for tok in toks {
        if let Some(v) = kv(tok, "sid") {
            if sid.replace(v).is_some() || v.is_empty() {
                return LINK_USAGE.to_string();
            }
            continue;
        }
        if let Some(v) = kv(tok, "topic") {
            if topic.replace(v).is_some() || !is_topic(v) {
                return LINK_USAGE.to_string();
            }
            continue;
        }
        let slot = if let Some(v) = kv(tok, "from") {
            (&mut from, v)
        } else if let Some(v) = kv(tok, "to") {
            (&mut to, v)
        } else if let Some(v) = kv(tok, "records") {
            (&mut records, v)
        } else {
            return LINK_USAGE.to_string();
        };
        match (slot.0.is_none(), parse_uint::<u64>(slot.1)) {
            (true, Some(n)) => *slot.0 = Some(n),
            _ => return LINK_USAGE.to_string(),
        }
    }
    let (Some(from), Some(to)) = (from, to) else {
        return LINK_USAGE.to_string();
    };
    if from > to || (topic.is_some() && sid.is_none()) {
        return LINK_USAGE.to_string();
    }
    let gap = GivenUp { from, to, records };
    let scope = match (sid, topic) {
        (None, _) => GapScope::Node,
        (Some(_), None) => GapScope::Lane,
        (Some(_), Some(t)) => GapScope::Topic(t),
    };
    let owed = with_current_bridge(generation, || {
        if matches!(scope, GapScope::Node) {
            LINK.node_gaps
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .note(gap);
        }
        let told: Vec<_> = {
            let g = store.read().unwrap_or_else(|p| p.into_inner());
            match sid {
                Some(sid) => g
                    .session(&SessionId::new(sid.to_string()))
                    .into_iter()
                    .collect(),
                None => g.sessions(),
            }
        };
        told.into_iter()
            .filter(|ctx| note_given_up(ctx.as_ref(), gap, scope))
            .map(|ctx| ctx.self_id().clone())
            .collect::<Vec<_>>()
    });
    let Some(owed) = owed else {
        return "OK stale=1\n".to_string();
    };
    for sid in &owed {
        wake(sid);
    }
    "OK\n".to_string()
}

/// What `link gap` told this instance of the node's unread mail (no `sid=`: astream's gap notice
/// on the node's inbox group, 6E-1d) since the instance started: how many ranges, and the newest
/// with the broker's count while its offsets name the bridge's log. `fabric status` names it
/// ([`fabric_gap_facts`]), so `aterm fabric` lists the node's loss under WARNINGS even when no
/// session was there to be told, or the sessions told have gone. A new bridge resets
/// `lane_faults` and not this: what retention gave up stays gone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct NodeGaps {
    /// Ranges told, a range told again over one it overlaps counted once ([`Self::note`]).
    ranges: u64,
    /// The newest range, while its offsets name the bridge's log: the log it was told on, or one
    /// continuing it past the range's end ([`Self::log_moved`]).
    newest: Option<GivenUp>,
}

impl NodeGaps {
    /// Count `gap`. A range that overlaps the newest is that range told again, as a new
    /// subscription from a cursor below its end tells it (astream counts a range's records once
    /// per stream, so the new stream's count is of the whole of its range), on the log it was
    /// told on or on one continuing it past its end: the newest widens to cover both and keeps
    /// the count of whichever range covers the other (the newer's when they are one range and it
    /// has one), none when neither does. An empty range is no loss.
    fn note(&mut self, gap: GivenUp) {
        if gap.from >= gap.to {
            return;
        }
        if let Some(told) = self.newest
            && gap.from < told.to
            && told.from < gap.to
        {
            let (from, to) = (gap.from.min(told.from), gap.to.max(told.to));
            let records = match (
                (gap.from, gap.to) == (from, to),
                (told.from, told.to) == (from, to),
            ) {
                (true, true) => gap.records.or(told.records),
                (true, false) => gap.records,
                (false, true) => told.records,
                (false, false) => None,
            };
            self.newest = Some(GivenUp { from, to, records });
            return;
        }
        self.ranges = self.ranges.saturating_add(1);
        self.newest = Some(gap);
    }

    /// The bridge's log moved from `was` to `clock` ([`cmd_link_log`]). The newest range stays
    /// where `clock` continues `was` past its end, exactly as each session's `given_up=` does
    /// ([`Inbox::forget_bus_offsets`]), so that range told again on the new log is a repeat to
    /// both; any other move forgets it, as its offsets name records of a log since replaced, and
    /// keeps the count.
    fn log_moved(&mut self, was: Option<LogIdentity>, clock: LogClock) {
        if was == Some(clock.ident) {
            return;
        }
        let fork = clock.keep_below(was);
        self.newest = self
            .newest
            .filter(|gap| fork.is_some_and(|fork| gap.to <= fork));
    }
}

/// The words `fabric status` adds for [`NodeGaps`] once a range was told: ` mail_gaps=<k>`, then
/// the newest range while its offsets name the bridge's log (the log it was told on, or one
/// continuing it past the range's end): ` mail_gap=@<a>..@<b>`, and ` mail_gap_count=<n>` where
/// the broker counted its records.
#[must_use]
pub fn fabric_gap_facts() -> String {
    use std::fmt::Write as _;
    let gaps = *LINK.node_gaps.lock().unwrap_or_else(|p| p.into_inner());
    if gaps.ranges == 0 {
        return String::new();
    }
    let mut out = format!(" mail_gaps={}", gaps.ranges);
    if let Some(gap) = gaps.newest {
        let _ = write!(out, " mail_gap=@{}..@{}", gap.from, gap.to);
        if let Some(n) = gap.records {
            let _ = write!(out, " mail_gap_count={n}");
        }
    }
    out
}

/// Whose records a `link gap` names ([`cmd_link_gap`]).
#[derive(Clone, Copy)]
enum GapScope<'a> {
    /// The node's unread mail, whose sessions went with it.
    Node,
    /// The told session's mail lane.
    Lane,
    /// The told session's broadcast topic.
    Topic(&'a str),
}

/// Whether `inbox`'s ring, on the current log, holds a row of `topic` (`None`: mail) at an
/// offset in `gap`.
fn ring_holds(inbox: &Inbox, topic: Option<&str>, gap: GivenUp) -> bool {
    inbox
        .rows
        .iter()
        .any(|r| r.on_log && r.topic.as_deref() == topic && (gap.from..gap.to).contains(&r.off))
}

/// Tell `ctx` the records of `scope` at `gap` were given up ([`cmd_link_gap`]); returns whether
/// its log's readers were owed a wake. A repeat of the newest range told of the same scope (the
/// node, the lane, or the one topic) is not marked again, nor is a lane's or topic's range its
/// ring still holds mail of, nor a topic it no longer holds.
fn note_given_up(ctx: &dyn FabricSession, gap: GivenUp, scope: GapScope<'_>) -> bool {
    let mut inbox = ctx.fabric().lock();
    let fact = match scope {
        GapScope::Node | GapScope::Lane => {
            if matches!(scope, GapScope::Lane) && ring_holds(&inbox, None, gap) {
                return false;
            }
            let told = if matches!(scope, GapScope::Node) {
                &mut inbox.given_up
            } else {
                &mut inbox.lane_given_up
            };
            if told.is_some_and(|told| told.repeated_by(&gap)) {
                return false;
            }
            *told = Some(gap);
            FabricFact::MailGap {
                from: gap.from,
                to: gap.to,
                records: gap.records,
            }
        }
        GapScope::Topic(topic) => {
            if !ctx.fabric().opted_into(topic) || ring_holds(&inbox, Some(topic), gap) {
                return false;
            }
            if inbox
                .topic_given_up
                .get(topic)
                .is_some_and(|told| told.repeated_by(&gap))
            {
                return false;
            }
            inbox.topic_given_up.insert(topic.to_string(), gap);
            FabricFact::TopicGap {
                from: gap.from,
                to: gap.to,
                topic,
            }
        }
    };
    let owed = ctx.record_fact("inbox-gap", fact);
    ctx.fabric().changed.ring(inbox);
    ctx.fabric_changed();
    owed
}

/// The endpoint answered one of `generation`'s bridge-only lines `ERR usage`.
pub fn note_lane_fault(generation: Option<BridgeGeneration>) {
    unless_another_link_section(|| {
        let owner = LINK.generation.lock().unwrap_or_else(|p| p.into_inner());
        if !generation.is_some_and(|g| g.0 == *owner) || *owner == 0 {
            return;
        }
        let mut link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
        link.lane_faults = link.lane_faults.saturating_add(1);
    });
}

/// What the attached bridge said of itself and how its lane has gone, as `fabric status` prints it.
pub fn fabric_bridge_facts() -> (Option<u32>, Option<String>, u64) {
    let link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
    let (lane, build) = link
        .hello
        .as_ref()
        .map_or((None, None), |h| (Some(h.lane), h.build.clone()));
    (lane, build, link.lane_faults)
}

const LINK_USAGE: &str = "ERR usage: link up rtt=<ms> | link down reason=<token> [rtt=<ms>] | \
                          link log <identity> [continues=<off> was=<identity>] | \
                          link bye <quit|handoff> | \
                          link mirror on|stopped|capped|budget|unretained [reason=<token>] | \
                          link set-aside files=<n> | \
                          link quarantine torn=<k> | \
                          link gap from=<off> to=<off> [records=<n>] [sid=<sid> [topic=<t>]]\n";

static BYES_TAKEN: Mutex<u64> = Mutex::new(0);

static BYE_TAKEN: Condvar = Condvar::new();

/// The [`BYES_TAKEN`] count now.
#[must_use]
pub fn byes_taken() -> u64 {
    *BYES_TAKEN.lock().unwrap_or_else(|p| p.into_inner())
}

/// Wait at most `wait` for the bridge to take a `bye` past `before`.
#[must_use]
pub fn await_bye_taken(before: u64, wait: std::time::Duration) -> bool {
    let taken = BYES_TAKEN.lock().unwrap_or_else(|p| p.into_inner());
    let (taken, _) = BYE_TAKEN
        .wait_timeout_while(taken, wait, |n| *n <= before)
        .unwrap_or_else(|p| p.into_inner());
    *taken > before
}

/// Whether a bridge incarnation owns the link right now.
#[must_use]
pub fn bridge_attached_now() -> bool {
    matches!(
        LINK.state.load(Ordering::Relaxed),
        FABRIC_CONNECTED | FABRIC_STALLED
    )
}

/// WHETHER A QUIT'S CLOSES CAN LAND ON THE BUS NOW (fix unit Q1): a bridge owns the link, its
/// recorded `hello` asked for the mirror, the link is not futile (a broker the bridge reported
/// unreachable, a stale or absent link), and the mirror runs or is starting (not stopped, capped,
/// held by the broker's window, or unretained). A quit waits for its closes' landings only then,
/// and never past its budget.
#[must_use]
pub fn mirror_lands_now() -> bool {
    if !bridge_mirrors_logs() || wait_is_futile() {
        return false;
    }
    LINK.link
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .mirror
        .as_ref()
        .is_none_or(|m| matches!(m.state, MirrorState::Starting | MirrorState::On))
}

fn note_bye_taken(generation: Option<BridgeGeneration>) -> bool {
    let owner = LINK.generation.lock().unwrap_or_else(|p| p.into_inner());
    if !generation.is_some_and(|g| g.0 == *owner) || *owner == 0 {
        return false;
    }
    drop(owner);
    *BYES_TAKEN.lock().unwrap_or_else(|p| p.into_inner()) += 1;
    BYE_TAKEN.notify_all();
    true
}

/// `link up rtt=<ms>` / `link down reason=<token> [rtt=<ms>]` / `link log <identity>` /
/// `link bye <quit|handoff>` / `link mirror …` / `link set-aside files=<n>` /
/// `link quarantine torn=<k>` / `link gap from=<a> to=<b> [records=<n>] [sid=<sid> [topic=<t>]]`,
/// from the bridge lane of `generation`.
pub fn cmd_link(store: &impl AsStore, rest: &str, generation: Option<BridgeGeneration>) -> String {
    cmd_link_after_log_switch(store, rest, generation, || {})
}

/// [`cmd_link`], calling `on_forgot` (the endpoint's log-clock retirement) on a completed
/// `forgot=1` switch, before the transition is released.
pub fn cmd_link_after_log_switch(
    store: &impl AsStore,
    rest: &str,
    generation: Option<BridgeGeneration>,
    on_forgot: impl FnOnce(),
) -> String {
    cmd_link_waking(store, rest, generation, on_forgot, &|_| {})
}

/// [`cmd_link_after_log_switch`], waking each session's log readers through `wake` when a
/// `link gap` marked its log.
pub fn cmd_link_waking(
    store: &impl AsStore,
    rest: &str,
    generation: Option<BridgeGeneration>,
    on_forgot: impl FnOnce(),
    wake: &dyn Fn(&SessionId),
) -> String {
    let store = &store.as_store();
    let mut toks = rest.split_whitespace();
    let up = match toks.next() {
        Some("up") => true,
        Some("down") => false,
        Some("log") => {
            let ident = toks.next();
            let (mut fork, mut was) = (None, None);
            for tok in toks {
                if let Some(v) = kv(tok, "continues")
                    && fork.is_none()
                {
                    match parse_uint::<u64>(v) {
                        Some(n) => fork = Some(n),
                        None => return LINK_USAGE.to_string(),
                    }
                } else if let Some(v) = kv(tok, "was")
                    && was.is_none()
                {
                    was = Some(v);
                } else {
                    return LINK_USAGE.to_string();
                }
            }
            let continues = match (was, fork) {
                (None, None) => None,
                (Some(was), Some(fork)) => Some((was, fork)),
                _ => return LINK_USAGE.to_string(),
            };
            return cmd_link_log(store, ident, continues, generation, on_forgot);
        }
        Some("bye") => {
            let word = toks
                .next()
                .and_then(aterm_types::fabric_grammar::Bye::parse);
            return match (word, toks.next()) {
                (Some(_), None) if note_bye_taken(generation) => "OK\n".to_string(),
                (Some(_), None) => "OK stale=1\n".to_string(),
                _ => LINK_USAGE.to_string(),
            };
        }
        Some("mirror") => return cmd_link_mirror(toks, generation),
        Some("set-aside") => return cmd_link_set_aside(store, toks, generation),
        Some("quarantine") => return cmd_link_quarantine(store, toks, generation),
        Some("gap") => return cmd_link_gap(store, toks, generation, wake),
        _ => return LINK_USAGE.to_string(),
    };
    let mut rtt: Option<u64> = None;
    let mut reason: Option<String> = None;
    for tok in toks {
        if let Some(v) = kv(tok, "rtt") {
            match parse_uint::<u64>(v) {
                Some(n) => rtt = Some(n),
                None => return LINK_USAGE.to_string(),
            }
        } else if let Some(v) = kv(tok, "reason") {
            reason = Some(link_reason_token(v));
        } else {
            return LINK_USAGE.to_string();
        }
    }
    if up && (rtt.is_none() || reason.is_some()) {
        return LINK_USAGE.to_string();
    }
    let reason = match (up, reason) {
        (true, _) => String::new(),
        (false, Some(r)) => r,
        (false, None) => return LINK_USAGE.to_string(),
    };
    if link_report(store, generation, up, &reason, rtt) {
        "OK\n".to_string()
    } else {
        "OK stale=1\n".to_string()
    }
}

fn cmd_link_log(
    store: &Store,
    ident: Option<&str>,
    continues: Option<(&str, u64)>,
    generation: Option<BridgeGeneration>,
    on_forgot: impl FnOnce(),
) -> String {
    // Both identities are read as astream reads them (`LogIdentity::parse`), and compared as
    // values: `<id>.01` is the clock `<id>.1`. A move is taken only from an earlier
    // generation of the same log, the one pair a lineage can carry.
    let Some(ident) = ident.and_then(LogIdentity::parse) else {
        return LINK_USAGE.to_string();
    };
    let continues = match continues {
        None => None,
        Some((was, fork)) => match LogIdentity::parse(was).filter(|w| ident.continues_from(*w)) {
            Some(was) => Some((was, fork)),
            None => return LINK_USAGE.to_string(),
        },
    };
    let _transition = LINK
        .log_transition
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let previous = {
        let owner = LINK.generation.lock().unwrap_or_else(|p| p.into_inner());
        if !generation.is_some_and(|g| g.0 == *owner) || *owner == 0 {
            return "OK stale=1\n".to_string();
        }
        *LINK.log.lock().unwrap_or_else(|p| p.into_inner())
    };
    let mut changed = previous.is_some_and(|was| was.ident != ident);
    let clock = LogClock {
        ident,
        continues: continues.or_else(|| {
            previous
                .filter(|p| p.ident == ident)
                .and_then(|p| p.continues)
        }),
    };
    let live: Vec<_> = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.sessions()
    };
    for ctx in &live {
        let mut inbox = ctx.fabric().lock();
        if inbox.bus_log == Some(ident) {
            continue;
        }
        let forget = inbox.bus_log.map_or_else(
            || changed || inbox.holds_bus_references(),
            |was| was != ident,
        );
        if forget {
            let wait = ctx.fabric().fence_wait();
            let Some(guard) = ctx.fabric().wait_unfenced_retirement(inbox, wait) else {
                return HOLD_BUSY.to_string();
            };
            inbox = guard;
            let keep_below = clock.keep_below(inbox.bus_log);
            let retired = inbox.forget_bus_offsets(keep_below);
            for id in retired {
                let fact = FabricFact::MailLanded {
                    post: id,
                    off: None,
                    why: Some(DeadReason::LogReplaced),
                };
                let _ = ctx.record_fact("post-landed", fact);
            }
            changed = true;
        }
        inbox.bus_log = Some(ident);
        ctx.fabric().changed.ring(inbox);
    }
    LINK.node_gaps
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .log_moved(previous.map(|p| p.ident), clock);
    *LINK.log.lock().unwrap_or_else(|p| p.into_inner()) = Some(clock);
    if changed {
        on_forgot();
        "OK forgot=1\n"
    } else {
        "OK\n"
    }
    .to_string()
}

fn link_reason_token(raw: &str) -> String {
    let out: String = raw
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .take(LINK_REASON_MAX)
        .collect();
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

fn wake_parked(store: &Store) {
    let parked: Vec<_> = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.sessions()
    };
    for ctx in &parked {
        let guard = ctx.fabric().lock();
        ctx.fabric_changed();
        ctx.fabric().changed.ring(guard);
    }
}

fn note_bridge_touched(store: &Store, sid: &str) {
    let key = registry_key(store);
    let over = {
        let mut touched = LINK.touched.lock().unwrap_or_else(|p| p.into_inner());
        let slot = touched.entry(key).or_insert_with(|| TouchedRegistry {
            registry: Arc::downgrade(store),
            sids: BTreeSet::new(),
        });
        debug_assert!(
            slot.registry.upgrade().is_some(),
            "a live registry's key resolved to a dead registry's slot"
        );
        slot.sids.insert(sid.to_string());
        touched.values().map(|r| r.sids.len()).sum::<usize>() > TOUCHED_PRUNE_AT
    };
    if !over {
        return;
    }
    let live: BTreeSet<String> = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.sessions()
            .iter()
            .map(|s| s.self_id().as_str().to_string())
            .collect()
    };
    let mut touched = LINK.touched.lock().unwrap_or_else(|p| p.into_inner());
    touched.retain(|k, r| {
        if r.registry.upgrade().is_none() {
            return false;
        }
        if *k == key {
            r.sids.retain(|s| live.contains(s));
        }
        !r.sids.is_empty()
    });
}

/// The bridge is gone.
#[cfg(any(unix, test, debug_assertions))]
pub fn bridge_lost(store: &impl AsStore, generation: BridgeGeneration) -> usize {
    bridge_lost_waking(store, generation, &|_| {})
}

/// [`bridge_lost`], calling `wake` with the id of each session whose log owes its readers a wake
/// (the halt's `hold` mark), once its fabric lock is released.
///
/// A generation older than the link's owner is a dead one's late loss: its supervisor said its
/// loss at its exit and relaunched, and the newer bridge serves the governed sessions. Its sweep
/// sets no hold and no advisory, judged under each session's lock.
#[cfg(any(unix, test, debug_assertions))]
pub fn bridge_lost_waking(
    store: &impl AsStore,
    generation: BridgeGeneration,
    wake: &dyn Fn(&SessionId),
) -> usize {
    let store = &store.as_store();
    {
        let owner = LINK.generation.lock().unwrap_or_else(|p| p.into_inner());
        if *owner == generation.0 {
            let mut link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
            link.up = false;
            link.reason = LINK_BRIDGE_LOST.to_string();
            link.stalled_at = None;
            let was = LINK.state.swap(FABRIC_DISCONNECTED, Ordering::Relaxed);
            drop(link);
            if was != FABRIC_DISCONNECTED {
                link_moved(FABRIC_DISCONNECTED, generation.0, LinkSaid::Lost);
            }
        }
    }
    let sids: Vec<String> = LINK
        .touched
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .flat_map(|r| r.sids.iter().cloned())
        .collect();
    let halts = holds_on_bridge_lost();
    let mut held = 0usize;
    let mut advised = 0usize;
    for sid in sids {
        let ctx = {
            let g = store.read().unwrap_or_else(|p| p.into_inner());
            g.session(&SessionId::new(sid.clone()))
        };
        if let Some(ctx) = ctx {
            let lost = Hold {
                reason: FABRIC_LOST.to_string(),
                origin: "fleet".to_string(),
            };
            if halts {
                let act = HoldAct::Lost(lost, generation);
                apply_hold(&ctx, act, HoldIssuer::Bridge, None, &|| {
                    wake(ctx.self_id());
                });
            } else if advise(&ctx, lost, generation) {
                advised += 1;
            }
            held += 1;
        }
    }
    if advised > 0 {
        store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .bridge_lost_advised(advised);
    }
    wake_parked(store);
    held
}

/// `[fabric] hold_on_bridge_lost`.
#[cfg(any(unix, test, debug_assertions))]
pub fn set_hold_on_bridge_lost(on: bool) {
    LINK.hold_on_loss.store(on, Ordering::Relaxed);
}

/// Whether a lost bridge halts its sessions ([`set_hold_on_bridge_lost`]; `false` by default).
#[cfg(any(unix, test, debug_assertions))]
pub fn holds_on_bridge_lost() -> bool {
    LINK.hold_on_loss.load(Ordering::Relaxed)
}

/// Whether a newer generation than `generation` owns the link: `generation`'s loss is a dead
/// bridge's, said late.
#[cfg(any(unix, test, debug_assertions))]
fn superseded(generation: BridgeGeneration) -> bool {
    generation.0 < *LINK.generation.lock().unwrap_or_else(|p| p.into_inner())
}

/// Whether the lane of bridge generation `generation` may still move a session's hold: no newer
/// generation owns the link, and the owner's loss, when it is `generation`, has not been said
/// ([`bridge_lost`] marks it before its sweep takes any session's lock). A generation newer than
/// the owner, whose attach has not landed, is no lost one.
fn lane_governs(generation: BridgeGeneration) -> bool {
    let owner = LINK.generation.lock().unwrap_or_else(|p| p.into_inner());
    generation.0 > *owner
        || (generation.0 == *owner && LINK.state.load(Ordering::Relaxed) != FABRIC_DISCONNECTED)
}

/// Advise `ctx` of `generation`'s loss: once, however many of that generation's lanes and its
/// supervisor say it, and never past a newer attach ([`superseded`]).
#[cfg(any(unix, test, debug_assertions))]
fn advise(ctx: &dyn FabricSession, lost: Hold, generation: BridgeGeneration) -> bool {
    let mut inbox = ctx.fabric().lock();
    if inbox.holds.is_held() || superseded(generation) {
        return false;
    }
    if inbox
        .advisory
        .as_ref()
        .is_some_and(|(hold, under)| *under == generation.0 && *hold == lost)
    {
        return false;
    }
    let fact = FabricFact::Advisory {
        origin: &lost.origin,
        reason: &lost.reason,
        generation: generation.0,
    };
    let _ = ctx.record_fact("hold-advisory", fact);
    inbox.advisory = Some((lost, generation.0));
    ctx.fabric().changed.ring(inbox);
    ctx.fabric_changed();
    true
}

/// Whether a standing hold, of any origin, refuses `verb`: whether its input reaches the PTY
/// ([`aterm_types::control_verbs::Input::PTY`]).
#[must_use]
pub fn is_pty_reaching(verb: &str) -> bool {
    aterm_types::control_verbs::input_of(verb).contains(aterm_types::control_verbs::Input::PTY)
}

/// The refusal a halted session answers `verb` with, or `None` when the verb may proceed
/// ([`is_pty_reaching`], then [`hold_refusal`]).
#[must_use]
pub fn halt_refusal(ctx: &dyn FabricSession, verb: &str) -> Option<String> {
    if !is_pty_reaching(verb) {
        return None;
    }
    hold_refusal(ctx)
}

/// The refusal `ctx`'s standing hold answers a verb the halt binds, or `None` when no hold stands.
#[must_use]
pub fn hold_refusal(ctx: &dyn FabricSession) -> Option<String> {
    let (refusal, origin) = standing_refusal(ctx)?;
    if origin == PERSON_ORIGIN {
        ctx.fabric().person_refusals.fetch_add(1, Ordering::Relaxed);
    }
    Some(refusal)
}

fn standing_refusal(ctx: &dyn FabricSession) -> Option<(String, String)> {
    let driver = serving_driver().map(|driver| driver.node);
    let holds = ctx.fabric().holds();
    let hold = holds.first_against(driver.as_deref())?;
    Some((halted(ctx, &holds, &hold, None), hold.origin))
}

/// What lifts the remote hold of `node` (shown as `host`) on session `sid`. Peer removal
/// drops named grants locally; signed membership can return through another member's book.
fn remote_lifts(sid: &str, host: &str, node: &str) -> String {
    let off = format!("`aterm ctl --host <this session's Mac> hold {sid} off` run on {host}");
    format!(
        "{off}, or the person at this session's Mac (Take Over then Hand Back, or `aterm fleet \
         allow content` there), lifts it; `aterm fleet peers rm {node}` on this session's Mac \
         also drops grants naming that node, but an all-member grant can admit a valid signed entry \
         merged back from another member's book"
    )
}

/// The `ERR halted` line of `hold`, one of `holds`: a remote one names who holds it and what
/// lifts it, by what a local `hold off` found of its node where one met it (`found`).
fn halted(ctx: &dyn FabricSession, holds: &Holds, hold: &Hold, found: Option<&Holder>) -> String {
    let lifts = match holds.remote().filter(|_| hold.origin == REMOTE_ORIGIN) {
        Some(remote) => {
            let (host, sid) = (&remote.host, ctx.self_id().as_str());
            // The holder's name already carries its node's head (`<host> (node <head>)`).
            let node = &remote.node;
            let lifts = remote_lifts(sid, host, node);
            match found {
                Some(Holder::Drives) => format!(
                    " (held by {host} over the fleet, which still drives this session's Mac: \
                     {lifts})"
                ),
                Some(Holder::Again(why)) => format!(
                    " (held by {host} over the fleet, which may still drive this session's Mac: \
                     its place did not prove it just now ({why}), and its drives are its own dials \
                     of that Mac; {lifts})"
                ),
                Some(Holder::Gone(_)) | None => format!(
                    " (held by {host} over the fleet: {lifts}; so does `aterm ctl hold {sid} off` \
                     on this session's Mac once that node may drive it no more)"
                ),
            }
        }
        None => String::new(),
    };
    format!(
        "ERR halted reason={} origin={}{lifts}\n",
        hold.reason, hold.origin
    )
}

/// [`halt_refusal`] for an app-target verb, which has no session to check.
pub fn app_halt_refusal(store: &impl AsStore, verb: &str) -> Option<String> {
    if !is_pty_reaching(verb) {
        return None;
    }
    any_hold_refusal(store)
}

/// The refusal of the first session on this instance a hold stands on, or `None`.
pub fn any_hold_refusal(store: &impl AsStore) -> Option<String> {
    let store = &store.as_store();
    let ctxs: Vec<_> = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.sessions()
    };
    ctxs.iter()
        .find_map(|ctx| standing_refusal(ctx.as_ref()))
        .map(|(refusal, _)| refusal)
}

/// `open` of a gateway card under any hold.
pub fn open_halt_refusal(store: &impl AsStore, verb: &str, rest: &str) -> Option<String> {
    use aterm_types::control_verbs::AuxTarget;
    if verb != "open" {
        return None;
    }
    let mut words = rest.split_whitespace();
    let target = AuxTarget::parse(words.next()?)?;
    if words.next() == Some("close")
        || !matches!(
            target,
            AuxTarget::Menu | AuxTarget::TabMenu | AuxTarget::Overview | AuxTarget::FactMenu
        )
    {
        return None;
    }
    app_halt_refusal(store, "invoke")
}

/// A principal address: a bare principal ([`is_principal`]), or `<owner>@<node>` for a session
/// behind a node, both halves bare.
pub fn valid_principal(p: &str) -> bool {
    match p.split_once('@') {
        Some((owner, node)) => is_principal(owner) && is_principal(node),
        None => is_principal(p),
    }
}

/// The most bytes a `post key=` may carry.
pub const KEY_MAX: usize = 64;

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= KEY_MAX
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn kv<'a>(tok: &'a str, key: &str) -> Option<&'a str> {
    tok.strip_prefix(key)
        .and_then(|rest| rest.strip_prefix('='))
}

fn reason_token(raw: &str) -> String {
    let mut kept: Vec<u8> = raw.trim().bytes().filter(u8::is_ascii_graphic).collect();
    let cut = match kept.as_slice() {
        [.., b'%'] => 1,
        [.., b'%', d] if d.is_ascii_hexdigit() => 2,
        _ => 0,
    };
    kept.truncate(kept.len() - cut);
    let hex = |i: usize| kept.get(i).is_some_and(u8::is_ascii_hexdigit);
    let mut out = String::with_capacity(REASON_MAX);
    let mut i = 0;
    while i < kept.len() {
        let (piece, step): (&[u8], usize) = match kept[i] {
            b'%' if hex(i + 1) && hex(i + 2) => (&kept[i..i + 3], 3),
            b'%' => (b"%25", 1),
            _ => (&kept[i..=i], 1),
        };
        if out.len() + piece.len() > REASON_MAX {
            break;
        }
        out.extend(piece.iter().map(|&b| char::from(b)));
        i += step;
    }
    if out.is_empty() { "-".to_string() } else { out }
}

const HOLD_USAGE: &str = "ERR usage: hold <sid> on|off [reason=<pct>] [origin=fleet|local]\n";

/// The usage a drive connection gets: it holds as its own node and names no `origin=`.
const HOLD_REMOTE_USAGE: &str = "ERR usage: hold <sid> on|off [reason=<pct>] (a drive connection \
                                 holds as its own node, so it takes no origin=)\n";

/// The usage `hold` answers `issuer` with.
#[must_use]
pub fn hold_usage(issuer: HoldIssuer) -> &'static str {
    if issuer == HoldIssuer::Remote {
        HOLD_REMOTE_USAGE
    } else {
        HOLD_USAGE
    }
}

/// The answer to a `hold` that names the person's slot from the wire (`origin=person`).
pub const HOLD_PERSON_USAGE: &str = "ERR usage: origin=person is the window's own hold \
                                            (Agents \u{25b8} Take Over This Tab / Hand Back This \
                                            Tab); hold <sid> on|off [reason=<pct>] \
                                            [origin=fleet|local]\n";

/// The refusal of a `hold` that is not its issuer's to make: an Owner naming or lifting the fleet
/// hold, or a remote one no drive connection serves.
pub const HOLD_DENIED: &str = "ERR denied: only the fleet sets or lifts a fleet hold\n";

/// The answer to a fenced verb (`hold`, `post`, `topic add|drop`, `inbox seen` with a verdict, …)
/// that waited [`SessionFabric::fence_wait`] while a seamless update committed the session, and
/// saw it neither land nor stand down.
pub const HOLD_BUSY: &str = "ERR busy update-committing\n";

/// The refusal of a `hold` from the lane of a lost bridge generation: its loss has been said, or a
/// newer generation owns the link ([`lane_governs`]). Nothing moved.
pub const HOLD_LANE_LOST: &str = "ERR bridge lost: a lost bridge's lane holds and frees nothing\n";

#[cfg(any(test, debug_assertions))]
fn note_fence_block(fabric: &SessionFabric, bound: Option<std::time::Duration>) {
    FENCE_BLOCKS.with(|n| n.set(n.get() + 1));
    *fabric.fence_bound.lock().unwrap_or_else(|p| p.into_inner()) = Some(bound);
    fabric.fence_waits.fetch_add(1, Ordering::SeqCst);
}

#[cfg(not(any(test, debug_assertions)))]
fn note_fence_block(_: &SessionFabric, _: Option<std::time::Duration>) {}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static FENCE_BLOCKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many times this thread has blocked on a hold fence ([`note_fence_block`]).
#[cfg(any(test, debug_assertions))]
pub fn fence_blocks_on_this_thread() -> u64 {
    FENCE_BLOCKS.with(std::cell::Cell::get)
}

/// The negative control of a fenced wait, without a clock.
#[cfg(test)]
pub fn assert_parked_on_the_fence(
    fabric: &SessionFabric,
    before: u64,
    finished: impl Fn() -> bool,
    what: &str,
) {
    let parked = |n: u64| {
        aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::from_millis(1),
            || (fabric.fence_waits() > n).then_some(()),
        )
        .is_some()
    };
    assert!(
        parked(before),
        "{what}: the waiter never blocked on the fence"
    );
    assert!(!finished(), "{what}: blocked, so not answered");
    let blocked = fabric.fence_waits();
    fabric.wake_waiters_for_test();
    assert!(
        parked(blocked),
        "{what}: woken under the standing fence, it must block again"
    );
    assert!(!finished(), "{what}: still not answered");
}

/// Who is issuing a `hold`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HoldIssuer {
    /// The inherited bridge connection.
    Bridge,
    /// The instance Owner token over the local control socket.
    Owner,
    /// The person at the window.
    Person,
    /// A remote node over the fleet's drive op.
    Remote,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum HoldAct<'a> {
    /// Set the slot of `hold.origin`, or move its reason.
    Set(Hold),
    /// [`Self::Set`] for a lost bridge's sweep ([`bridge_lost_waking`]), unless a newer
    /// generation owns the link once the session's lock is taken ([`superseded`]): that bridge
    /// serves the session, and may already have freed it.
    #[cfg(any(unix, test, debug_assertions))]
    Lost(Hold, BridgeGeneration),
    /// Lift the slot of this origin, and only it.
    Lift(&'a str),
    /// Set the remote slot as its node ([`HoldIssuer::Remote`]).
    SetRemote(RemoteHold),
    /// Lift the remote slot.
    LiftRemote(Option<&'a str>),
    /// Lift every slot: the test door's lift ([`apply_hold_for_test`]), never a `hold` verb's.
    #[cfg(any(test, debug_assertions))]
    LiftAll,
}

impl HoldAct<'_> {
    const fn origin(&self) -> Option<&str> {
        match self {
            Self::Set(hold) => Some(hold.origin.as_str()),
            #[cfg(any(unix, test, debug_assertions))]
            Self::Lost(hold, _) => Some(hold.origin.as_str()),
            Self::Lift(origin) => Some(origin),
            Self::SetRemote(_) | Self::LiftRemote(_) => Some(REMOTE_ORIGIN),
            #[cfg(any(test, debug_assertions))]
            Self::LiftAll => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Applied {
    /// A slot moved, and the event was recorded.
    Changed { held: bool },
    /// Nothing moved: the slot already stood as asked, or the act was a superseded generation's
    /// loss ([`HoldAct::Lost`]).
    Unchanged { held: bool },
    /// An Owner's act named the fleet's slot, or an Owner `off` found only a fleet hold standing.
    FleetHeld,
    /// The act named a slot its issuer may not touch.
    NotItsSlot,
    /// A remote node's set while the person's or another node's hold stands, or its lift of
    /// another node's.
    Taken,
    /// A seamless update was committing the session for the whole wait the caller allowed
    /// ([`SessionFabric::fence_hold`]).
    Fenced,
    /// The act came from the lane of a lost bridge generation ([`lane_governs`]).
    LaneLost,
}

fn apply_hold(
    ctx: &dyn FabricSession,
    act: HoldAct<'_>,
    issuer: HoldIssuer,
    fence_wait: Option<std::time::Duration>,
    wake: &dyn Fn(),
) -> Applied {
    apply_hold_as(ctx, act, issuer, fence_wait, false, None, wake)
}

/// Apply `act` under the session's lock. `lane` is the bridge generation whose lane issued it, a
/// lane's own `hold` verb ([`cmd_hold`]): it moves nothing once that generation is lost, judged
/// under the lock, which [`bridge_lost`]'s sweep takes after marking the loss. So a lost
/// generation's verb lands before its loss's sweep of the session, or moves nothing.
fn apply_hold_as(
    ctx: &dyn FabricSession,
    act: HoldAct<'_>,
    issuer: HoldIssuer,
    fence_wait: Option<std::time::Duration>,
    carried: bool,
    lane: Option<BridgeGeneration>,
    wake: &dyn Fn(),
) -> Applied {
    let mut inbox = ctx.fabric().lock();
    let deadline = fence_wait.map(deadline_after);
    while inbox.hold_fenced {
        inbox = match deadline {
            None => {
                note_fence_block(ctx.fabric(), None);
                ctx.fabric().changed.wait(inbox)
            }
            Some(deadline) => {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    return Applied::Fenced;
                }
                note_fence_block(ctx.fabric(), fence_wait);
                ctx.fabric().changed.wait_timeout(inbox, left).0
            }
        };
    }
    if lane.is_some_and(|generation| !lane_governs(generation)) {
        return Applied::LaneLost;
    }
    let remote_act = matches!(act, HoldAct::SetRemote(_) | HoldAct::LiftRemote(_));
    let its_slot = match issuer {
        HoldIssuer::Remote => {
            matches!(act, HoldAct::SetRemote(_) | HoldAct::LiftRemote(Some(_)))
        }
        HoldIssuer::Person => {
            matches!(act, HoldAct::LiftRemote(None))
                || (!remote_act && act.origin() == Some(PERSON_ORIGIN))
        }
        HoldIssuer::Bridge | HoldIssuer::Owner => {
            !remote_act && !matches!(act.origin(), Some(PERSON_ORIGIN | REMOTE_ORIGIN))
        }
    };
    if !its_slot {
        return Applied::NotItsSlot;
    }
    if issuer == HoldIssuer::Remote {
        let anothers = |node: &str| inbox.holds.remote.as_ref().is_some_and(|r| r.node != node);
        let taken = match &act {
            HoldAct::SetRemote(hold) => inbox.holds.person.is_some() || anothers(&hold.node),
            HoldAct::LiftRemote(Some(node)) => anothers(node),
            _ => false,
        };
        if taken {
            return Applied::Taken;
        }
    }
    if issuer == HoldIssuer::Owner {
        // Not the owner's slot to name (`cmd_hold` refuses it first; the rule stays by the slots).
        if act.origin() != Some("local") {
            return Applied::FleetHeld;
        }
        if matches!(act, HoldAct::Lift(_)) && !inbox.holds.has("local") && inbox.holds.has("fleet")
        {
            return Applied::FleetHeld;
        }
    }
    // Read under the session's lock, which the newer bridge's own `hold off` takes after its
    // attach: a loss judged current here lands before that lift, never over it.
    #[cfg(any(unix, test, debug_assertions))]
    if let HoldAct::Lost(_, generation) = &act
        && superseded(*generation)
    {
        return Applied::Unchanged {
            held: inbox.holds.is_held(),
        };
    }
    let before = inbox.holds.clone();
    match &act {
        HoldAct::Set(hold) => {
            *inbox.holds.slot_mut(&hold.origin) = Some(hold.reason.clone());
            if issuer == HoldIssuer::Person {
                inbox.holds.remote = None;
            }
        }
        #[cfg(any(unix, test, debug_assertions))]
        HoldAct::Lost(hold, _) => *inbox.holds.slot_mut(&hold.origin) = Some(hold.reason.clone()),
        HoldAct::Lift(origin) => *inbox.holds.slot_mut(origin) = None,
        HoldAct::SetRemote(hold) => inbox.holds.remote = Some(hold.clone()),
        HoldAct::LiftRemote(_) => inbox.holds.remote = None,
        #[cfg(any(test, debug_assertions))]
        HoldAct::LiftAll => inbox.holds = Holds::default(),
    }
    let held = inbox.holds.is_held();
    let changed = inbox.holds != before;
    if !changed {
        drop(inbox);
        return Applied::Unchanged { held };
    }
    inbox
        .hold_history
        .moved(before.is_held(), held, std::time::Instant::now());
    if issuer == HoldIssuer::Person && matches!(act, HoldAct::Set(_)) {
        ctx.fabric().person_refusals.store(0, Ordering::Relaxed);
    }
    let lifted = act.origin().unwrap_or("local");
    ctx.fabric().hold_serial.fetch_add(1, Ordering::AcqRel);
    let (origin, reason, hold_origin) = inbox.holds.event_fields(lifted);
    let fact = FabricFact::Hold {
        held,
        bridge: issuer == HoldIssuer::Bridge,
        origin: &origin,
        hold_origin: &hold_origin,
        reason: reason.as_deref(),
        carried,
    };
    let owed = ctx.record_fact("hold", fact);
    ctx.fabric()
        .changed
        .ring_after(&ctx.fabric().inbox, inbox, || {
            if owed {
                wake();
            }
        });
    ctx.fabric_changed();
    ctx.hold_moved();
    Applied::Changed { held }
}

#[cfg(any(test, debug_assertions))]
static LINK_SECTION: Mutex<Option<std::thread::ThreadId>> = Mutex::new(None);

/// Run `in_turn` unless another thread's [`with_link_reset`] section is open (`None` then),
/// holding [`LINK_SECTION`] across it. A link read or write from outside a section is a no-op
/// while one is open; held across the body, not only the check, the lock keeps a section from
/// opening between them, resetting the link and then taking the write into its own test.
#[cfg(any(test, debug_assertions))]
fn unless_another_link_section<T>(in_turn: impl FnOnce() -> T) -> Option<T> {
    let section = LINK_SECTION.lock().unwrap_or_else(|p| p.into_inner());
    if section.is_some_and(|owner| owner != std::thread::current().id()) {
        return None;
    }
    let done = in_turn();
    drop(section);
    Some(done)
}

/// A release build opens no link sections: `in_turn` always runs.
#[cfg(not(any(test, debug_assertions)))]
fn unless_another_link_section<T>(in_turn: impl FnOnce() -> T) -> Option<T> {
    Some(in_turn())
}

#[cfg(any(test, debug_assertions))]
static LINK_RESET_ALSO: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// Have every [`with_link_reset`] section also run `reset` at its start and end, under its lock.
#[cfg(any(test, debug_assertions))]
pub fn also_reset_with_the_link(reset: fn()) {
    let _ = LINK_RESET_ALSO.set(reset);
}

/// Run `f` with the process-wide bridge link reset to "never launched", and reset again after.
#[cfg(any(test, debug_assertions))]
pub fn with_link_reset<T>(f: impl FnOnce() -> T) -> T {
    static LINK_TESTS: Mutex<()> = Mutex::new(());

    fn reset_now() {
        #[cfg(test)]
        STALE_AFTER_IN_TEST.store(0, Ordering::Relaxed);
        LINK.state.store(FABRIC_ABSENT, Ordering::Relaxed);
        LINK.supervised.store(false, Ordering::Relaxed);
        #[cfg(any(unix, test, debug_assertions))]
        LINK.hold_on_loss.store(false, Ordering::Relaxed);
        *LINK.generation.lock().unwrap_or_else(|p| p.into_inner()) = 0;
        *LINK.log.lock().unwrap_or_else(|p| p.into_inner()) = None;
        *LINK.node_gaps.lock().unwrap_or_else(|p| p.into_inner()) = NodeGaps::default();
        {
            let mut link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
            link.up = false;
            link.reason.clear();
            link.rtt_ms = None;
            link.acked_at = None;
            link.stalled_at = None;
            link.owed_since = None;
            link.hello = None;
            link.mirror = None;
            link.lane_faults = 0;
        }
        LINK.touched
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        if let Some(also) = LINK_RESET_ALSO.get() {
            also();
        }
    }

    struct Section<'a> {
        _lock: std::sync::MutexGuard<'a, ()>,
    }
    impl Drop for Section<'_> {
        fn drop(&mut self) {
            assert!(
                LINK_TESTS.try_lock().is_err(),
                "the link reset ran outside the lock that serializes it"
            );
            reset_now();
            *LINK_SECTION.lock().unwrap_or_else(|p| p.into_inner()) = None;
        }
    }

    let _section = Section {
        _lock: LINK_TESTS.lock().unwrap_or_else(|p| p.into_inner()),
    };
    *LINK_SECTION.lock().unwrap_or_else(|p| p.into_inner()) = Some(std::thread::current().id());
    reset_now();
    f()
}

/// Whether [`FabricLink::touched`] currently holds `sid`, under any registry.
#[cfg(any(test, debug_assertions))]
pub fn touched_contains(sid: &str) -> bool {
    LINK.touched
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .any(|r| r.sids.contains(sid))
}

/// The owner's local hold a restore layout carried, put back on an already registered session.
pub fn carry_hold(ctx: &dyn FabricSession, hold: Hold) -> bool {
    carry_hold_waking(ctx, hold, &|| {})
}

/// [`carry_hold`], calling `wake` once the fabric lock is released when the session log owes its
/// readers a wake (its `hold` mark).
pub fn carry_hold_waking(ctx: &dyn FabricSession, hold: Hold, wake: &dyn Fn()) -> bool {
    let act = HoldAct::Set(Hold {
        origin: "local".to_string(),
        ..hold
    });
    match apply_hold_as(
        ctx,
        act,
        HoldIssuer::Owner,
        Some(std::time::Duration::ZERO),
        true,
        None,
        wake,
    ) {
        Applied::Changed { held } | Applied::Unchanged { held } => held,
        Applied::FleetHeld
        | Applied::NotItsSlot
        | Applied::Taken
        | Applied::Fenced
        | Applied::LaneLost => false,
    }
}

/// [`apply_hold`] for a caller that already holds the ctx.
#[cfg(any(test, debug_assertions))]
pub fn apply_hold_for_test(ctx: &dyn FabricSession, hold: Option<Hold>) -> bool {
    let act = hold.map_or(HoldAct::LiftAll, HoldAct::Set);
    matches!(
        apply_hold(
            ctx,
            act,
            HoldIssuer::Bridge,
            Some(ctx.fabric().fence_wait()),
            &|| {}
        ),
        Applied::Changed { .. }
    )
}

/// `hold <sid> on|off [reason=<pct>] [origin=fleet|local]`.
pub fn cmd_hold(store: &impl AsStore, rest: &str, issuer: HoldIssuer) -> String {
    let store = &store.as_store();
    cmd_hold_within(store, rest, issuer, None)
}

/// [`cmd_hold`], calling `wake` with the session's id once the fabric lock is released when its
/// log owes its readers a wake (its `hold` mark).
pub fn cmd_hold_waking(
    store: &impl AsStore,
    rest: &str,
    issuer: HoldIssuer,
    wake: &dyn Fn(&SessionId),
) -> String {
    let store = &store.as_store();
    cmd_hold_within_waking(store, rest, issuer, None, wake)
}

/// [`cmd_hold`] with the wait for a committing update as an argument.
pub fn cmd_hold_within(
    store: &impl AsStore,
    rest: &str,
    issuer: HoldIssuer,
    fence_wait: Option<std::time::Duration>,
) -> String {
    cmd_hold_within_waking(store, rest, issuer, fence_wait, &|_| {})
}

/// [`cmd_hold_within`] with [`cmd_hold_waking`]'s wake.
pub fn cmd_hold_within_waking(
    store: &impl AsStore,
    rest: &str,
    issuer: HoldIssuer,
    fence_wait: Option<std::time::Duration>,
    wake: &dyn Fn(&SessionId),
) -> String {
    let store = &store.as_store();
    let usage = hold_usage(issuer);
    let mut toks = rest.split_whitespace();
    let (Some(sid), Some(state)) = (toks.next(), toks.next()) else {
        return usage.to_string();
    };
    if sid.starts_with('@') {
        return format!(
            "ERR usage: hold names its session by its bare sid, with no @: {}",
            usage.trim_start_matches("ERR usage: ")
        );
    }
    let on = match state {
        "on" => true,
        "off" => false,
        _ => return usage.to_string(),
    };
    let mut reason = String::new();
    let mut origin = match issuer {
        HoldIssuer::Bridge => "fleet",
        HoldIssuer::Owner => "local",
        HoldIssuer::Remote => REMOTE_ORIGIN,
        HoldIssuer::Person => return HOLD_PERSON_USAGE.to_string(),
    }
    .to_string();
    for tok in toks {
        if let Some(v) = kv(tok, "reason") {
            reason = v.to_string();
        } else if let Some(v) = kv(tok, "origin") {
            if v == PERSON_ORIGIN {
                return HOLD_PERSON_USAGE.to_string();
            }
            if issuer == HoldIssuer::Remote {
                return usage.to_string();
            }
            if !matches!(v, "fleet" | "local") {
                return usage.to_string();
            }
            origin = v.to_string();
        } else {
            return usage.to_string();
        }
    }
    if issuer == HoldIssuer::Owner && origin != "local" {
        return HOLD_DENIED.to_string();
    }
    let ctx = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        match g.session(&SessionId::new(sid)) {
            Some(ctx) => ctx,
            None => return "ERR no such session\n".to_string(),
        }
    };
    // A bridge lane's own verb speaks for that lane's generation; no other caller's does.
    let lane = match issuer {
        HoldIssuer::Bridge => lane_generation(),
        HoldIssuer::Owner | HoldIssuer::Person | HoldIssuer::Remote => None,
    };
    if lane.is_some_and(|generation| !lane_governs(generation)) {
        return HOLD_LANE_LOST.to_string();
    }
    if issuer == HoldIssuer::Bridge {
        note_bridge_touched(store, sid);
    }
    let driver = match issuer {
        HoldIssuer::Remote => match serving_driver() {
            Some(driver) => Some(driver),
            None => return HOLD_DENIED.to_string(),
        },
        _ => None,
    };
    let act = match (driver, on) {
        (Some(Driver { node, host }), true) => HoldAct::SetRemote(RemoteHold {
            reason: reason_token(&reason),
            node,
            host,
        }),
        (Some(Driver { node, .. }), false) => {
            origin = node;
            HoldAct::LiftRemote(Some(&origin))
        }
        (None, true) => HoldAct::Set(Hold {
            reason: reason_token(&reason),
            origin,
        }),
        (None, false) => HoldAct::Lift(&origin),
    };
    let fence_wait = fence_wait.unwrap_or_else(|| ctx.fabric().fence_wait());
    let applied = apply_hold_as(&ctx, act, issuer, Some(fence_wait), false, lane, &|| {
        wake(ctx.self_id());
    });
    match applied {
        Applied::LaneLost => HOLD_LANE_LOST.to_string(),
        Applied::FleetHeld => HOLD_DENIED.to_string(),
        Applied::NotItsSlot => HOLD_PERSON_USAGE.to_string(),
        Applied::Taken => {
            let holds = ctx.fabric().holds();
            match holds.first() {
                Some(hold) => halted(ctx.as_ref(), &holds, &hold, None),
                None => HOLD_DENIED.to_string(),
            }
        }
        Applied::Fenced => HOLD_BUSY.to_string(),
        Applied::Changed { held } | Applied::Unchanged { held } => {
            format!("OK hold={}\n", u8::from(held))
        }
    }
}

/// Lift each remote hold whose node may drive no more, as its `hold off` would: how many it lifted.
pub fn lift_ungranted_remote_holds(
    store: &impl AsStore,
    granted: &dyn Fn(&str) -> bool,
    wake: &dyn Fn(&SessionId),
) -> usize {
    let store = &store.as_store();
    let ctxs: Vec<_> = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.sessions()
    };
    let mut lifted = 0;
    for ctx in ctxs {
        let Some(node) = ctx
            .fabric()
            .holds()
            .remote()
            .map(|hold| hold.node.clone())
            .filter(|node| !granted(node))
        else {
            continue;
        };
        let applied = apply_hold(
            ctx.as_ref(),
            HoldAct::LiftRemote(Some(&node)),
            HoldIssuer::Remote,
            Some(ctx.fabric().fence_wait()),
            &|| wake(ctx.self_id()),
        );
        if matches!(applied, Applied::Changed { .. }) {
            lifted += 1;
        }
    }
    lifted
}

/// What this Mac found of the node of a remote hold a local `hold off` met ([`owner_off_remote`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Holder {
    /// It proved itself to this Mac just now: it still drives it.
    Drives,
    /// It did not prove itself just now, for this reason, which says nothing of its own drives.
    Again(String),
    /// It may drive this Mac no more, for this reason.
    Gone(String),
}

/// What a local `hold off` did to a remote hold ([`owner_off_remote`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteOff {
    /// It lifted the hold.
    Lifted {
        /// The hold lifted.
        hold: RemoteHold,
        /// Why its node may drive this Mac no more.
        why: String,
        /// The `hold` verb's answer.
        reply: String,
    },
    /// The hold stands: the refusal, naming why and what lifts it.
    Stands(String),
}

/// An Owner's `hold <sid> off` that finds no local slot and a remote one standing: `holder` asks
/// whether that node may still drive this Mac, and its hold is lifted, as its own `hold off`
/// would, once it may not. `None` where the `off` meets no such hold.
pub fn owner_off_remote(
    store: &impl AsStore,
    sid: &str,
    holder: &dyn Fn(&RemoteHold) -> Holder,
    wake: &dyn Fn(&SessionId),
) -> Option<RemoteOff> {
    let store = &store.as_store();
    let ctx = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.session(&SessionId::new(sid))?
    };
    let holds = ctx.fabric().holds();
    let remote = holds.remote().filter(|_| !holds.has("local"))?.clone();
    let why = match holder(&remote) {
        Holder::Gone(why) => why,
        found => {
            let holds = ctx.fabric().holds();
            let hold = holds.first()?;
            return Some(RemoteOff::Stands(halted(
                ctx.as_ref(),
                &holds,
                &hold,
                Some(&found),
            )));
        }
    };
    let applied = apply_hold(
        ctx.as_ref(),
        HoldAct::LiftRemote(Some(&remote.node)),
        HoldIssuer::Remote,
        Some(ctx.fabric().fence_wait()),
        &|| wake(ctx.self_id()),
    );
    match applied {
        Applied::Changed { held } => Some(RemoteOff::Lifted {
            hold: remote,
            why,
            reply: format!("OK hold={} lifted=remote\n", u8::from(held)),
        }),
        Applied::Fenced => Some(RemoteOff::Stands(HOLD_BUSY.to_string())),
        _ => None,
    }
}

/// What the window's Take Over or Hand Back did ([`person_hold`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersonHoldOutcome {
    /// The person's slot moved, as asked.
    Moved,
    /// The slot already stood as asked.
    Unchanged,
    /// A seamless update was committing the session.
    Busy,
}

/// The person's take over (`on`) or hand back (`off`) of one session.
pub fn person_hold(ctx: &dyn FabricSession, on: bool, wake: &dyn Fn()) -> PersonHoldOutcome {
    let (act, issuer) = if on {
        (
            HoldAct::Set(Hold {
                reason: TAKEN_OVER.to_string(),
                origin: PERSON_ORIGIN.to_string(),
            }),
            HoldIssuer::Person,
        )
    } else {
        match ctx.fabric().holds().hand_back_origin() {
            Some("local") => (HoldAct::Lift("local"), HoldIssuer::Owner),
            Some(REMOTE_ORIGIN) => (HoldAct::LiftRemote(None), HoldIssuer::Person),
            _ => (HoldAct::Lift(PERSON_ORIGIN), HoldIssuer::Person),
        }
    };
    match apply_hold(ctx, act, issuer, Some(std::time::Duration::ZERO), wake) {
        Applied::Changed { .. } => PersonHoldOutcome::Moved,
        Applied::Fenced => PersonHoldOutcome::Busy,
        Applied::Unchanged { .. }
        | Applied::FleetHeld
        | Applied::NotItsSlot
        | Applied::Taken
        | Applied::LaneLost => PersonHoldOutcome::Unchanged,
    }
}

const DELIVER_USAGE: &str = "ERR usage: deliver <sid> off=<n> from=<p> kind=<k> [trust=<t>] \
                             [topic=<t>] [re=<n>] [dl=<ms>] [was=<n>] [late=1] [demoted=<k>] \
                             [via=<p,...>] [verdict=<handled|refused|deferred>] [len=<n>] \
                             [text=<pct>] \
                             | deliver <sid> landed=<post-id> off=<n> \
                             | deliver <sid> fetched=<off> (err=<token> [floor=<n>] \
                             | from=<p> kind=<k> [trust=<t>] [re=<n>] [dl=<ms>] \
                             [demoted=<k>] [via=<p,...>] [verdict=<v>] [len=<n>] [at=<n>] \
                             [more=1] [text=<pct>]) \
                             | deliver <sid> receipt=<rid> off=<n|->\n";

/// `deliver` — put one bus record in a session's inbox, close an outbound post (`landed=`), answer
/// a parked `inbox get @<off>` (`fetched=`), or retire an owed receipt (`receipt=`).
pub fn cmd_deliver(store: &impl AsStore, rest: &str) -> String {
    cmd_deliver_waking(store, rest, &|_| {})
}

/// [`cmd_deliver`], calling `wake` with the session's id once the fabric lock is released when its
/// log owes its readers a wake (a `mail` or `mail-landed` mark).
pub fn cmd_deliver_waking(store: &impl AsStore, rest: &str, wake: &dyn Fn(&SessionId)) -> String {
    let store = &store.as_store();
    let mut toks = rest.split_whitespace();
    let Some(sid) = toks.next() else {
        return DELIVER_USAGE.to_string();
    };
    let ctx = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        match g.session(&SessionId::new(sid)) {
            Some(ctx) => ctx,
            None => return "ERR no such session\n".to_string(),
        }
    };
    let rest_toks: Vec<&str> = toks.collect();
    note_bridge_touched(store, sid);
    link_evidence(lane_generation());
    let session_wake = || wake(ctx.self_id());
    if rest_toks.iter().any(|t| t.starts_with("landed=")) {
        let landed = deliver_landed(&ctx, &rest_toks, &session_wake);
        settle_work_owed(store);
        return landed;
    }
    if rest_toks.iter().any(|t| t.starts_with("fetched=")) {
        return deliver_fetched(&ctx, &rest_toks);
    }
    if rest_toks.iter().any(|t| t.starts_with("receipt=")) {
        return deliver_receipt(&ctx, &rest_toks);
    }
    deliver_row(&ctx, &rest_toks, &session_wake)
}

fn parse_uint<T: std::str::FromStr>(v: &str) -> Option<T> {
    (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())).then(|| v.parse().ok())?
}

fn deliver_receipt(ctx: &dyn FabricSession, toks: &[&str]) -> String {
    let mut rid: Option<u64> = None;
    let mut landed = false;
    for tok in toks {
        if let Some(v) = kv(tok, "receipt") {
            match parse_uint(v) {
                Some(n) => rid = Some(n),
                None => return DELIVER_USAGE.to_string(),
            }
        } else if let Some(v) = kv(tok, "off") {
            if v != "-" && parse_uint::<u64>(v).is_none() {
                return DELIVER_USAGE.to_string();
            }
            landed = true;
        } else {
            return DELIVER_USAGE.to_string();
        }
    }
    let (Some(rid), true) = (rid, landed) else {
        return DELIVER_USAGE.to_string();
    };
    let Some(mut inbox) = ctx
        .fabric()
        .wait_unfenced_retirement(ctx.fabric().lock(), ctx.fabric().fence_wait())
    else {
        return HOLD_BUSY.to_string();
    };
    let before = inbox.receipts.len();
    inbox.receipts.retain(|r| r.rid != rid);
    if inbox.receipts.len() != before {
        inbox.outbox_moves += 1;
    }
    "OK\n".to_string()
}

fn deliver_fetched(ctx: &dyn FabricSession, toks: &[&str]) -> String {
    let mut off: Option<u64> = None;
    let mut err: Option<String> = None;
    let mut floor: Option<u64> = None;
    let mut at: usize = 0;
    let mut more = false;
    let mut fields: Vec<&str> = Vec::with_capacity(toks.len());
    for tok in toks {
        if let Some(v) = kv(tok, "fetched") {
            match parse_uint(v) {
                Some(n) => off = Some(n),
                None => return DELIVER_USAGE.to_string(),
            }
        } else if let Some(v) = kv(tok, "err") {
            // The bridge's output is still input: the reason is one bounded token or the line is
            // refused.
            if !valid_dead_reason(v) {
                return DELIVER_USAGE.to_string();
            }
            err = Some(v.to_string());
        } else if let Some(v) = kv(tok, "floor") {
            match parse_uint(v) {
                Some(n) => floor = Some(n),
                None => return DELIVER_USAGE.to_string(),
            }
        } else if let Some(v) = kv(tok, "at") {
            match parse_uint(v) {
                Some(n) => at = n,
                None => return DELIVER_USAGE.to_string(),
            }
        } else if *tok == "more=1" {
            more = true;
        } else {
            fields.push(tok);
        }
    }
    let Some(off) = off else {
        return DELIVER_USAGE.to_string();
    };
    let slot = match err {
        Some(why) => FetchSlot::Failed(match floor {
            Some(floor) if why == "retained" => format!("retained-from={floor}"),
            _ => why,
        }),
        None => {
            let f = match parse_deliver_fields(&fields, usize::MAX) {
                Ok(f) if !f.late && f.off.is_none() => f,
                Ok(_) => return refuse_fetched(ctx, off, DELIVER_USAGE),
                Err(reply) => return refuse_fetched(ctx, off, &reply),
            };
            let mut inbox = ctx.fabric().lock();
            let mut body = String::new();
            if at > 0 {
                let partial = inbox.fetches.iter_mut().find_map(|(o, slot)| match slot {
                    FetchSlot::Partial(p) if *o == off && p.body.len() == at => Some(p),
                    _ => None,
                });
                let Some(p) = partial else {
                    drop(inbox);
                    return refuse_fetched(ctx, off, "ERR fetch out of order\n");
                };
                body = std::mem::take(&mut p.body);
            }
            body.push_str(&f.body);
            if body.len() > BODY_MAX {
                drop(inbox);
                return refuse_fetched(ctx, off, "ERR too large\n");
            }
            drop(inbox);
            let fetched = Fetched {
                off,
                from: f.from,
                kind: f.kind,
                trust: f.trust,
                re: f.re,
                dl: f.dl,
                demoted: f.demoted,
                via: f.via,
                verdict: f.verdict,
                len: f.declared_len.unwrap_or(body.len()).max(body.len()),
                body,
            };
            if more {
                FetchSlot::Partial(fetched)
            } else {
                FetchSlot::Done(fetched)
            }
        }
    };
    let answer_ready = !matches!(slot, FetchSlot::Partial(_));
    let mut inbox = ctx.fabric().lock();
    if matches!(slot, FetchSlot::Done(_)) {
        inbox.oldest_on_bus = Some(inbox.oldest_on_bus.map_or(off, |o| o.min(off)));
    }
    match inbox.fetches.iter().position(|(o, _)| *o == off) {
        Some(i) => inbox.fetches[i].1 = slot,
        None if !matches!(slot, FetchSlot::Done(_)) => return "OK unrequested=1\n".to_string(),
        None => {
            evict_answered_fetch(&mut inbox);
            if inbox.fetches.len() >= FETCH_SLOTS {
                return "OK unrequested=1\n".to_string();
            }
            inbox.fetches.push_back((off, slot));
        }
    }
    if answer_ready {
        ctx.fabric().changed.ring(inbox);
        ctx.fabric_changed();
    }
    "OK\n".to_string()
}

fn refuse_fetched(ctx: &dyn FabricSession, off: u64, reply: &str) -> String {
    let mut inbox = ctx.fabric().lock();
    let waiting = inbox
        .fetches
        .iter_mut()
        .find(|(o, s)| *o == off && matches!(s, FetchSlot::Pending | FetchSlot::Partial(_)));
    if let Some((_, slot)) = waiting {
        *slot = FetchSlot::Failed("malformed".to_string());
        ctx.fabric().changed.ring(inbox);
        ctx.fabric_changed();
    }
    reply.to_string()
}

fn evict_answered_fetch(inbox: &mut Inbox) {
    if inbox.fetches.len() < FETCH_SLOTS {
        return;
    }
    if let Some(i) = inbox
        .fetches
        .iter()
        .position(|(_, s)| !matches!(s, FetchSlot::Pending | FetchSlot::Partial(_)))
    {
        inbox.fetches.remove(i);
    }
}

struct DeliverFields {
    off: Option<u64>,
    from: String,
    kind: String,
    trust: String,
    re: Option<u64>,
    dl: Option<u64>,
    late: bool,
    demoted: Option<String>,
    via: Option<String>,
    verdict: Option<String>,
    len: usize,
    declared_len: Option<usize>,
    topic: Option<String>,
    /// The offset on this log a record moved here across this Mac's key rotation was first moved
    /// from (§11 C3b, `aterm_types::control_verbs::LINK_MOVED_LANE`).
    was: Option<u64>,
    body: String,
}

fn parse_deliver_fields(toks: &[&str], len_max: usize) -> Result<DeliverFields, String> {
    let mut f = DeliverFields {
        off: None,
        from: String::new(),
        kind: String::new(),
        trust: String::new(),
        re: None,
        dl: None,
        late: false,
        demoted: None,
        via: None,
        verdict: None,
        len: 0,
        declared_len: None,
        topic: None,
        was: None,
        body: String::new(),
    };
    let mut declared_len: Option<usize> = None;
    let mut text = String::new();
    for tok in toks {
        if let Some(v) = kv(tok, "off") {
            match parse_uint(v) {
                Some(n) => f.off = Some(n),
                None => return Err(DELIVER_USAGE.to_string()),
            }
        } else if let Some(v) = kv(tok, "from") {
            f.from = v.to_string();
        } else if let Some(v) = kv(tok, "kind") {
            f.kind = v.to_string();
        } else if let Some(v) = kv(tok, "trust") {
            f.trust = v.to_string();
        } else if let Some(v) = kv(tok, "re") {
            match parse_uint(v) {
                Some(n) => f.re = Some(n),
                None => return Err(DELIVER_USAGE.to_string()),
            }
        } else if let Some(v) = kv(tok, "dl") {
            match parse_uint(v) {
                Some(n) => f.dl = Some(n),
                None => return Err(DELIVER_USAGE.to_string()),
            }
        } else if let Some(v) = kv(tok, "was") {
            match parse_uint(v) {
                Some(n) => f.was = Some(n),
                None => return Err(DELIVER_USAGE.to_string()),
            }
        } else if let Some(v) = kv(tok, "late") {
            f.late = v == "1";
        } else if let Some(v) = kv(tok, "demoted") {
            if !KINDS.contains(&v) {
                return Err(DELIVER_USAGE.to_string());
            }
            f.demoted = Some(v.to_string());
        } else if let Some(v) = kv(tok, "via") {
            if !via_ok(v) {
                return Err(DELIVER_USAGE.to_string());
            }
            f.via = Some(v.to_string());
        } else if let Some(v) = kv(tok, "topic") {
            // The topic is closed to the grammar, like every word on this line: the bridge's output
            // is still input, and a topic with a space or an `=` could invent a field on the `msg`
            // row.
            if !is_topic(v) {
                return Err(DELIVER_USAGE.to_string());
            }
            f.topic = Some(v.to_string());
        } else if let Some(v) = kv(tok, "verdict") {
            // The verdict is closed, like `trust=`: a receipt's word is one of three, and an agent
            // never reads a verdict token this endpoint did not classify.
            if !VERDICTS.contains(&v) {
                return Err(DELIVER_USAGE.to_string());
            }
            f.verdict = Some(v.to_string());
        } else if let Some(v) = kv(tok, "len") {
            match parse_uint::<usize>(v) {
                Some(n) if n <= len_max => declared_len = Some(n),
                _ => return Err("ERR too large\n".to_string()),
            }
        } else if let Some(v) = kv(tok, "text") {
            text = v.to_string();
        } else {
            return Err(DELIVER_USAGE.to_string());
        }
    }
    if !valid_principal(&f.from) || !KINDS.contains(&f.kind.as_str()) {
        return Err(DELIVER_USAGE.to_string());
    }
    if f.trust.is_empty() {
        f.trust = "agent".to_string();
    }
    if !TRUSTS.contains(&f.trust.as_str()) {
        return Err(DELIVER_USAGE.to_string());
    }
    f.body = aterm_control::wire::pct_decode(&text);
    if f.body.len() > BODY_MAX {
        return Err("ERR too large\n".to_string());
    }
    f.len = declared_len.unwrap_or(f.body.len());
    f.declared_len = declared_len;
    Ok(f)
}

fn deliver_landed(ctx: &dyn FabricSession, toks: &[&str], wake: &dyn Fn()) -> String {
    let mut post_id: Option<u64> = None;
    let mut off: Option<u64> = None;
    for tok in toks {
        if let Some(v) = kv(tok, "landed") {
            post_id = parse_uint(v);
        } else if let Some(v) = kv(tok, "off") {
            off = parse_uint(v);
        } else {
            return DELIVER_USAGE.to_string();
        }
    }
    let (Some(post_id), Some(off)) = (post_id, off) else {
        return DELIVER_USAGE.to_string();
    };
    match retire_post(ctx, post_id, Some(off), DEAD_DEFAULT, false, wake) {
        Ok(()) => "OK\n".to_string(),
        Err(e) => e,
    }
}

/// Retire post `post_id` as the bridge says: landed at `off`, or undelivered for the bridge's
/// `reason` (`off: None`). The waiting client is told `reason` itself; the session log's
/// `mail-landed` mark and the timeline name the [`DeadReason`] it is ([`DeadReason::told`]).
fn retire_post(
    ctx: &dyn FabricSession,
    post_id: u64,
    off: Option<u64>,
    reason: &str,
    dup: bool,
    wake: &dyn Fn(),
) -> Result<(), String> {
    let Some(mut inbox) = ctx
        .fabric()
        .wait_unfenced_retirement(ctx.fabric().lock(), ctx.fabric().fence_wait())
    else {
        return Err(HOLD_BUSY.to_string());
    };
    let Some(row) = inbox.posts.iter_mut().find(|p| p.id == post_id) else {
        return Err("ERR no such post\n".to_string());
    };
    let already = match off {
        Some(n) => row.off == Some(n),
        None => row.dead,
    };
    match off {
        Some(n) => {
            if row.off.is_none() {
                row.landed_on_log = true;
            }
            row.off = Some(n);
            row.dup |= dup;
        }
        None => {
            row.dead = true;
            row.dead_reason = reason.to_string();
        }
    }
    row.body.clear();
    row.body.shrink_to_fit();
    row.via = None;
    row.key = None;
    let mut owed = false;
    if !already {
        inbox.outbox_moves += 1;
        let fact = FabricFact::MailLanded {
            post: post_id,
            off,
            why: off.is_none().then(|| DeadReason::told(reason)),
        };
        owed = ctx.record_fact("post-landed", fact);
    }
    inbox.trim_retired_posts();
    ctx.fabric()
        .changed
        .ring_after(&ctx.fabric().inbox, inbox, || {
            if owed {
                wake();
            }
        });
    ctx.fabric_changed();
    Ok(())
}

const OUTBOX_USAGE: &str = "ERR usage: outbox [<max>] [earlier-receipts=1] | outbox sent <sid> <post-id> off=<n|-> \
                            [dup=1] [reason=<word>]\n";

const DEAD_DEFAULT: &str = DeadReason::Undeliverable.word();

fn valid_dead_reason(v: &str) -> bool {
    !v.is_empty() && v.len() <= 24 && v.chars().all(|c| c.is_ascii_lowercase() || c == '-')
}

/// `outbox` — the whole keyword.
pub fn cmd_outbox_verb(store: &impl AsStore, rest: &str) -> String {
    cmd_outbox_verb_waking(store, rest, &|_| {})
}

/// [`cmd_outbox_verb`] with `outbox sent`'s wake ([`cmd_outbox_sent_waking`]).
pub fn cmd_outbox_verb_waking(
    store: &impl AsStore,
    rest: &str,
    wake: &dyn Fn(&SessionId),
) -> String {
    let store = &store.as_store();
    let trimmed = rest.trim();
    match trimmed.split_once(' ') {
        Some(("sent", tail)) => cmd_outbox_sent_waking(store, tail, wake),
        None if trimmed == "sent" => OUTBOX_USAGE.to_string(),
        _ => cmd_outbox(store, rest),
    }
}

/// `outbox [<max>] [earlier-receipts=1]`.
pub fn cmd_outbox(store: &impl AsStore, rest: &str) -> String {
    let store = &store.as_store();
    let mut max = usize::MAX;
    let mut earlier_receipts = false;
    for tok in rest.split_whitespace() {
        if tok == "earlier-receipts=1" {
            earlier_receipts = true;
            continue;
        }
        match parse_uint::<usize>(tok) {
            Some(n) => max = n,
            None => return OUTBOX_USAGE.to_string(),
        }
    }
    let handles = store.read().unwrap_or_else(|p| p.into_inner()).sessions();
    let mut turns: Vec<_> = handles
        .into_iter()
        .map(|h| {
            let turn = h.fabric().lock().outbox_turn;
            (turn, h)
        })
        .collect();
    let turn = turns
        .iter()
        .map(|(turn, _)| *turn)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    turns.sort_by_key(|(turn, _)| *turn);
    let handles: Vec<_> = turns.into_iter().map(|(_, h)| h).collect();
    let mut payload = String::new();
    let mut sent = 0usize;
    let mut listed: Vec<Option<u64>> = vec![None; handles.len()];
    let mut exhausted = vec![false; handles.len()];
    let mut stopped = false;
    'turns: loop {
        let mut took = false;
        for (i, h) in handles.iter().enumerate() {
            if exhausted[i] {
                continue;
            }
            if sent >= max || (sent > 0 && payload.len() >= OUTBOX_DRAIN_BYTES_MAX) {
                stopped = true;
                break 'turns;
            }
            let mut inbox = h.fabric().lock();
            let after = listed[i];
            let Some(post) = inbox
                .posts
                .iter()
                .filter(|p| p.off.is_none() && !p.dead)
                .find(|p| after.is_none_or(|last| p.id > last))
            else {
                exhausted[i] = true;
                continue;
            };
            payload.push_str(&format!(
                "post sid={} id={} to={} kind={}",
                h.self_id().as_str(),
                post.id,
                post.to,
                post.kind
            ));
            if let Some(re) = post.re {
                payload.push_str(&format!(" re={re}"));
            }
            if let Some(dl) = post.dl {
                payload.push_str(&format!(" dl={dl}"));
            }
            if let Some(via) = &post.via {
                payload.push_str(&format!(" via={via}"));
            }
            if let Some(key) = &post.key {
                payload.push_str(&format!(" key={key}"));
            }
            payload.push_str(&format!(" len={}\n", post.body.len()));
            payload.push_str(&post.body);
            listed[i] = Some(post.id);
            inbox.outbox_turn = turn;
            sent += 1;
            took = true;
        }
        if !took {
            break;
        }
    }
    let mut more = false;
    for (i, h) in handles.iter().enumerate() {
        if !stopped || more {
            break;
        }
        if exhausted[i] {
            continue;
        }
        let after = listed[i];
        more = h
            .fabric()
            .lock()
            .posts
            .iter()
            .any(|p| p.off.is_none() && !p.dead && after.is_none_or(|last| p.id > last));
    }
    for h in &handles {
        let sid = h.self_id().as_str();
        let inbox = h.fabric().lock();
        for r in &inbox.receipts {
            // A bridge that did not ask (`earlier-receipts=1`) would ignore `log=earlier`.
            if r.earlier_log && !earlier_receipts {
                continue;
            }
            payload.push_str(&format!(
                "receipt sid={sid} rid={} off={} verdict={} kind={} from={}{}\n",
                r.rid,
                r.off,
                r.verdict,
                r.kind,
                r.from,
                if r.earlier_log { " log=earlier" } else { "" }
            ));
        }
        for (off, slot) in &inbox.fetches {
            if matches!(slot, FetchSlot::Pending | FetchSlot::Partial(_)) {
                payload.push_str(&format!("fetch sid={sid} off={off}\n"));
            }
        }
    }
    if more {
        payload.push_str("more\n");
    }
    // READ AFTER THE LISTING, where the bridge's own re-read would stand: a session that left
    // before a post listed here was queued has moved this count, so the bridge looks before it
    // routes that post ([`Registry::roster_seq`]). Read with the snapshot above, an exit during
    // the walk would go unseen beside a post queued after it.
    let roster = store.read().unwrap_or_else(|p| p.into_inner()).roster_seq();
    format!("OK {} roster={roster}\n{payload}", payload.len())
}

/// `outbox sent <sid> <post-id> off=<n|-> [dup=1] [reason=<word>]`.
pub fn cmd_outbox_sent(store: &impl AsStore, rest: &str) -> String {
    cmd_outbox_sent_waking(store, rest, &|_| {})
}

/// [`cmd_outbox_sent`], calling `wake` with the session's id once the fabric lock is released
/// when its log owes its readers a wake (a `mail-landed` mark).
pub fn cmd_outbox_sent_waking(
    store: &impl AsStore,
    rest: &str,
    wake: &dyn Fn(&SessionId),
) -> String {
    let store = &store.as_store();
    let mut toks = rest.split_whitespace();
    let (Some(sid), Some(id), Some(off_tok)) = (toks.next(), toks.next(), toks.next()) else {
        return OUTBOX_USAGE.to_string();
    };
    let mut reason = DEAD_DEFAULT;
    let mut dup = false;
    for tok in toks {
        if tok == "dup=1" {
            dup = true;
            continue;
        }
        match kv(tok, "reason") {
            Some(v) if valid_dead_reason(v) => reason = v,
            _ => return OUTBOX_USAGE.to_string(),
        }
    }
    let Some(post_id) = parse_uint::<u64>(id) else {
        return OUTBOX_USAGE.to_string();
    };
    let Some(off_raw) = kv(off_tok, "off") else {
        return OUTBOX_USAGE.to_string();
    };
    let off = match off_raw {
        "-" => None,
        v => match parse_uint::<u64>(v) {
            Some(n) => Some(n),
            None => return OUTBOX_USAGE.to_string(),
        },
    };
    if dup && off.is_none() {
        return OUTBOX_USAGE.to_string();
    }
    let ctx = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        match g.session(&SessionId::new(sid)) {
            Some(ctx) => ctx,
            None => return "ERR no such session\n".to_string(),
        }
    };
    note_bridge_touched(store, sid);
    link_evidence(lane_generation());
    let retired = retire_post(&ctx, post_id, off, reason, dup, &|| wake(ctx.self_id()));
    settle_work_owed(store);
    match retired {
        Ok(()) => "OK\n".to_string(),
        Err(e) => e,
    }
}

fn deliver_row(ctx: &dyn FabricSession, toks: &[&str], wake: &dyn Fn()) -> String {
    let DeliverFields {
        off,
        from,
        kind,
        trust,
        re,
        dl,
        late,
        demoted,
        via,
        verdict,
        len,
        declared_len: _,
        topic,
        was,
        body,
    } = match parse_deliver_fields(toks, BODY_MAX) {
        Ok(f) => f,
        Err(reply) => return reply,
    };
    let Some(off) = off else {
        return DELIVER_USAGE.to_string();
    };

    let refill = topic.is_some() && bridge_refills_broadcasts();
    let mut inbox = ctx.fabric().wait_fenced_delivery(
        ctx.fabric().lock(),
        ctx.self_id().as_str(),
        topic.as_deref(),
    );
    // A repeat of an offset this endpoint holds is answered from that memory, and so is a record
    // moved here across this Mac's key rotation whose first offset it holds (`was=`, §11 C3b): its
    // session was shown that mail before the move, which a bridge that went before its commit
    // left to the move.
    let known = |at: u64| inbox.dedup_set.contains(&at) || inbox.row_at(at).is_some();
    let (off_known, was_known) = (known(off), was.is_some_and(known));
    if !aterm_mesh_core::inbox_move::shows_anew(off_known, was_known) {
        let existing = inbox
            .row_at(off)
            .or_else(|| was.and_then(|w| inbox.row_at(w)))
            .map(|r| r.id);
        // The mail it names was shown, its `MailIn` recorded: each offset this delivery names is
        // remembered as that mail's, so a copy a later rotation moved from this one, whose bridge
        // names it by this copy's offset where it cannot read further back, is answered too.
        if !off_known {
            remember_offset(&mut inbox, off, true);
        }
        if let Some(was) = was.filter(|_| !was_known) {
            remember_offset(&mut inbox, was, true);
        }
        return match existing {
            Some(id) => format!("OK {id}\n"),
            None => "OK 0\n".to_string(),
        };
    }
    if let Some(t) = topic.as_deref()
        && !ctx.fabric().opted_into(t)
    {
        return "ERR not-subscribed\n".to_string();
    }
    let key = quota_key(&from);
    let unread_from_peer = inbox
        .rows
        .iter()
        .filter(|r| !r.listed && quota_key(&r.from) == key)
        .count();
    if unread_from_peer >= SENDER_QUOTA {
        // A refused broadcast is counted once: its offset is remembered as delivered, so a bridge
        // that walks this topic again from an earlier unhandled row ([`InboxRow::refill`]) is
        // answered `OK 0` rather than refused and counted a second time.
        if topic.is_some() {
            inbox.dropped += 1;
            remember_offset(&mut inbox, off, false);
        }
        return "ERR quota\n".to_string();
    }

    inbox.last_msg_id += 1;
    let id = inbox.last_msg_id;
    let re_id = re.and_then(|r| {
        inbox
            .posts
            .iter()
            .find(|p| p.off == Some(r) && p.landed_on_log)
            .map(|p| p.id)
    });
    let is_addressed = topic.is_none();
    let row = InboxRow {
        id,
        off,
        t_ms: now_ms(),
        from: from.clone(),
        kind: kind.clone(),
        trust,
        re,
        re_id,
        dl,
        late,
        verdict,
        decision: None,
        demoted,
        via,
        len,
        body,
        topic,
        listed: false,
        refill,
        fleet: None,
        on_log: true,
    };
    evict_for(&mut inbox);
    inbox.rows.push_back(row);
    inbox.arrived = handler_tally::stamp();
    inbox.bus_head = inbox.bus_head.max(off);
    if is_addressed {
        inbox.addressed_head = Some(inbox.addressed_head.map_or(off, |h| h.max(off)));
    }
    inbox.oldest_on_bus = Some(inbox.oldest_on_bus.map_or(off, |o| o.min(off)));
    remember_offset(&mut inbox, off, true);
    if let Some(was) = was {
        // So another copy of it, one a second rotation moved again, is answered too.
        remember_offset(&mut inbox, was, true);
    }
    let carried_was = was.is_some_and(|w| inbox.carried_marks.remove(&w));
    if inbox.carried_marks.remove(&off) | carried_was {
        ctx.fabric().changed.ring(inbox);
        ctx.fabric_changed();
        return format!("OK {id}\n");
    }
    let fact = FabricFact::MailIn {
        kind: &kind,
        from: &from,
        off,
        inbox: id,
    };
    let owed = ctx.record_fact("inbox", fact);
    ctx.fabric().changed.ring(inbox);
    ctx.fabric_changed();
    if owed {
        wake();
    }
    format!("OK {id}\n")
}

fn quota_key(from: &str) -> &str {
    from.split_once('@').map_or(from, |(_, node)| node)
}

fn remember_offset(inbox: &mut Inbox, off: u64, marked: bool) {
    inbox.dedup.push_back((off, marked));
    inbox.dedup_set.insert(off);
    while inbox.dedup.len() > DEDUP_CAP {
        if let Some((old, _)) = inbox.dedup.pop_front() {
            inbox.dedup_set.remove(&old);
        }
    }
}

fn evict_for(inbox: &mut Inbox) {
    if inbox.rows.len() < RING_CAP {
        return;
    }
    let seen = inbox.seen;
    let pick = [
        |r: &InboxRow| !r.is_human() && r.listed,
        |r: &InboxRow| !r.is_human(),
        |r: &InboxRow| r.listed,
        |_: &InboxRow| true,
    ]
    .iter()
    .find_map(|pred| inbox.rows.iter().position(pred));
    if let Some(i) = pick
        && let Some(row) = inbox.rows.remove(i)
        && row.id > seen
    {
        inbox.dropped += 1;
    }
}

/// `topic add <t> [since=head|@<off>] | topic drop <t> | topic ls`.
pub fn cmd_topic(ctx: &dyn FabricSession, rest: &str) -> String {
    let mut words = rest.split_whitespace();
    match words.next() {
        Some("ls") | None => {
            if words.next().is_some() {
                return TOPIC_USAGE.to_string();
            }
            let topics = ctx.fabric().topics();
            let (seen, given_up) = {
                let inbox = ctx.fabric().lock();
                (inbox.seen, inbox.topic_given_up.clone())
            };
            let mut out = format!("OK {} seen={seen}\n", topics.len());
            for (t, e) in topics {
                out.push_str(&format!("topic {t} since={}", e.since));
                if let Some(g) = given_up.get(&t) {
                    out.push_str(&format!(" given_up=@{}..@{}", g.from, g.to));
                }
                out.push('\n');
            }
            out
        }
        Some("add") => {
            let Some(t) = words.next() else {
                return TOPIC_USAGE.to_string();
            };
            if !is_topic(t) {
                return TOPIC_USAGE.to_string();
            }
            let since = match words.next() {
                None => "head".to_string(),
                Some(tok) => match tok.strip_prefix("since=") {
                    Some(v) if valid_since(v) => v.to_string(),
                    _ => return TOPIC_USAGE.to_string(),
                },
            };
            if words.next().is_some() {
                return TOPIC_USAGE.to_string();
            }
            let Some(unfenced) = ctx
                .fabric()
                .wait_unfenced(ctx.fabric().lock(), ctx.fabric().fence_wait())
            else {
                return HOLD_BUSY.to_string();
            };
            let added = ctx.fabric().topic_add_recording(t, &since, &|kind, fact| {
                let _ = ctx.record_fact(kind, fact);
            });
            let Ok(added) = added else {
                return format!("ERR topics full (max {MAX_TOPICS}): drop one first\n");
            };
            ctx.fabric().changed.ring(unfenced);
            format!("OK {t} since={since} added={}\n", u8::from(added))
        }
        Some("drop") => {
            let Some(t) = words.next() else {
                return TOPIC_USAGE.to_string();
            };
            if words.next().is_some() || !is_topic(t) {
                return TOPIC_USAGE.to_string();
            }
            let Some(mut unfenced) = ctx
                .fabric()
                .wait_unfenced(ctx.fabric().lock(), ctx.fabric().fence_wait())
            else {
                return HOLD_BUSY.to_string();
            };
            let dropped = ctx.fabric().topic_drop_recording(t, &|kind, fact| {
                let _ = ctx.record_fact(kind, fact);
            });
            if dropped {
                for row in unfenced
                    .rows
                    .iter_mut()
                    .filter(|r| r.topic.as_deref() == Some(t))
                {
                    row.refill = false;
                }
                unfenced.topic_given_up.remove(t);
            }
            ctx.fabric().changed.ring(unfenced);
            format!("OK {t} dropped={}\n", u8::from(dropped))
        }
        Some(_) => TOPIC_USAGE.to_string(),
    }
}

/// The topic set as the handoff manifest carries it.
#[cfg(any(unix, test, debug_assertions))]
pub fn render_topics(topics: &[(String, TopicEntry)]) -> Vec<String> {
    topics
        .iter()
        .map(|(t, e)| format!("{t} {}", e.since))
        .collect()
}

/// The inverse of [`render_topics`], validating every row.
pub fn parse_topics(rows: &[String]) -> Vec<(String, TopicEntry)> {
    rows.iter()
        .filter_map(|row| {
            let mut words = row.split(' ');
            let t = words.next()?;
            let since = words.next()?;
            if let Some(extra) = words.next()
                && (extra.is_empty() || !extra.bytes().all(|b| b.is_ascii_digit()))
            {
                return None;
            }
            if words.next().is_some() || !is_topic(t) || !valid_since(since) {
                return None;
            }
            Some((
                t.to_string(),
                TopicEntry {
                    since: since.to_string(),
                },
            ))
        })
        .collect()
}

/// The reason [`parse_hold`] gives a carried hold it could not read.
pub const CARRIED_UNREADABLE: &str = "carried-unreadable";

/// The reason a keeper meta gives a carried hold whose own reason it had no room for
/// ([`KeeperCarry::rungs`]).
#[cfg(unix)]
pub const REASON_NOT_CARRIED: &str = "not-carried";

/// Every standing hold but a remote node's (a drive hold is not carried), as [`render_hold`] rows
/// in [`Holds::standing`]'s order.
pub fn render_holds(holds: &Holds) -> Vec<String> {
    holds
        .standing()
        .filter(|hold| hold.origin != REMOTE_ORIGIN)
        .map(|hold| render_hold(&hold))
        .collect()
}

/// The inverse of [`render_holds`], fail-closed as [`parse_hold`] is.
pub fn parse_holds<'a>(rows: impl IntoIterator<Item = &'a str>) -> Holds {
    let mut holds = Holds::default();
    for row in rows {
        holds.seed(parse_hold(row));
    }
    holds
}

/// The standing hold as the handoff manifest carries it (`SessionRecord::hold`).
pub fn render_hold(hold: &Hold) -> String {
    format!("{} {}", hold.origin, hold.reason)
}

/// The inverse of [`render_hold`], fail-closed.
pub fn parse_hold(row: &str) -> Hold {
    let mut words = row.split(' ');
    let origin = words.next().unwrap_or_default();
    let reason = words.next().filter(|reason| !reason.is_empty());
    let origin_word = match origin {
        PERSON_ORIGIN => PERSON_ORIGIN,
        "fleet" => "fleet",
        _ => "local",
    };
    let reason = match reason {
        Some(reason)
            if matches!(origin, PERSON_ORIGIN | "fleet" | "local") && words.next().is_none() =>
        {
            reason_token(reason)
        }
        _ => CARRIED_UNREADABLE.to_string(),
    };
    Hold {
        reason,
        origin: origin_word.to_string(),
    }
}

fn valid_since(s: &str) -> bool {
    s == "head"
        || s.strip_prefix('@').is_some_and(|n| {
            // Digits only: `u64::from_str` would accept `@+5`.
            !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) && n.parse::<u64>().is_ok()
        })
}

const TOPIC_USAGE: &str = "ERR usage: topic add <t> [since=head|@<off>] | topic drop <t> | \
                           topic ls (topic is [a-z0-9][a-z0-9._-]{0,31})\n";

/// The most broadcast topics one session opts into.
pub const MAX_TOPICS: usize = 32;

/// [`SessionFabric::topic_add`]'s refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopicsFull;

const INBOX_USAGE: &str = "ERR usage: inbox [<n>] [since=<id>] [--peek] [--meta] \
                           | inbox get <id> | inbox get @<off> \
                           | inbox seen <id> [handled|refused|deferred]\n";

/// `inbox` — the whole keyword, sub-forms included.
pub fn cmd_inbox_verb(ctx: &dyn FabricSession, rest: &str) -> String {
    let trimmed = rest.trim();
    if serving_driver().is_some() {
        if trimmed == "seen" {
            return fleet_mail::acknowledge(ctx, "");
        }
        if let Some(word) = trimmed.strip_prefix("seen ") {
            return fleet_mail::acknowledge(ctx, word);
        }
        let tail = if trimmed == "fleet" {
            ""
        } else {
            trimmed.strip_prefix("fleet ").unwrap_or(trimmed).trim()
        };
        return fleet_mail::inbox(ctx, tail, false);
    }
    match trimmed.split_once(' ') {
        Some(("fleet", tail)) => fleet_mail::inbox(ctx, tail, false),
        Some(("seen", tail)) if tail.trim_start().starts_with('f') => {
            match tail.split_whitespace().collect::<Vec<_>>().as_slice() {
                [id, "forget"] => fleet_mail::forget(ctx, id),
                _ => "ERR usage: inbox seen f<id> forget\n".to_owned(),
            }
        }
        Some(("get", tail)) => cmd_inbox_get(ctx, tail),
        Some(("seen", tail)) => cmd_inbox_seen(ctx, tail),
        None if trimmed == "fleet" => fleet_mail::inbox(ctx, "", false),
        None if matches!(trimmed, "get" | "seen") => INBOX_USAGE.to_string(),
        _ => cmd_inbox(ctx, rest),
    }
}

fn holder_token(ctx: &dyn FabricSession) -> String {
    ctx.holder_token().unwrap_or_else(|| "-".to_string())
}

/// `inbox [<n>] [since=<id>] [--peek] [--meta]`.
pub fn cmd_inbox(ctx: &dyn FabricSession, rest: &str) -> String {
    use std::fmt::Write as _;
    let mut limit: Option<usize> = None;
    let mut since = 0u64;
    let mut peek = false;
    let mut meta_only = false;
    for tok in rest.split_whitespace() {
        if let Some(v) = kv(tok, "since") {
            match parse_uint(v) {
                Some(n) => since = n,
                None => return INBOX_USAGE.to_string(),
            }
        } else if tok == "--peek" {
            peek = true;
        } else if tok == "--meta" {
            meta_only = true;
        } else if let Some(n) = parse_uint::<usize>(tok) {
            limit = Some(n);
        } else {
            return INBOX_USAGE.to_string();
        }
    }

    // With the mesh on, the ring holds the unread direct fleet mail (§11 C6): no page of the
    // mailbox follows it.
    let (direct, mut direct_unavailable) = if ctx.fabric().mesh_imports() {
        (fleet_mail::View::default(), false)
    } else {
        match fleet_mail::view(ctx, rest, true) {
            Ok(view) => (view, false),
            Err(_) => (fleet_mail::View::default(), true),
        }
    };
    let mut listed_fleet = Vec::new();
    let holder = holder_token(ctx);
    let mut inbox = ctx.fabric().lock();
    let hold_flag = u8::from(inbox.holds.is_held());
    let selected = inbox.rows.iter().filter(|r| r.id > since).count();
    let mut skip = limit.map_or(0, |n| {
        selected.saturating_sub(n.saturating_sub(direct.count))
    });

    let mut body = String::new();
    let mut sent = 0usize;
    let mut pending = direct.pending;
    for row in &mut inbox.rows {
        let mut kept = row.id > since;
        if kept && skip > 0 {
            skip -= 1;
            kept = false;
        }
        if kept {
            render_row_into(&mut body, row, meta_only);
            body.push('\n');
            sent += 1;
            if !peek {
                if !row.listed
                    && let Some(id) = row.fleet
                {
                    listed_fleet.push(id);
                }
                row.listed = true;
            }
        } else if !row.listed {
            pending += 1;
        }
    }
    for p in inbox.posts.iter().filter(|p| p.off.is_none() && !p.dead) {
        let _ = write!(
            body,
            "post {} to={} kind={} off=- len={}",
            p.id,
            p.to,
            p.kind,
            p.body.len()
        );
        if let Some(k) = &p.key {
            let _ = write!(body, " key={k}");
        }
        body.push('\n');
        sent += 1;
    }

    let oldest = inbox
        .oldest_on_bus
        .map_or_else(|| "-".to_string(), |o| format!("@{o}"));
    sent += direct.count;
    body.push_str(&direct.text);
    let mut out = format!(
        "OK {sent} hold={hold_flag} holder={holder} seen={} bus_head={} oldest_on_bus={oldest} \
         dropped={} pending={pending}",
        inbox.seen, inbox.bus_head, inbox.dropped,
    );
    let mut gaps = String::new();
    if let Some(g) = inbox.given_up {
        let _ = write!(gaps, " given_up=@{}..@{}", g.from, g.to);
        if let Some(n) = g.records {
            let _ = write!(gaps, " given_up_count={n}");
        }
    }
    if let Some(g) = inbox.lane_given_up {
        let _ = write!(gaps, " lane_given_up=@{}..@{}", g.from, g.to);
    }
    drop(inbox);
    // The listing is the import's mark: written to the sealed mailbox, so no later start imports
    // these again. One the mailbox refuses stays unlisted there, and the next start offers it
    // again rather than lose it.
    direct_unavailable |= fleet_mail::mark_listed(ctx, &listed_fleet).is_err();
    if direct_unavailable {
        out.push_str(" fleet_error=unavailable");
    }
    out.push_str(&gaps);
    out.push('\n');
    out.push_str(&body);
    out
}

fn render_row_into(out: &mut String, row: &InboxRow, meta_only: bool) {
    use std::fmt::Write as _;
    match row.fleet {
        // Direct fleet mail the mesh imported names no bus record: its mailbox id answers it.
        Some(fleet) => {
            let _ = write!(
                out,
                "msg {} off=- t={} from={} kind={} trust={} fleet=f{fleet}",
                row.id, row.t_ms, row.from, row.kind, row.trust
            );
        }
        None => {
            let _ = write!(
                out,
                "msg {} off={} t={} from={} kind={} trust={}",
                row.id, row.off, row.t_ms, row.from, row.kind, row.trust
            );
        }
    }
    if let Some(re) = row.re {
        let _ = write!(out, " re={re}");
        if let Some(re_id) = row.re_id {
            let _ = write!(out, " re-id={re_id}");
        }
    }
    if let Some(v) = &row.verdict {
        let _ = write!(out, " verdict={v}");
    }
    if let Some(dl) = row.dl {
        let _ = write!(out, " dl={dl}");
    }
    if row.late {
        out.push_str(" late=1");
    }
    if let Some(d) = &row.demoted {
        let _ = write!(out, " demoted={d}");
    }
    if let Some(v) = &row.via {
        let _ = write!(out, " via={v}");
    }
    if let Some(t) = &row.topic {
        let _ = write!(out, " topic={t}");
    }
    let _ = write!(out, " len={}", row.len);
    let truncated = row.len > row.body.len();
    if row.body.len() > TEXT_PREVIEW_MAX || truncated {
        out.push_str(" more=1");
    }
    if truncated {
        out.push_str(" truncated=1");
    }
    if !meta_only {
        let mut end = TEXT_PREVIEW_MAX.min(row.body.len());
        while end > 0 && !row.body.is_char_boundary(end) {
            end -= 1;
        }
        out.push_str(" text=");
        aterm_control::wire::pct_encode_into(out, &row.body[..end]);
    }
}

/// `inbox get <id>`.
pub fn cmd_inbox_get(ctx: &dyn FabricSession, rest: &str) -> String {
    let mut toks = rest.split_whitespace();
    let (Some(arg), None) = (toks.next(), toks.next()) else {
        return INBOX_USAGE.to_string();
    };
    if let Some(off) = arg.strip_prefix('@') {
        return match parse_uint::<u64>(off) {
            Some(off) => inbox_get_at(ctx, off),
            None => INBOX_USAGE.to_string(),
        };
    }
    let Some(id) = parse_uint::<u64>(arg) else {
        return INBOX_USAGE.to_string();
    };
    let inbox = ctx.fabric().lock();
    match inbox.rows.iter().find(|r| r.id == id) {
        Some(row) if row.len > row.body.len() => format!(
            "OK {} truncated=1 len={}\n{}",
            row.body.len(),
            row.len,
            row.body
        ),
        Some(row) => format!("OK {}\n{}", row.body.len(), row.body),
        None => "ERR no such message\n".to_string(),
    }
}

fn inbox_get_at(ctx: &dyn FabricSession, off: u64) -> String {
    inbox_get_at_within(ctx, off, fetch_wait())
}

fn inbox_get_at_within(ctx: &dyn FabricSession, off: u64, wait: std::time::Duration) -> String {
    let mut inbox = ctx.fabric().lock();
    if let Some(row) = inbox.row_at(off)
        && (row.len <= row.body.len() || wait_is_futile())
    {
        return Fetched::from_row(row).render();
    }
    match inbox.fetches.iter().position(|(o, _)| *o == off) {
        Some(i) => match &inbox.fetches[i].1 {
            FetchSlot::Done(f) => return f.render(),
            FetchSlot::Failed(why) => {
                let why = why.clone();
                inbox.fetches.remove(i);
                return unfetched(&inbox, off, fetch_failure(&why, off));
            }
            FetchSlot::Pending | FetchSlot::Partial(_) => {}
        },
        None => {
            if wait_is_futile() {
                return format!("ERR fabric {} off={off}\n", fabric_state());
            }
            evict_answered_fetch(&mut inbox);
            if inbox.fetches.len() >= FETCH_SLOTS {
                return format!(
                    "ERR busy: {FETCH_SLOTS} inbox reads already waiting; retry when one answers\n"
                );
            }
            inbox.fetches.push_back((off, FetchSlot::Pending));
            let _ = ctx.record_fact("fetch", FabricFact::Fetch { off });
        }
    }
    let deadline = deadline_after(wait);
    loop {
        let Some(i) = inbox.fetches.iter().position(|(o, _)| *o == off) else {
            // Evicted from under a parked read, though only an answered slot is evicted and this
            // one was pending: a reader that cannot tell says "nothing" rather than hang.
            return fetch_failure("no-record", off);
        };
        match &inbox.fetches[i].1 {
            FetchSlot::Done(f) => return f.render(),
            FetchSlot::Failed(why) => {
                let why = why.clone();
                inbox.fetches.remove(i);
                return unfetched(&inbox, off, fetch_failure(&why, off));
            }
            FetchSlot::Pending | FetchSlot::Partial(_) => {}
        }
        if wait_is_futile() {
            inbox.fetches.remove(i);
            let why = format!("ERR fabric {} off={off}\n", fabric_state());
            return unfetched(&inbox, off, why);
        }
        if std::time::Instant::now() >= deadline || aterm_control::hangup::caller_hung_up() {
            inbox.fetches.remove(i);
            return unfetched(&inbox, off, format!("ERR timeout off={off}\n"));
        }
        inbox = ctx.fabric().changed.park(
            inbox,
            park_until(deadline),
            aterm_control::park::Lane::SERVED,
        );
    }
}

fn unfetched(inbox: &Inbox, off: u64, why: String) -> String {
    match inbox.row_at(off) {
        Some(row) => Fetched::from_row(row).render(),
        None => why,
    }
}

fn fetch_failure(why: &str, off: u64) -> String {
    if why == "no-record" {
        format!("ERR no such record off={off}\n")
    } else if let Some(floor) = why.strip_prefix("retained-from=") {
        format!("ERR retained-from={floor} off={off}\n")
    } else {
        format!("ERR fetch failed reason={why} off={off}\n")
    }
}

/// `inbox seen <id> [handled|refused|deferred]`.
pub fn cmd_inbox_seen(ctx: &dyn FabricSession, rest: &str) -> String {
    let mut toks = rest.split_whitespace();
    let Some(id) = toks.next().and_then(parse_uint::<u64>) else {
        return INBOX_USAGE.to_string();
    };
    let verdict = match toks.next() {
        None => None,
        Some(v) if VERDICTS.contains(&v) => Some(v),
        Some(_) => return INBOX_USAGE.to_string(),
    };
    if toks.next().is_some() {
        return INBOX_USAGE.to_string();
    }
    let mut inbox = ctx.fabric().lock();
    if verdict.is_some() {
        match ctx.fabric().wait_unfenced(inbox, ctx.fabric().fence_wait()) {
            Some(guard) => inbox = guard,
            None => return HOLD_BUSY.to_string(),
        }
    }
    let Some(at) = inbox.rows.iter().position(|r| r.id == id) else {
        return "ERR no such message\n".to_string();
    };
    let off = inbox.rows[at].off;
    let earlier_log = !inbox.rows[at].on_log;
    let decided = verdict.filter(|v| inbox.rows[at].decision.as_deref() != Some(v));
    if let Some(v) = decided {
        let row = &mut inbox.rows[at];
        row.decision = Some(v.to_string());
        // Direct fleet mail is answered by `post re=f<id>`, which its sender reads on its next
        // drive: no bus receipt reaches it (§11 C6).
        if matches!(row.kind.as_str(), "ask" | "task") && row.fleet.is_none() {
            let (kind, from) = (row.kind.clone(), row.from.clone());
            owe_receipt(&mut inbox, off, v, &kind, &from, earlier_log);
        }
    }
    let seen_before = inbox.seen;
    inbox.seen = inbox.seen.max(id);
    let mut listed_fleet = Vec::new();
    for row in &mut inbox.rows {
        if row.id <= id {
            if !row.listed
                && let Some(fleet) = row.fleet
            {
                listed_fleet.push(fleet);
            }
            row.listed = true;
        }
    }
    let seen = inbox.seen;
    let floor_at = addressed_floor(&inbox);
    // A direct fleet message closes by its own fact, a reply's or a retirement's
    // (`FleetMailSeen`), as it does with the mesh off: its listing here moves only the bus
    // messages the watermark passed, so the session log does not close it twice.
    let bus_passed = inbox
        .rows
        .iter()
        .any(|r| r.id > seen_before && r.id <= id && r.fleet.is_none());
    let row = &inbox.rows[at];
    let fact = FabricFact::MailSeen {
        inbox: id,
        off: row.fleet.is_none().then_some(off),
        floor: floor_at,
        seen,
        verdict: decided,
        kind: &row.kind,
        from: &row.from,
        moved: bus_passed || (decided.is_some() && row.fleet.is_none()),
    };
    let _ = ctx.record_fact("inbox-seen", fact);
    // As `inbox`'s listing does: a mark the mailbox refuses leaves the message for the next
    // start to offer again. A waiter the ring wakes before the mark is made may read the mailbox
    // unmarked; one that parks again then is woken once the mark is made.
    ctx.fabric()
        .changed
        .ring_after(&ctx.fabric().inbox, inbox, || {
            let _ = fleet_mail::mark_listed(ctx, &listed_fleet);
        });
    ctx.fabric_changed();
    format!("OK seen={seen}\n")
}

fn addressed_floor(inbox: &Inbox) -> Option<u64> {
    inbox
        .rows
        .iter()
        .filter(|r| r.topic.is_none() && r.id > inbox.seen && r.on_log)
        .map(|r| r.off)
        .min()
        .or_else(|| inbox.addressed_head.map(|h| h.saturating_add(1)))
}

const POST_USAGE: &str = "ERR usage: post to=<@<sid>[@<node>]|<principal>|say[:<topic>]> \
                          kind=<ask|answer|task|report|note|ack|control> [re=[@]<off>] \
                          [dl=<ms>] [via=<p,...>] [key=<token>] [--wait[=<ms>]] \
                          [--wait-ack[=<ms>]] ([--] <text> | len=<n> + <n> raw bytes)\n";

const POST_LEN_USAGE: &str = "ERR usage: post len=<n> must be a byte count\n";

/// Parse a `post` `len=` value.
pub fn post_len(v: &str) -> Result<usize, String> {
    match parse_uint::<usize>(v) {
        Some(n) if n <= BODY_MAX => Ok(n),
        _ if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => Err(post_body_too_large()),
        _ => Err(POST_LEN_USAGE.to_string()),
    }
}

fn post_body_too_large() -> String {
    format!("ERR too large: a post body is capped at {BODY_MAX} bytes\n")
}

fn post_body_offset(rest: &str) -> Option<usize> {
    let mut idx = 0usize;
    for tok in rest.split_whitespace() {
        let at = idx + rest[idx..].find(tok)?;
        if tok == POST_OPTIONS_END {
            let after = at + tok.len();
            let skip = rest[after..].len() - rest[after..].trim_start().len();
            return (after + skip < rest.len()).then_some(after + skip);
        }
        if !is_post_option(tok) {
            return Some(at);
        }
        idx = at + tok.len();
    }
    None
}

const WAIT_DEFAULT_MS: u64 = 30_000;
const WAIT_MAX_MS: u64 = 600_000;

/// `post` — queue one outbound message and, for the kinds that need a correlation id, wait for
/// the offset it landed at.
pub fn cmd_post(ctx: &dyn FabricSession, rest: &str, body: Option<Vec<u8>>) -> String {
    let began = handler_tally::stamp();
    if serving_driver().is_some() || fleet_mail::is_reply(rest) {
        return if body.is_none() {
            fleet_mail::post(ctx, rest)
        } else {
            "ERR usage: direct fleet mail uses inline text\n".to_owned()
        };
    }
    let mut to: Option<String> = None;
    let mut kind: Option<String> = None;
    let mut re: Option<u64> = None;
    let mut dl: Option<u64> = None;
    let mut via: Option<String> = None;
    let mut key: Option<String> = None;
    let mut wait: Option<Option<u64>> = None;
    let mut wait_ack: Option<Option<u64>> = None;
    let mut declared_len: Option<usize> = None;
    let body_at = post_body_offset(rest);
    for tok in post_options(rest.split_whitespace()) {
        if let Some(v) = kv(tok, "to") {
            to = Some(v.to_string());
        } else if let Some(v) = kv(tok, "kind") {
            kind = Some(v.to_string());
        } else if let Some(v) = kv(tok, "re") {
            match parse_bus_offset(v) {
                Some(n) => re = Some(n),
                None => return POST_USAGE.to_string(),
            }
        } else if let Some(v) = kv(tok, "dl") {
            match parse_uint(v) {
                Some(n) => dl = Some(n),
                None => return POST_USAGE.to_string(),
            }
        } else if let Some(v) = kv(tok, "via") {
            via = Some(v.to_string());
        } else if let Some(v) = kv(tok, "key") {
            // The key is bounded at the door, like `to`: it is printed onto the `outbox` line and
            // into the bridge's state dir, so it keeps to the charset and length of the ids there.
            if !valid_key(v) {
                return POST_USAGE.to_string();
            }
            key = Some(v.to_string());
        } else if let Some(v) = kv(tok, "len") {
            match post_len(v) {
                Ok(n) => declared_len = Some(n),
                Err(reply) => return reply,
            }
        } else if tok == "--wait" {
            wait = Some(None);
        } else if let Some(v) = kv(tok, "--wait") {
            match parse_uint::<u64>(v) {
                Some(n) => wait = Some(Some(n.min(WAIT_MAX_MS))),
                None => return POST_USAGE.to_string(),
            }
        } else if tok == "--wait-ack" {
            wait_ack = Some(None);
        } else if let Some(v) = kv(tok, "--wait-ack") {
            match parse_uint::<u64>(v) {
                Some(n) => wait_ack = Some(Some(n.min(WAIT_MAX_MS))),
                None => return POST_USAGE.to_string(),
            }
        }
    }
    let (Some(to), Some(kind)) = (to, kind) else {
        return POST_USAGE.to_string();
    };
    if wait_ack.is_some() && !matches!(kind.as_str(), "ask" | "task") {
        return POST_USAGE.to_string();
    }
    if !POSTABLE.contains(&kind.as_str()) {
        return POST_USAGE.to_string();
    }
    if to == "fleet" {
        return "ERR denied: no to=fleet — an agent may only ask a human to halt\n".to_string();
    }
    if let Some(topic) = to.strip_prefix("say:") {
        if !is_topic(topic) {
            return POST_USAGE.to_string();
        }
    } else if to != "say" && !valid_principal(to.strip_prefix('@').unwrap_or(&to)) {
        return POST_USAGE.to_string();
    }
    if let Some(v) = &via
        && !via_ok(v)
    {
        return POST_USAGE.to_string();
    }
    let inline = body.is_none() && declared_len.is_none();
    let body = match (body, declared_len) {
        (Some(bytes), _) => bytes,
        (None, Some(_)) => {
            return "ERR usage: post len=<n> must be followed by <n> raw bytes\n".to_string();
        }
        (None, None) => {
            let text = body_at
                .map_or("", |at| &rest[at..])
                .trim_end_matches(['\r', '\n'])
                .to_string();
            if text.is_empty() {
                return POST_USAGE.to_string();
            }
            if text.len() > POST_INLINE_MAX {
                return format!(
                    "ERR too large: inline post text is capped at {POST_INLINE_MAX} bytes; send it \
                     with len=<n>\n"
                );
            }
            text.into_bytes()
        }
    };
    if body.len() > BODY_MAX {
        return post_body_too_large();
    }
    let body = String::from_utf8_lossy(&body).into_owned();

    // With the mesh on, a session on a Mac that does not speak the mesh mail route (a pre-R3
    // Mac) is reached the way it reads mail: by direct fleet mail over a fleet drive (§11 C6).
    if ctx.fabric().mesh_imports()
        && let Some((sid, node)) = to.strip_prefix('@').and_then(|rest| rest.split_once('@'))
        && ctx.peer_mail(node) == fleet_mail::PeerMail::Fleet
    {
        return fleet_mail::carry(
            ctx,
            &fleet_mail::Carry {
                sid,
                node,
                kind: &kind,
                key: key.as_deref(),
                text: &body,
                inline,
                plain: re.is_none() && dl.is_none() && via.is_none() && wait_ack.is_none(),
            },
        );
    }

    let wait = wait.or_else(|| {
        (matches!(kind.as_str(), "ask" | "task") || wait_ack.is_some()).then_some(None)
    });

    let Some(mut inbox) = ctx
        .fabric()
        .wait_unfenced(ctx.fabric().lock(), ctx.fabric().fence_wait())
    else {
        return HOLD_BUSY.to_string();
    };
    let (queued, queued_bytes) = inbox.queued_load();
    let incoming = caller_sized_bytes(&body, via.as_deref());
    if queued >= OUTBOX_CAP || queued_bytes.saturating_add(incoming) > OUTBOX_BYTES_MAX {
        return format!("ERR outbox full queued={queued} bytes={queued_bytes}\n");
    }
    inbox.next_post_id += 1;
    inbox.outbox_moves += 1;
    let id = inbox.next_post_id;
    if to == "say" || to.starts_with("say:") {
        ctx.fabric()
            .note_topic_post(to.strip_prefix("say:").unwrap_or("say"), &body);
    }
    inbox.posts.push_back(PostRow {
        id,
        to: pct_encode(&to),
        kind: kind.clone(),
        off: None,
        landed_on_log: false,
        re,
        dl,
        via: via.clone(),
        key: key.clone(),
        dup: false,
        dead: false,
        dead_reason: String::new(),
        body,
    });
    inbox.trim_retired_posts();
    note_work_owed();
    let fact = FabricFact::MailOut {
        kind: &kind,
        to: &to,
        post: id,
        re,
        dl,
        via: via.as_deref(),
        idem: key.as_deref(),
    };
    let _ = ctx.record_fact("post", fact);
    ctx.fabric().changed.ring(inbox);
    ctx.fabric_changed();
    handler_tally::note_since(handler_tally::Row::Post, began);

    let Some(wait) = wait else {
        return format!("OK {id}\n");
    };
    if wait_is_futile() {
        return fabric_wait_refusal(id);
    }
    let deadline = deadline_after(std::time::Duration::from_millis(
        wait.unwrap_or(WAIT_DEFAULT_MS),
    ));
    let (off, dup, watch) = match wait_landing(ctx, id, deadline) {
        Ok(landed) => landed,
        Err(reply) => return reply,
    };
    let Some(wait_ack) = wait_ack else {
        return format!("OK {id} off={off}{dup}\n");
    };
    let bound = match wait_ack {
        Some(ms) => ms,
        None => dl.map_or(WAIT_DEFAULT_MS, |d| d.saturating_add(expiry_grace_ms())),
    }
    .min(WAIT_MAX_MS);
    wait_receipt_on_log(ctx, id, dup, &watch, bound)
}

fn wait_landing(
    ctx: &dyn FabricSession,
    id: u64,
    deadline: std::time::Instant,
) -> Result<(u64, &'static str, Arc<OffsetWatch>), String> {
    let mut guard = ctx.fabric().lock();
    loop {
        let row = guard.posts.iter().find(|p| p.id == id);
        if let Some(row) = row
            && let Some(off) = row.off
        {
            let dup = if row.dup { " dup=1" } else { "" };
            let on_log = row.landed_on_log;
            return Ok((off, dup, guard.watch(off, on_log)));
        }
        if let Some(row) = row.filter(|p| p.dead) {
            let reason = if row.dead_reason.is_empty() {
                DEAD_DEFAULT
            } else {
                row.dead_reason.as_str()
            };
            return Err(format!("ERR {reason} id={id}\n"));
        }
        if wait_is_futile() {
            return Err(fabric_wait_refusal(id));
        }
        if aterm_control::hangup::caller_hung_up() {
            return Err(aterm_control::hangup::HUNG_UP_REPLY.to_string());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("ERR timeout id={id}\n"));
        }
        guard = ctx.fabric().changed.park(
            guard,
            park_until(deadline),
            aterm_control::park::Lane::SERVED,
        );
    }
}

#[cfg(test)]
fn wait_receipt(ctx: &dyn FabricSession, id: u64, off: u64, dup: &str, bound_ms: u64) -> String {
    let watch = {
        let mut inbox = ctx.fabric().lock();
        let on_log = inbox
            .posts
            .iter()
            .find(|p| p.id == id)
            .is_none_or(|p| p.landed_on_log);
        inbox.watch(off, on_log)
    };
    wait_receipt_on_log(ctx, id, dup, &watch, bound_ms)
}

fn deadline_after(wait: std::time::Duration) -> std::time::Instant {
    let now = std::time::Instant::now();
    now.checked_add(wait)
        .unwrap_or_else(|| now + std::time::Duration::from_secs(365 * 24 * 3600))
}

fn wait_receipt_on_log(
    ctx: &dyn FabricSession,
    id: u64,
    dup: &str,
    post: &OffsetWatch,
    bound_ms: u64,
) -> String {
    let off = post.off;
    let deadline = deadline_after(std::time::Duration::from_millis(bound_ms));
    let mut guard = ctx.fabric().lock();
    loop {
        // The post's own record must still be on the log (a moved generation below the fork
        // keeps it).
        if !post.on_log() {
            return format!("ERR log-replaced id={id} off={off}\n");
        }
        let reply = guard.rows.iter().rev().find(|r| {
            r.on_log && r.re == Some(off) && matches!(r.kind.as_str(), "ack" | "expired")
        });
        if let Some(row) = reply {
            if row.kind == "expired" {
                return format!("ERR expired id={id} off={off}\n");
            }
            let verdict = row.verdict.as_deref().unwrap_or("-");
            return format!("OK {id} off={off}{dup} ack={verdict} msg={}\n", row.id);
        }
        if wait_is_futile() {
            return format!("ERR fabric {} id={id} off={off}\n", fabric_state());
        }
        if aterm_control::hangup::caller_hung_up() {
            return aterm_control::hangup::HUNG_UP_REPLY.to_string();
        }
        if std::time::Instant::now() >= deadline {
            return format!("ERR timeout id={id} off={off}\n");
        }
        guard = ctx.fabric().changed.park(
            guard,
            park_until(deadline),
            aterm_control::park::Lane::SERVED,
        );
    }
}

fn park_until(deadline: std::time::Instant) -> std::time::Instant {
    let horizon = stale_in()
        .filter(|d| !d.is_zero())
        .unwrap_or_else(stale_after);
    deadline.min(deadline_after(horizon))
}

fn fabric_wait_refusal(id: u64) -> String {
    let state = fabric_state();
    if bridge_reachable() {
        format!("ERR fabric {state} id={id} queued=1\n")
    } else {
        format!("ERR fabric {state} id={id} no-bridge=1\n")
    }
}

fn wait_is_futile() -> bool {
    match LINK.state.load(Ordering::Relaxed) {
        FABRIC_CONNECTED if link_is_stale() => true,
        FABRIC_CONNECTED => false,
        FABRIC_STALLED => {
            let link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
            link.reported || link.reason != LINK_STARTING
        }
        _ => true,
    }
}

/// `await inbox since=<id> [kinds=<k|hold,...>] [topic=<t>] | await inbox re=<off> [since=<id>]
/// [kinds=<k|hold,...>] [topic=<t>]` (the caller strips `timeout=<ms>`).
pub fn cmd_await_inbox(ctx: &dyn FabricSession, args: &[&str], timeout_ms: u64) -> String {
    let mut spans = handler_tally::AwaitSpans::begin(timeout_ms);
    const USAGE: &str = "ERR usage: await inbox since=<id|f-id> [fleet_since=f<id>] [kinds=<k|hold,...>] [topic=<t>] \
                         [timeout=<ms>] | await inbox re=<off> [since=<id>] [kinds=<k|hold,...>] \
                         [topic=<t>] [timeout=<ms>]\n";
    let mut since: Option<u64> = None;
    let mut fleet_since: Option<u64> = None;
    let mut direct_only = false;
    let mut re: Option<u64> = None;
    let mut kinds: Option<Vec<String>> = None;
    let mut topic: Option<String> = None;
    for tok in args {
        if let Some(v) = kv(tok, "since") {
            if since.is_some() {
                return USAGE.to_string();
            }
            if let Some(value) = v.strip_prefix('f').and_then(parse_uint) {
                if fleet_since.replace(value).is_some() {
                    return USAGE.to_string();
                }
                direct_only = true;
                since = Some(0);
                continue;
            }
            match parse_uint(v) {
                Some(n) => since = Some(n),
                None => return USAGE.to_string(),
            }
        } else if let Some(value) = kv(tok, "fleet_since")
            .and_then(|s| s.strip_prefix('f'))
            .and_then(parse_uint)
        {
            if direct_only || fleet_since.replace(value).is_some() {
                return USAGE.to_string();
            }
        } else if let Some(v) = kv(tok, "re") {
            match parse_bus_offset(v) {
                Some(n) if re.replace(n).is_none() => {}
                _ => return USAGE.to_string(),
            }
        } else if let Some(v) = kv(tok, "kinds") {
            if kinds.is_some() {
                return USAGE.to_string();
            }
            let list: Vec<String> = v.split(',').map(str::to_string).collect();
            if list
                .iter()
                .any(|k| !KINDS.contains(&k.as_str()) && k != "hold")
            {
                return USAGE.to_string();
            }
            kinds = Some(list);
        } else if let Some(v) = kv(tok, "topic") {
            // `is_topic` is the grammar `post to=say:<t>` and `topic add` take: no row carries a
            // topic outside it, so a wait on one could only time out.
            if !is_topic(v) || topic.replace(v.to_string()).is_some() {
                return USAGE.to_string();
            }
        } else {
            return USAGE.to_string();
        }
    }
    let since = match (since, re) {
        (Some(s), _) => s,
        (None, Some(_)) => 0,
        (None, None) => return USAGE.to_string(),
    };
    if (direct_only || fleet_since.is_some()) && (re.is_some() || topic.is_some()) {
        return USAGE.to_owned();
    }
    let fleet_since = fleet_since.unwrap_or(0);
    let watch_hold = kinds
        .as_ref()
        .is_some_and(|k| k.iter().any(|k| k == "hold"));
    let accepts = |kind: &str| match &kinds {
        Some(list) => list.iter().any(|k| k == kind),
        None => re.is_some() || topic.is_some() || kind != AWAIT_DEFAULT_SKIP,
    };
    let latches = |r: &InboxRow| {
        r.id > since
            && accepts(&r.kind)
            && re.is_none_or(|n| r.on_log && r.re == Some(n))
            && (topic.is_none() || r.topic == topic)
    };

    let deadline = deadline_after(std::time::Duration::from_millis(timeout_ms));
    let mut guard = ctx.fabric().lock();
    if since > guard.last_msg_id {
        return format!("ERR bad since high={}\n", guard.last_msg_id);
    }
    let hold_at_arm = guard.holds.is_held();
    let named = re.map(|off| guard.watch(off, true));
    // The named post's record must still be on the log (a moved generation below the fork keeps
    // it), and then a latching row answers. Both are asked under the guard before the direct
    // mailbox is read, and again after, so a bus answer reads no file.
    let latched = |inbox: &mut Inbox, spans: &handler_tally::AwaitSpans| {
        if let Some(post) = &named
            && !post.on_log()
        {
            return Some(format!("ERR log-replaced re={}\n", post.off));
        }
        let id = (!direct_only)
            .then(|| inbox.rows.iter().filter(|r| latches(r)).map(|r| r.id).max())
            .flatten()?;
        spans.answers(&mut inbox.arrived);
        Some(format!("OK inbox {id}\n"))
    };
    loop {
        if let Some(reply) = latched(&mut guard, &spans) {
            return reply;
        }
        // With the mesh on, the ring holds the unread direct mail (§11 C6), and its rows latch
        // above; only a wait on direct mail alone reads the mailbox.
        let direct =
            if re.is_none() && topic.is_none() && (direct_only || !ctx.fabric().mesh_imports()) {
                let epoch = ctx.fabric().direct_epoch.load(Ordering::Acquire);
                drop(guard);
                let direct = fleet_mail::pending(ctx, fleet_since, accepts);
                guard = ctx.fabric().lock();
                if epoch != ctx.fabric().direct_epoch.load(Ordering::Acquire) {
                    continue;
                }
                if let Some(reply) = latched(&mut guard, &spans) {
                    return reply;
                }
                direct
            } else {
                Ok(None)
            };
        let direct = match direct {
            Ok(pending) => pending,
            Err(why) if direct_only => return fleet_mail::refusal(why),
            Err(_) => None,
        };
        if let Some(id) = direct {
            return format!("OK inbox {} fleet=f{id}\n", guard.last_msg_id);
        }
        if watch_hold && guard.holds.is_held() != hold_at_arm {
            return format!("OK inbox hold={}\n", u8::from(guard.holds.is_held()));
        }
        if guard.closed {
            return "ERR exited\n".to_string();
        }
        if aterm_control::hangup::caller_hung_up() {
            return aterm_control::hangup::HUNG_UP_REPLY.to_string();
        }
        if std::time::Instant::now() >= deadline {
            return "OK timeout\n".to_string();
        }
        // Another aterm process may own the same durable mailbox during handoff. Its writes
        // cannot signal this process's condvar; the session's fleet-mail watcher sees the
        // file's stamp move, and its refresh rings it ([`fleet_mail::restore`]). Nothing else
        // ends the park before the wait's own deadline.
        spans.parks();
        guard = ctx.fabric().park_inbox(guard, deadline);
    }
}

#[cfg(test)]
#[path = "fabric_person_tests.rs"]
mod person_tests;
#[cfg(test)]
#[path = "fabric_remote_tests.rs"]
mod remote_tests;
#[cfg(test)]
#[path = "lib_test_support_tests.rs"]
mod test_support;

#[cfg(test)]
#[path = "lib_mirror_request_tests.rs"]
mod mirror_request_tests;

#[cfg(test)]
#[path = "lib_topic_tests.rs"]
mod topic_tests;

#[cfg(test)]
#[path = "lib_inbox_hold_tests.rs"]
mod inbox_hold;
