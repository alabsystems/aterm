// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session fabric endpoint: bounded [`Inbox`] ring, outbound post queue, [`Holds`] gate,
//! [`SessionFabric`] verb handlers (`inbox`, `post`, `deliver`, `hold`, `topic`), and instance `LINK` state.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock, Weak};

use aterm_control::wire::pct_encode;
use aterm_session::SessionId;
use aterm_types::fabric_grammar::{KINDS, VERDICTS, is_principal, is_topic, via_ok};

use aterm_gui_timing::now_ms;

/// What the fabric needs of a session.
pub trait FabricSession: Send + Sync {
    /// The session's stable id.
    fn self_id(&self) -> &SessionId;
    /// The session's fabric endpoint.
    fn fabric(&self) -> &SessionFabric;
    /// Record one event on the session's timeline.
    fn record(&self, kind: &'static str, payload: String);
    /// [`Self::record`], with what the record says typed ([`FabricFact`]) for the session log's mark beside it, written in the same timeline hold.
    fn record_fact(&self, kind: &'static str, payload: String, fact: FabricFact<'_>) -> bool {
        let _ = fact;
        self.record(kind, payload);
        false
    }
    /// Tell the presence band this session's mail or hold moved.
    fn fabric_changed(&self);
    /// The `holder=` token of `inbox`.
    fn holder_token(&self) -> Option<String>;
}

impl<T: FabricSession + ?Sized> FabricSession for Arc<T> {
    fn self_id(&self) -> &SessionId {
        (**self).self_id()
    }
    fn fabric(&self) -> &SessionFabric {
        (**self).fabric()
    }
    fn record(&self, kind: &'static str, payload: String) {
        (**self).record(kind, payload);
    }
    fn record_fact(&self, kind: &'static str, payload: String, fact: FabricFact<'_>) -> bool {
        (**self).record_fact(kind, payload, fact)
    }
    fn fabric_changed(&self) {
        (**self).fabric_changed();
    }
    fn holder_token(&self) -> Option<String> {
        (**self).holder_token()
    }
}

/// A session's timeline, as the fabric writes it.
pub trait Timeline {
    /// Append one `kind` record carrying `payload`.
    fn record(&mut self, kind: &'static str, payload: String);
    /// [`Self::record`] with its typed [`FabricFact`] (see [`FabricSession::record_fact`]).
    fn record_fact(&mut self, kind: &'static str, payload: String, fact: FabricFact<'_>) -> bool {
        let _ = fact;
        self.record(kind, payload);
        false
    }
}

/// What a fabric record says, typed, for the session log's mark beside it ([`FabricSession::record_fact`]), so nothing parses a timeline payload back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FabricFact<'a> {
    /// A row delivered to this session's inbox.
    MailIn {
        kind: &'a str,
        from: &'a str,
        off: u64,
        inbox: u64,
    },
    /// `inbox seen <id>`, every one.
    MailSeen {
        inbox: u64,
        off: u64,
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
    /// An outbound post retired.
    MailLanded {
        post: u64,
        off: Option<u64>,
        why: Option<&'static str>,
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
    /// A broadcast opt-in moved (`topic add <t> since=<s>`, `topic drop <t>`; phase 6, 6F-5).
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
    /// A parked `inbox get @<off>` asked the bridge for the record at `off` (the `fetch` row the bridge drains).
    Fetch { off: u64 },
}

/// What the two bridge verbs need of the registry.
pub trait Registry: Send + Sync + 'static {
    /// Every live session, in local-id order (the order `outbox` drains them).
    fn sessions(&self) -> Vec<Arc<dyn FabricSession>>;
    /// The live session with this id, if any.
    fn session(&self, sid: &SessionId) -> Option<Arc<dyn FabricSession>>;
    /// A lost bridge left `sessions` running with an advisory instead of halting them ([`bridge_lost`]).
    fn bridge_lost_advised(&self, sessions: usize);
    /// The bridge that owns the link moved `files` state files it found torn after an unclean shutdown into its state dir's quarantine (`link set-aside`, [`cmd_link`]).
    fn torn_state_set_aside(&self, files: u64, told_when_empty: bool);
    /// The bridge that owns the link says its quarantine still holds `files` of the torn files it set aside (`link quarantine`, [`cmd_link`]).
    fn torn_state_held(&self, files: u64);
}

/// The registry as the fabric holds it.
pub type Store = Arc<RwLock<dyn Registry>>;

/// A registry handle the bridge verbs take.
pub trait AsStore {
    /// The [`Store`] this handle is (a reference count, not a copy).
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

/// How many outbound posts one session may hold waiting for the bridge, and how many bytes of retained body they may hold together.
pub const OUTBOX_CAP: usize = 128;
pub const OUTBOX_BYTES_MAX: usize = 4 * 1024 * 1024;

const OUTBOX_DRAIN_BYTES_MAX: usize = OUTBOX_BYTES_MAX;

const RETIRED_KEEP: usize = 512;

const DEDUP_CAP: usize = 2 * RING_CAP;

const REASON_MAX: usize = 128;

/// The seven session-timeline record kinds the fabric writes, which are also their wire names on the `events` digest (`EVENT <local> inbox <id> …`).
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

/// Return `true` iff a peer with `unlisted_from_peer` unlisted inbox rows has room under [`SENDER_QUOTA`].
#[must_use]
#[inline]
pub const fn sender_within_quota(unlisted_from_peer: usize) -> bool {
    unlisted_from_peer < SENDER_QUOTA
}

/// Return `true` iff adding `add_bytes` to an outbox with `(queued_posts, queued_bytes)` stays within [`OUTBOX_CAP`], [`BODY_MAX`], and [`OUTBOX_BYTES_MAX`].
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
    /// One of [`TRUSTS`]: the receiver's verdict on what the content is.
    pub trust: String,
    /// The offset this message answers, when it answers one.
    pub re: Option<u64>,
    /// The local post id `re` resolves to through the post table, when the answer is to something this session sent.
    pub re_id: Option<u64>,
    /// Advisory deadline in ms.
    pub dl: Option<u64>,
    /// The answer arrived after the deadline had already been recorded expired.
    pub late: bool,
    /// On a `kind=ack` row: the recipient's verdict on the `ask`/`task` at `re=`.
    pub verdict: Option<String>,
    /// This session's verdict on the row, as `inbox seen <id> <verdict>` last recorded it.
    pub decision: Option<String>,
    /// The kind this message claimed before the receiver demoted it (a relayed row is always `note`, whatever it says it is).
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
    /// A broadcast the delivering bridge offers again to a successor that keeps this sid while it is unhandled.
    pub refill: bool,
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
    } else if from.starts_with("s-") {
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
    /// The offset this post answers, carried through to the bridge so the published record's body can say `re=`.
    pub re: Option<u64>,
    /// Advisory deadline in ms, carried through the same way.
    pub dl: Option<u64>,
    /// The relay chain, when the sender declared one.
    pub via: Option<String>,
    /// The caller's idempotency key (`post key=<token>`), carried to the bridge on every `outbox` drain.
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
    /// `person` (the person at the window took the tab over: Take Over, [`HoldIssuer::Person`]), `fleet` (a human's halt through the bridge, or a lost bridge) or `local` (the instance Owner's own halt, over the local control socket).
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
    static DRIVING: std::cell::RefCell<Option<Driver>> = const { std::cell::RefCell::new(None) };
}

/// Marks the connection this thread serves as a remote node's drive connection until dropped.
pub struct DrivingAs(Option<Driver>);

impl DrivingAs {
    /// Serve as `driver` until the guard drops.
    #[must_use]
    pub fn enter(driver: Driver) -> Self {
        Self(DRIVING.with(|driving| driving.replace(Some(driver))))
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
    DRIVING.with(|driving| driving.borrow().clone())
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

    /// Whose hold the session shows (`status hold_by=`, the survey's `hold_by=`, the need classifier's [`aterm_types::needs::HoldWord`]).
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

    /// Every standing hold, the person's first, then a remote node's, then the fleet's.
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

    fn event(&self, lifted: &str) -> String {
        let (origin, reason, hold_origin) = self.event_fields(lifted);
        format!(
            "{} reason={} origin={origin} hold_origin={hold_origin}",
            u8::from(reason.is_some()),
            reason.as_deref().unwrap_or("-"),
        )
    }

    fn event_fields(&self, lifted: &str) -> (String, Option<String>, String) {
        match self.first() {
            Some(first) => (first.origin, Some(first.reason), self.origins()),
            None => (lifted.to_string(), None, self.origins()),
        }
    }
}

/// The `reason=` of the fleet hold a lost bridge leaves under `[fabric] hold_on_bridge_lost = true` ([`bridge_lost`]).
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
    dedup: VecDeque<u64>,
    dedup_set: HashSet<u64>,
    holds: Holds,
    advisory: Option<(Hold, u64)>,
    hold_fenced: bool,
    retirements_fenced: bool,
    fetches: VecDeque<(u64, FetchSlot)>,
    watches: Vec<Weak<OffsetWatch>>,
    bus_log: Option<String>,
    receipts: VecDeque<OwedReceipt>,
    next_receipt_id: u64,
    outbox_moves: u64,
    msg_id_ceiling: Option<u64>,
    closed: bool,
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
    /// The log the carried receipts and reply references name.
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
    /// How many owed receipts this handoff could not carry (their asks' senders wait out their own deadlines, as for any receipt that never comes).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub receipts_uncarried: u64,
    /// The session's `dropped=` at the draw ([`Inbox::dropped`]).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dropped: u64,
    /// The broadcasts this session held unhandled at the draw (`topic=`, id past `seen`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub broadcasts_uncarried: u64,
    /// Of [`Self::broadcasts_uncarried`], the ones the successor's bridge offers again, which are therefore not lost.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub broadcasts_refilled: u64,
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

/// The most mailbox bytes — carried posts and owed receipts, each at what the
/// manifest's TOML spends on it ([`PostCarry::cost`], [`ReceiptCarry::cost`])
/// — one handoff manifest carries across every session ([`FabricCarry`]).
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

/// The reason a post the handoff could not carry is retired with in the successor ([`FabricCarry::uncarried`]).
pub const NOT_CARRIED: &str = "not-carried";

/// The reason a queued reply is retired with when the bus log it answers was replaced by one that does not hold the record its `re=` names (`Inbox::forget_bus_offsets`).
pub const LOG_REPLACED: &str = "log-replaced";

const CARRY_INT_MAX: u64 = i64::MAX as u64;

/// How many inbox row ids a seamless update leaves this process between the park's draw and its `_exit` ([`SessionFabric::handoff_carry`]).
pub const INBOX_ID_RESERVE: u64 = 1 << 12;

/// How often a delivery waiting on a committing update — one past
/// [`INBOX_ID_RESERVE`], or a broadcast — logs that it is still waiting
/// ([`SessionFabric::wait_fenced_delivery`]): a cadence, never a bound.
pub const MSG_ROOM_NOTICE_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

/// The production bound every fenced verb waits for a committing update before it answers [`HOLD_BUSY`] ([`SessionFabric::fence_wait`], which reads it through the process's timing value).
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
            .fold(0, usize::saturating_add)
    }

    /// Give up the rows, keep the counters.
    pub fn shed(&mut self) -> bool {
        if self.posts.is_empty() && self.receipts.is_empty() && self.earlier_receipts.is_empty() {
            return false;
        }
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

/// The fabric a PTY keeper recovery carries (the fabric audit's fourth round).
#[cfg(unix)]
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct KeeperCarry {
    /// The first standing hold, as [`render_hold`] writes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold: Option<String>,
    /// Every other standing hold, one row each (the fleet's and the owner's beside the person's, the owner's beside the fleet's).
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
    /// The session's `dropped=` at the sample ([`Inbox::dropped`]), which the recovered session goes on counting from.
    #[serde(skip_serializing_if = "is_zero")]
    pub dropped: u64,
}

#[cfg(unix)]
impl KeeperCarry {
    /// The carry as the meta's cap tries it, most first.
    pub fn rungs(&self) -> [Self; 5] {
        let count = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
        let mut unnamed = self.clone();
        unnamed.queued_lost = unnamed.queued_lost.saturating_add(count(self.queued.len()));
        unnamed.queued.clear();
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
        self.dedup.retain(|off| kept(*off));
        self.dedup_set.retain(|off| kept(*off));
        self.oldest_on_bus = self.oldest_on_bus.filter(|off| kept(*off));
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
                .chain(self.dedup.iter().copied())
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
            || self
                .posts
                .iter()
                .any(|p| p.off.is_some() || (!p.dead && p.re.is_some()))
    }

    fn row_at(&self, off: u64) -> Option<&InboxRow> {
        self.rows.iter().find(|r| r.off == off && r.on_log)
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

/// The per-session fabric state, hung on aterm-gui's `SessionCtx` ([`FabricSession`]).
#[derive(Default)]
pub struct SessionFabric {
    inbox: Mutex<Inbox>,
    changed: Condvar,
    topics: Mutex<std::collections::BTreeMap<String, TopicEntry>>,
    hold_serial: std::sync::atomic::AtomicU64,
    person_refusals: AtomicU64,
    #[cfg(any(test, debug_assertions))]
    fence_wait: Mutex<Option<std::time::Duration>>,
    #[cfg(any(test, debug_assertions))]
    fence_waits: AtomicU64,
    #[cfg(any(test, debug_assertions))]
    fence_bound: Mutex<Option<Option<std::time::Duration>>>,
}

impl SessionFabric {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inbox> {
        self.inbox.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Run `f` on this session's ring under its lock.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn with_inbox_for_test<T>(&self, f: impl FnOnce(&mut Inbox) -> T) -> T {
        f(&mut self.lock())
    }

    /// The bound every fenced verb on this session waits for a committing update before it answers [`HOLD_BUSY`], or a test's own ([`Self::set_fence_wait`]), so a test of what outlasts the bound need not sit out the real one.
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

    /// How many times a waiter on this session has blocked on the fence ([`note_fence_block`]), every thread's.
    #[cfg(any(test, debug_assertions))]
    pub fn fence_waits(&self) -> u64 {
        self.fence_waits.load(Ordering::SeqCst)
    }

    /// The bound the latest waiter that blocked on this session's fence waits ([`note_fence_block`]).
    #[cfg(any(test, debug_assertions))]
    pub fn fence_bound(&self) -> Option<Option<std::time::Duration>> {
        *self.fence_bound.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Wake every waiter parked on this session's condvar without changing anything.
    #[cfg(any(test, debug_assertions))]
    pub fn wake_waiters_for_test(&self) {
        let _inbox = self.lock();
        self.changed.notify_all();
    }

    /// The session ended: its program exited or its tab closed.
    pub fn close(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
    }

    /// Whether [`Self::close`] has run.
    pub fn endpoint_closed(&self) -> bool {
        self.lock().closed
    }

    /// The delivered rows newer than `after` that `want` asks for, with the cursor to resume from and the oldest id still in the ring.
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
                .filter(|r| r.id > after && want.wants(r))
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

    /// The topics this session opted into with each one's `since=` token, in topic order.
    pub fn topics(&self) -> Vec<(String, TopicEntry)> {
        self.topics
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(t, e)| (t.clone(), e.clone()))
            .collect()
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
        self.topic_add_recording(t, since, &|kind, payload, fact| {
            let _ = timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record_fact(kind, payload, fact);
        })
    }

    /// [`Self::topic_add`], recording through `record` (a session's own [`FabricSession::record_fact`]), with the move's [`FabricFact::Topic`] for the session log's mark beside the row.
    pub fn topic_add_recording(
        &self,
        t: &str,
        since: &str,
        record: &dyn Fn(&'static str, String, FabricFact<'_>),
    ) -> Result<bool, TopicsFull> {
        let mut set = self.topics.lock().unwrap_or_else(|p| p.into_inner());
        if set.contains_key(t) {
            // `entry`'s answer, not `insert`'s: `insert` REPLACES, so a
            // repeated `topic add t since=@0` would rewind a running topic to
            // the start of the log while answering `added=0` — the value would
            // have moved and the reply would say nothing had.
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
        record("topic", format!("add {t} since={since}"), fact);
        Ok(true)
    }

    /// Drop a topic.
    pub fn topic_drop(&self, t: &str, timeline: &Mutex<impl Timeline>) -> bool {
        self.topic_drop_recording(t, &|kind, payload, fact| {
            let _ = timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record_fact(kind, payload, fact);
        })
    }

    /// [`Self::topic_drop`], recording through `record`, as [`Self::topic_add_recording`] does.
    pub fn topic_drop_recording(
        &self,
        t: &str,
        record: &dyn Fn(&'static str, String, FabricFact<'_>),
    ) -> bool {
        let mut set = self.topics.lock().unwrap_or_else(|p| p.into_inner());
        let dropped = set.remove(t).is_some();
        if dropped {
            let fact = FabricFact::Topic {
                add: false,
                name: t,
                since: None,
            };
            record("topic", format!("drop {t}"), fact);
        }
        dropped
    }

    /// Replace the whole set.
    pub fn set_topics(&self, topics: impl IntoIterator<Item = (String, TopicEntry)>) {
        let mut set = self.topics.lock().unwrap_or_else(|p| p.into_inner());
        *set = topics.into_iter().collect();
    }

    /// Whether broker offset `off` names mail this session holds (what `send|turn cause=@<off>` may name).
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

    /// Every standing halt on the session, one slot per origin.
    pub fn holds(&self) -> Holds {
        self.lock().holds.clone()
    }

    /// How many acts the person's hold refused since it was set ([`halt_refusal`]).
    pub fn person_refusals(&self) -> u64 {
        self.person_refusals.load(Ordering::Relaxed)
    }

    /// The halt a lost bridge did not make on this session, while it still speaks for the link (`status hold_advisory=`).
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

    /// How many times this session's hold has moved (see the field).
    #[cfg(any(test, debug_assertions))]
    pub fn hold_serial(&self) -> u64 {
        self.hold_serial.load(Ordering::Acquire)
    }

    /// The commit's reading of this session's hold, and the fence that keeps it true until the process exits.
    #[cfg(any(unix, test, debug_assertions))]
    pub fn fence_hold(&self, successor_reads_carry: bool) -> u64 {
        let mut inbox = self.lock();
        inbox.hold_fenced = true;
        inbox.retirements_fenced = successor_reads_carry;
        Self::serial_of(&self.hold_serial, &inbox)
    }

    /// Everything a handoff carries of this session's fabric that can move after the park drew the manifest.
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

    /// Whether this session has numbered an inbox row past the id the last handoff manifest carried ([`Inbox::msg_id_ceiling`]).
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
            let (guard, timeout) = self
                .changed
                .wait_timeout(inbox, every)
                .unwrap_or_else(|p| p.into_inner());
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
        let deadline = std::time::Instant::now() + wait;
        while inbox.hold_fenced {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            note_fence_block(self, Some(wait));
            inbox = self
                .changed
                .wait_timeout(inbox, left)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        Some(inbox)
    }

    /// Run `change` — a change the handoff carries that lives off this endpoint (a task mark, `task_record::mark_session_task`).
    pub fn run_unfenced<T>(
        &self,
        wait: std::time::Duration,
        change: impl FnOnce() -> T,
    ) -> Option<T> {
        let unfenced = self.wait_unfenced(self.lock(), wait)?;
        let out = change();
        drop(unfenced);
        Some(out)
    }

    /// A manual reset that ran directly on this session's engine (`manual_reset::run_direct`), outside the byte stream, made known to a seamless Commit before it is answered.
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
            bus_log: inbox.bus_log.clone(),
            last_msg_id: carried_msg_id,
            last_post_id: inbox.next_post_id.min(CARRY_INT_MAX),
            last_receipt_id: inbox.next_receipt_id.min(CARRY_INT_MAX),
            dropped: inbox.dropped.min(CARRY_INT_MAX),
            broadcasts_uncarried: count(unread_broadcasts),
            broadcasts_refilled: count(refilled_broadcasts),
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
        carry
    }

    /// What this session's keeper meta carries of its fabric ([`KeeperCarry`]).
    #[cfg(unix)]
    pub fn keeper_carry(&self) -> KeeperCarry {
        let topics = render_topics(&self.topics());
        let inbox = self.lock();
        let mut holds = render_holds(&inbox.holds).into_iter();
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
        }
    }

    /// Put back what a seamless update carried of this session's mailbox.
    pub fn seed_carry(&self, carry: &FabricCarry, timeline: &Mutex<impl Timeline>) {
        if carry.is_empty() {
            return;
        }
        let mut inbox = self.lock();
        inbox.bus_log = carry
            .bus_log
            .as_ref()
            .filter(|log| is_log_identity(log))
            .cloned();
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
                why: Some(NOT_CARRIED),
            };
            let _ = tl.record_fact(
                "post-landed",
                format!("{id} off=- reason={NOT_CARRIED}"),
                fact,
            );
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
        drop(inbox);
        self.changed.notify_all();
    }

    /// The attempt that fenced this session stood down.
    #[cfg(any(unix, test, debug_assertions))]
    pub fn lift_hold_fence(&self) {
        let mut inbox = self.lock();
        inbox.hold_fenced = false;
        inbox.retirements_fenced = false;
        drop(inbox);
        self.changed.notify_all();
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
            // Every slot it carried already stood: a second carry of one hold
            // (a cold restore's layout over a keeper's META) moves nothing.
            return;
        }
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
            .record_fact(
                "hold",
                format!("{} carried=1", inbox.holds.event("local")),
                fact,
            );
        self.hold_serial.fetch_add(1, Ordering::AcqRel);
        drop(inbox);
        self.changed.notify_all();
    }

    /// **the presence facts' mail read** (aterm-gui's `App::sense`).
    pub fn mail_facts(&self) -> aterm_messages::presence::MailFacts {
        let inbox = self.lock();
        let seen = inbox.seen;
        let unread = inbox.rows.iter().filter(|r| r.id > seen).count() as u64;
        let pending = inbox
            .rows
            .iter()
            .filter(|r| r.id > seen && r.listed)
            .count() as u64;
        let queued = inbox
            .posts
            .iter()
            .filter(|p| p.off.is_none() && !p.dead)
            .count() as u64;
        let is_unread = |r: &&InboxRow| r.id > seen;
        let last = inbox
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
            });
        aterm_messages::presence::MailFacts {
            unread,
            pending,
            dropped: inbox.dropped,
            queued,
            last,
            head: inbox.rows.back().map_or(0, |r| r.id),
        }
    }

    /// **the room's read** (the resident pet, `kitty_pet` panel #9(e)).
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
            .and_then(|ms| ms.to_str()?.parse::<u64>().ok())
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
    /// `log=auto` (`[fabric] log = "auto"`, the default).
    Auto,
    /// `log=turns`: it asks this endpoint to serve `log-take`/`log-landed`.
    Turns,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MirrorReport {
    state: &'static str,
    reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LogClock {
    ident: String,
    continues: Option<(String, u64)>,
}

impl LogClock {
    fn keep_below(&self, held: Option<&str>) -> Option<u64> {
        let (was, fork) = self.continues.as_ref()?;
        let held = held?;
        let in_chain = held == was.as_str()
            || (is_earlier_generation(was, held) && is_earlier_generation(held, &self.ident));
        in_chain.then_some(*fork)
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
    let current = {
        #[cfg(any(test, debug_assertions))]
        let section = LINK_SECTION.lock().unwrap_or_else(|p| p.into_inner());
        #[cfg(any(test, debug_assertions))]
        let foreign = section.is_some_and(|owner| owner != std::thread::current().id());
        #[cfg(not(any(test, debug_assertions)))]
        let foreign = false;
        if foreign {
            None
        } else {
            LINK.log.lock().unwrap_or_else(|p| p.into_inner()).clone()
        }
    };
    if let Some(clock) = current {
        let mut inbox = ctx.fabric().lock();
        let forget = inbox
            .bus_log
            .as_deref()
            .map_or_else(|| inbox.holds_bus_references(), |was| was != clock.ident);
        if forget {
            let keep_below = clock.keep_below(inbox.bus_log.as_deref());
            for id in inbox.forget_bus_offsets(keep_below) {
                let fact = FabricFact::MailLanded {
                    post: id,
                    off: None,
                    why: Some(LOG_REPLACED),
                };
                let _ = ctx.record_fact(
                    "post-landed",
                    format!("{id} off=- reason={LOG_REPLACED}"),
                    fact,
                );
            }
        }
        inbox.bus_log = Some(clock.ident);
        drop(inbox);
        ctx.fabric().changed.notify_all();
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
    // It is also the gate every other writer of this struct has and this one
    // was missing: `bridge_attached`, `link_report` and `link_evidence` all
    // refuse a write that does not belong to the live bridge generation, so a
    // caller with no bridge cannot move the link's state. Without the same rule
    // here, ANY `post` moved it — including one from a test with no bridge at
    // all, which made `owed_since` the one field of `LinkReport` that leaked
    // across the section lock `with_link` exists to provide.
    if LINK.state.load(Ordering::Relaxed) != FABRIC_CONNECTED {
        return;
    }
    #[cfg(any(test, debug_assertions))]
    if link_section_is_foreign() {
        return;
    }
    let mut link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
    if link.owed_since.is_none() {
        link.owed_since = Some(std::time::Instant::now());
    }
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
    #[cfg(any(test, debug_assertions))]
    if link_section_is_foreign() {
        return;
    }
    let outstanding = posts_outstanding(store);
    let mut link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
    link.owed_since = outstanding.then(std::time::Instant::now);
}

#[cfg(test)]
fn backdate_work_owed(by: std::time::Duration) {
    let mut link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
    link.owed_since = link.owed_since.map(|t| t.checked_sub(by).unwrap_or(t));
}

/// The `status` reply's `fabric=` token.
pub fn fabric_state() -> &'static str {
    match LINK.state.load(Ordering::Relaxed) {
        FABRIC_CONNECTED if link_is_stale() => "stale",
        FABRIC_CONNECTED => "connected",
        FABRIC_DISCONNECTED => "disconnected",
        FABRIC_STALLED => "stalled",
        _ => "absent",
    }
}

/// [`fabric_state`] as an App's presence facts read it.
pub fn fabric_state_seen() -> &'static str {
    #[cfg(any(test, debug_assertions))]
    {
        let section = LINK_SECTION.lock().unwrap_or_else(|p| p.into_inner());
        if section.is_some_and(|owner| owner != std::thread::current().id()) {
            return "absent";
        }
        fabric_state()
    }
    #[cfg(not(any(test, debug_assertions)))]
    fabric_state()
}

/// The link's three reported facts, as `status` and `fabric status` print them.
pub fn fabric_link_facts() -> (String, Option<u64>, Option<u64>) {
    let link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
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
    }
    LINK.state.store(FABRIC_STALLED, Ordering::Relaxed);
    link_moved(FABRIC_STALLED, generation.0, LinkSaid::Attached);
}

/// A bridge that never reports its link is read by what it does.
pub fn link_evidence(generation: Option<BridgeGeneration>) {
    #[cfg(any(test, debug_assertions))]
    if link_section_is_foreign() {
        return;
    }
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
        // The link is BACK: every band's fabric slot returns from `~ Ns` to
        // `⟟` — a presence wake per live session, and no parked `await inbox`
        // is woken (its answer did not change).
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

/// Whether the bridge that owns the link said, in its `hello`, that it offers a kept session's unhandled broadcasts again after a relaunch (`refill=…say`).
pub fn bridge_refills_broadcasts() -> bool {
    #[cfg(any(test, debug_assertions))]
    if link_section_is_foreign() {
        return false;
    }
    LINK.link
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .hello
        .as_ref()
        .is_some_and(|h| h.refills_broadcasts)
}

const HELLO_USAGE: &str = "ERR usage: hello lane=<n> [build=<token>]\n";

const BUILD_TOKEN_MAX: usize = 64;

/// `hello lane=<n> [build=<token>] [refill=<lanes>]`.
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
            // WHAT THE BRIDGE OFFERS AGAIN to a session a relaunch keeps, as a
            // comma list of lanes; `say` is the broadcast topics, which is all
            // this endpoint asks. An unknown lane in the list is read past.
            refills_broadcasts = v.split(',').any(|lane| lane == "say");
        } else if let Some(v) = kv(tok, "lane") {
            match v.parse::<u32>() {
                Ok(n) => lane = Some(n),
                Err(_) => return HELLO_USAGE.to_string(),
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
    let own = aterm_types::version::build_id();
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

/// Whether the bridge that owns the link asked for the bus mirror in its recorded `hello` (`log=turns`).
#[must_use]
pub fn bridge_mirrors_logs() -> bool {
    #[cfg(any(test, debug_assertions))]
    if link_section_is_foreign() {
        return false;
    }
    LINK.link
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .hello
        .as_ref()
        .is_some_and(|h| h.log == HelloLog::Turns)
}

/// Whether the node mirrors, as the owning bridge last said it (the word the store's `session_log_mirror::Shared::served` keeps).
#[must_use]
pub fn bridge_mirror_word() -> Option<bool> {
    #[cfg(any(test, debug_assertions))]
    if link_section_is_foreign() {
        return Some(false);
    }
    let link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
    match link.hello.as_ref().map(|h| h.log) {
        Some(HelloLog::Turns) => Some(true),
        Some(HelloLog::Auto) => link
            .mirror
            .as_ref()
            .is_some_and(|m| m.state == "unretained")
            .then_some(false),
        Some(HelloLog::Off) | None => Some(false),
    }
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
        || "starting".to_string(),
        |m| {
            if m.reason.is_empty() {
                m.state.to_string()
            } else {
                format!("{}:{}", m.state, m.reason)
            }
        },
    );
    format!(" bridge_log={word} mirror={state}")
}

fn cmd_link_mirror<'a>(
    mut toks: impl Iterator<Item = &'a str>,
    generation: Option<BridgeGeneration>,
) -> String {
    let state = match toks.next() {
        Some("on") => "on",
        Some("stopped") => "stopped",
        Some("capped") => "capped",
        Some("budget") => "budget",
        Some("unretained") => "unretained",
        _ => return LINK_USAGE.to_string(),
    };
    let mut reason = String::new();
    for tok in toks {
        match kv(tok, "reason") {
            Some(v) => reason = link_reason_token(v),
            None => return LINK_USAGE.to_string(),
        }
    }
    with_current_bridge(generation, || {
        LINK.link.lock().unwrap_or_else(|p| p.into_inner()).mirror =
            Some(MirrorReport { state, reason });
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
        .and_then(|v| v.parse::<u64>().ok())
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
        .and_then(|v| v.parse::<u64>().ok());
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

/// The endpoint answered one of `generation`'s bridge-only lines `ERR usage`.
pub fn note_lane_fault(generation: Option<BridgeGeneration>) {
    #[cfg(any(test, debug_assertions))]
    if link_section_is_foreign() {
        return;
    }
    let owner = LINK.generation.lock().unwrap_or_else(|p| p.into_inner());
    if !generation.is_some_and(|g| g.0 == *owner) || *owner == 0 {
        return;
    }
    let mut link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
    link.lane_faults = link.lane_faults.saturating_add(1);
}

/// What the attached bridge said about itself and how its lane has gone, as `fabric status` prints it.
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
                          link quarantine torn=<k>\n";

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

/// `link up rtt=<ms>` / `link down reason=<token> [rtt=<ms>]` / `link log <identity>` / `link bye <quit|handoff>` / `link mirror …` / `link set-aside files=<n>` / `link quarantine torn=<k>`, from the bridge lane whose generation `generation` is.
pub fn cmd_link(store: &impl AsStore, rest: &str, generation: Option<BridgeGeneration>) -> String {
    cmd_link_after_log_switch(store, rest, generation, || {})
}

/// `cmd_link` with the endpoint's additional log-clock retirement, called exactly on a completed `forgot=1` switch before releasing the transition.
pub fn cmd_link_after_log_switch(
    store: &impl AsStore,
    rest: &str,
    generation: Option<BridgeGeneration>,
    on_forgot: impl FnOnce(),
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
                    match v.parse::<u64>() {
                        Ok(n) => fork = Some(n),
                        Err(_) => return LINK_USAGE.to_string(),
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
        _ => return LINK_USAGE.to_string(),
    };
    let mut rtt: Option<u64> = None;
    let mut reason: Option<String> = None;
    for tok in toks {
        if let Some(v) = kv(tok, "rtt") {
            match v.parse::<u64>() {
                Ok(n) => rtt = Some(n),
                Err(_) => return LINK_USAGE.to_string(),
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
    let Some(ident) = ident.filter(|i| is_log_identity(i)) else {
        return LINK_USAGE.to_string();
    };
    if continues.is_some_and(|(was, _)| !is_earlier_generation(was, ident)) {
        return LINK_USAGE.to_string();
    }
    let _transition = LINK
        .log_transition
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let previous = {
        let owner = LINK.generation.lock().unwrap_or_else(|p| p.into_inner());
        if !generation.is_some_and(|g| g.0 == *owner) || *owner == 0 {
            return "OK stale=1\n".to_string();
        }
        LINK.log.lock().unwrap_or_else(|p| p.into_inner()).clone()
    };
    let mut changed = previous.as_ref().is_some_and(|was| was.ident != ident);
    let clock = LogClock {
        ident: ident.to_string(),
        continues: continues
            .map(|(was, fork)| (was.to_string(), fork))
            .or_else(|| {
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
        if inbox.bus_log.as_deref() == Some(ident) {
            continue;
        }
        let forget = inbox.bus_log.as_deref().map_or_else(
            || changed || inbox.holds_bus_references(),
            |was| was != ident,
        );
        if forget {
            let wait = ctx.fabric().fence_wait();
            let Some(guard) = ctx.fabric().wait_unfenced_retirement(inbox, wait) else {
                return HOLD_BUSY.to_string();
            };
            inbox = guard;
            let keep_below = clock.keep_below(inbox.bus_log.as_deref());
            let retired = inbox.forget_bus_offsets(keep_below);
            for id in retired {
                let fact = FabricFact::MailLanded {
                    post: id,
                    off: None,
                    why: Some(LOG_REPLACED),
                };
                let _ = ctx.record_fact(
                    "post-landed",
                    format!("{id} off=- reason={LOG_REPLACED}"),
                    fact,
                );
            }
            changed = true;
        }
        inbox.bus_log = Some(ident.to_string());
        drop(inbox);
        ctx.fabric().changed.notify_all();
    }
    *LINK.log.lock().unwrap_or_else(|p| p.into_inner()) = Some(clock);
    if changed {
        on_forgot();
        "OK forgot=1\n"
    } else {
        "OK\n"
    }
    .to_string()
}

fn is_log_identity(s: &str) -> bool {
    s.split_once('.').is_some_and(|(id, generation)| {
        id.len() == 32
            && id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && !generation.is_empty()
            && generation.len() <= 20
            && generation.bytes().all(|b| b.is_ascii_digit())
            && generation.parse::<u64>().is_ok()
    })
}

fn is_earlier_generation(was: &str, ident: &str) -> bool {
    fn split(s: &str) -> Option<(&str, u64)> {
        let (id, generation) = s.split_once('.').filter(|_| is_log_identity(s))?;
        Some((id, generation.parse().ok()?))
    }
    matches!(
        (split(was), split(ident)),
        (Some((log, before)), Some((now, after))) if log == now && before < after
    )
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
        ctx.fabric().changed.notify_all();
        ctx.fabric_changed();
        drop(guard);
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

/// [`bridge_lost`], calling `wake` with each session's id whose session log owes its readers a wake (the halt's `hold` mark), once its fabric lock is released.
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
            if LINK.state.swap(FABRIC_DISCONNECTED, Ordering::Relaxed) != FABRIC_DISCONNECTED {
                link_moved(FABRIC_DISCONNECTED, generation.0, LinkSaid::Lost);
            }
            let mut link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
            link.up = false;
            link.reason = LINK_BRIDGE_LOST.to_string();
            link.stalled_at = None;
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
                apply_hold(&ctx, HoldAct::Set(lost), HoldIssuer::Bridge, None, &|| {
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

/// Whether a lost bridge halts the sessions it governed ([`set_hold_on_bridge_lost`]; `false` by default since 2026-09-29).
#[cfg(any(unix, test, debug_assertions))]
pub fn holds_on_bridge_lost() -> bool {
    LINK.hold_on_loss.load(Ordering::Relaxed)
}

#[cfg(any(unix, test, debug_assertions))]
fn advise(ctx: &dyn FabricSession, lost: Hold, generation: BridgeGeneration) -> bool {
    let mut inbox = ctx.fabric().lock();
    if inbox.holds.is_held() {
        return false;
    }
    let record = format!("1 reason={} origin={}", lost.reason, lost.origin);
    let fact = FabricFact::Advisory {
        origin: &lost.origin,
        reason: &lost.reason,
        generation: generation.0,
    };
    let _ = ctx.record_fact("hold-advisory", record, fact);
    inbox.advisory = Some((lost, generation.0));
    drop(inbox);
    ctx.fabric().changed.notify_all();
    ctx.fabric_changed();
    true
}

/// Whether a fleet halt refuses `verb` (§5.3).
#[must_use]
pub fn is_pty_reaching(verb: &str) -> bool {
    aterm_types::control_verbs::input_of(verb).contains(aterm_types::control_verbs::Input::PTY)
}

/// The refusal a halted session answers `verb` with, or `None` when the verb may proceed ([`is_pty_reaching`], then [`hold_refusal`]).
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
    let hold = ctx.fabric().holds().first_against(driver.as_deref())?;
    Some((
        format!("ERR halted reason={} origin={}\n", hold.reason, hold.origin),
        hold.origin,
    ))
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
            AuxTarget::Menu
                | AuxTarget::TabMenu
                | AuxTarget::Overview
                | AuxTarget::AutopilotMenu
                | AuxTarget::FactMenu
        )
    {
        return None;
    }
    app_halt_refusal(store, "invoke")
}

/// A principal address: a bare principal ([`is_principal`]), or `<owner>@<node>` for a session behind a node, both halves bare.
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

/// The answer to a `hold` that names the person's slot from the wire (`origin=person`).
pub const HOLD_PERSON_USAGE: &str = "ERR usage: origin=person is the window's own hold \
                                            (Agents \u{25b8} Take Over This Tab / Hand Back This \
                                            Tab); hold <sid> on|off [reason=<pct>] \
                                            [origin=fleet|local]\n";

/// The refusal an Owner-issued `hold` gets for a fleet-origin act.
pub const HOLD_DENIED: &str = "ERR denied: only the fleet sets or lifts a fleet hold\n";

/// The answer to a `hold` that arrived while a seamless update was committing the session and waited its [`SessionFabric::fence_wait`] without the update either landing or standing down.
pub const HOLD_BUSY: &str = "ERR busy update-committing\n";

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
    /// The slot already stood as asked.
    Unchanged { held: bool },
    /// An Owner-issued act named the fleet's slot, or an Owner `off` found only a fleet hold standing.
    FleetHeld,
    /// The act named a slot its issuer may not touch.
    NotItsSlot,
    /// A remote node's set while the person's hold stands or another node's does, or its lift of another node's.
    Taken,
    /// A seamless update was committing the session for the whole of the wait the caller allowed ([`SessionFabric::fence_hold`]).
    Fenced,
}

fn apply_hold(
    ctx: &dyn FabricSession,
    act: HoldAct<'_>,
    issuer: HoldIssuer,
    fence_wait: Option<std::time::Duration>,
    wake: &dyn Fn(),
) -> Applied {
    apply_hold_as(ctx, act, issuer, fence_wait, false, wake)
}

fn apply_hold_as(
    ctx: &dyn FabricSession,
    act: HoldAct<'_>,
    issuer: HoldIssuer,
    fence_wait: Option<std::time::Duration>,
    carried: bool,
    wake: &dyn Fn(),
) -> Applied {
    let mut inbox = ctx.fabric().lock();
    let deadline = fence_wait.map(|wait| std::time::Instant::now() + wait);
    while inbox.hold_fenced {
        inbox = match deadline {
            None => {
                note_fence_block(ctx.fabric(), None);
                ctx.fabric()
                    .changed
                    .wait(inbox)
                    .unwrap_or_else(|p| p.into_inner())
            }
            Some(deadline) => {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    return Applied::Fenced;
                }
                note_fence_block(ctx.fabric(), fence_wait);
                ctx.fabric()
                    .changed
                    .wait_timeout(inbox, left)
                    .unwrap_or_else(|p| p.into_inner())
                    .0
            }
        };
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
        // Not the owner's slot to name at all (`cmd_hold` refuses it first;
        // this keeps the rule where the slots are).
        if act.origin() != Some("local") {
            return Applied::FleetHeld;
        }
        if matches!(act, HoldAct::Lift(_)) && !inbox.holds.has("local") && inbox.holds.has("fleet")
        {
            return Applied::FleetHeld;
        }
    }
    let before = inbox.holds.clone();
    match &act {
        HoldAct::Set(hold) => {
            *inbox.holds.slot_mut(&hold.origin) = Some(hold.reason.clone());
            if issuer == HoldIssuer::Person {
                inbox.holds.remote = None;
            }
        }
        HoldAct::Lift(origin) => *inbox.holds.slot_mut(origin) = None,
        HoldAct::SetRemote(hold) => inbox.holds.remote = Some(hold.clone()),
        HoldAct::LiftRemote(_) => inbox.holds.remote = None,
        #[cfg(any(test, debug_assertions))]
        HoldAct::LiftAll => inbox.holds = Holds::default(),
    }
    let held = inbox.holds.is_held();
    let changed = inbox.holds != before;
    if changed && issuer == HoldIssuer::Person && matches!(act, HoldAct::Set(_)) {
        ctx.fabric().person_refusals.store(0, Ordering::Relaxed);
    }
    let mut owed = false;
    if changed {
        let lifted = act.origin().unwrap_or("local");
        ctx.fabric().hold_serial.fetch_add(1, Ordering::AcqRel);
        let mark = if carried { " carried=1" } else { "" };
        let (origin, reason, hold_origin) = inbox.holds.event_fields(lifted);
        let fact = FabricFact::Hold {
            held,
            bridge: issuer == HoldIssuer::Bridge,
            origin: &origin,
            hold_origin: &hold_origin,
            reason: reason.as_deref(),
            carried,
        };
        let payload = format!("{}{mark}", inbox.holds.event(lifted));
        owed = ctx.record_fact("hold", payload, fact);
    }
    drop(inbox);
    if owed {
        wake();
    }
    if changed {
        ctx.fabric().changed.notify_all();
        ctx.fabric_changed();
        Applied::Changed { held }
    } else {
        Applied::Unchanged { held }
    }
}

#[cfg(any(test, debug_assertions))]
static LINK_SECTION: Mutex<Option<std::thread::ThreadId>> = Mutex::new(None);

#[cfg(any(test, debug_assertions))]
fn link_section_is_foreign() -> bool {
    LINK_SECTION
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_some_and(|owner| owner != std::thread::current().id())
}

#[cfg(any(test, debug_assertions))]
static LINK_RESET_ALSO: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// Have every [`with_link_reset`] section also run `reset` at its start and its end, under the section's lock.
#[cfg(any(test, debug_assertions))]
pub fn also_reset_with_the_link(reset: fn()) {
    let _ = LINK_RESET_ALSO.set(reset);
}

/// Run `f` with the process-wide bridge link reset to "never launched", and reset it again on the way out.
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

/// The owner's local hold a restore layout carried, put back on a session that is registered already.
pub fn carry_hold(ctx: &dyn FabricSession, hold: Hold) -> bool {
    carry_hold_waking(ctx, hold, &|| {})
}

/// [`carry_hold`], calling `wake` when the session log's readers are owed a wake (its `hold` mark), once the fabric lock is released.
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
        wake,
    ) {
        Applied::Changed { held } | Applied::Unchanged { held } => held,
        Applied::FleetHeld | Applied::NotItsSlot | Applied::Taken | Applied::Fenced => false,
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

/// [`cmd_hold`], calling `wake` with the session's id when the session log's readers are owed a wake (its `hold` mark), once the fabric lock is released.
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
    let mut toks = rest.split_whitespace();
    let (Some(sid), Some(state)) = (toks.next(), toks.next()) else {
        return HOLD_USAGE.to_string();
    };
    let on = match state {
        "on" => true,
        "off" => false,
        _ => return HOLD_USAGE.to_string(),
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
                return HOLD_USAGE.to_string();
            }
            if !matches!(v, "fleet" | "local") {
                return HOLD_USAGE.to_string();
            }
            origin = v.to_string();
        } else {
            return HOLD_USAGE.to_string();
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
    match apply_hold(&ctx, act, issuer, Some(fence_wait), &|| wake(ctx.self_id())) {
        Applied::FleetHeld => HOLD_DENIED.to_string(),
        Applied::NotItsSlot => HOLD_PERSON_USAGE.to_string(),
        Applied::Taken => match ctx.fabric().hold() {
            Some(hold) => format!("ERR halted reason={} origin={}\n", hold.reason, hold.origin),
            None => HOLD_DENIED.to_string(),
        },
        Applied::Fenced => HOLD_BUSY.to_string(),
        Applied::Changed { held } | Applied::Unchanged { held } => {
            format!("OK hold={}\n", u8::from(held))
        }
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
        Applied::Unchanged { .. } | Applied::FleetHeld | Applied::NotItsSlot | Applied::Taken => {
            PersonHoldOutcome::Unchanged
        }
    }
}

const DELIVER_USAGE: &str = "ERR usage: deliver <sid> off=<n> from=<p> kind=<k> trust=<t> \
                             [re=<n>] [dl=<ms>] [late=1] [demoted=<k>] [via=<p,...>] \
                             [verdict=<handled|refused|deferred>] [len=<n>] [text=<pct>] \
                             | deliver <sid> landed=<post-id> off=<n> \
                             | deliver <sid> fetched=<off> (err=<token> [floor=<n>] \
                             | from=<p> kind=<k> trust=<t> [re=<n>] [dl=<ms>] \
                             [demoted=<k>] [via=<p,...>] [verdict=<v>] [len=<n>] [at=<n>] \
                             [more=1] [text=<pct>]) \
                             | deliver <sid> receipt=<rid> off=<n|->\n";

/// `deliver` — put one bus record in a session's inbox, or close an outbound post.
pub fn cmd_deliver(store: &impl AsStore, rest: &str) -> String {
    cmd_deliver_waking(store, rest, &|_| {})
}

/// [`cmd_deliver`], calling `wake` with the session's id when the session log's readers are owed a wake (a `mail` or `mail-landed` mark), once the fabric lock is released.
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

fn deliver_receipt(ctx: &dyn FabricSession, toks: &[&str]) -> String {
    let mut rid: Option<u64> = None;
    let mut landed = false;
    for tok in toks {
        if let Some(v) = kv(tok, "receipt") {
            match v.parse() {
                Ok(n) => rid = Some(n),
                Err(_) => return DELIVER_USAGE.to_string(),
            }
        } else if let Some(v) = kv(tok, "off") {
            if v != "-" && v.parse::<u64>().is_err() {
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
            match v.parse() {
                Ok(n) => off = Some(n),
                Err(_) => return DELIVER_USAGE.to_string(),
            }
        } else if let Some(v) = kv(tok, "err") {
            // A bridge is a separate process and its output is still input:
            // the reason is one bounded token or the line is refused.
            if !valid_dead_reason(v) {
                return DELIVER_USAGE.to_string();
            }
            err = Some(v.to_string());
        } else if let Some(v) = kv(tok, "floor") {
            match v.parse() {
                Ok(n) => floor = Some(n),
                Err(_) => return DELIVER_USAGE.to_string(),
            }
        } else if let Some(v) = kv(tok, "at") {
            match v.parse() {
                Ok(n) => at = n,
                Err(_) => return DELIVER_USAGE.to_string(),
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
                // Every slot is a parked read of some OTHER offset: nothing
                // waits on this one, so keeping it would evict nothing safely.
                return "OK unrequested=1\n".to_string();
            }
            inbox.fetches.push_back((off, slot));
        }
    }
    drop(inbox);
    if answer_ready {
        ctx.fabric().changed.notify_all();
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
        drop(inbox);
        ctx.fabric().changed.notify_all();
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
        body: String::new(),
    };
    let mut declared_len: Option<usize> = None;
    let mut text = String::new();
    for tok in toks {
        if let Some(v) = kv(tok, "off") {
            match v.parse() {
                Ok(n) => f.off = Some(n),
                Err(_) => return Err(DELIVER_USAGE.to_string()),
            }
        } else if let Some(v) = kv(tok, "from") {
            f.from = v.to_string();
        } else if let Some(v) = kv(tok, "kind") {
            f.kind = v.to_string();
        } else if let Some(v) = kv(tok, "trust") {
            f.trust = v.to_string();
        } else if let Some(v) = kv(tok, "re") {
            match v.parse() {
                Ok(n) => f.re = Some(n),
                Err(_) => return Err(DELIVER_USAGE.to_string()),
            }
        } else if let Some(v) = kv(tok, "dl") {
            match v.parse() {
                Ok(n) => f.dl = Some(n),
                Err(_) => return Err(DELIVER_USAGE.to_string()),
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
            // CLOSED TO THE GRAMMAR, like every other word on this line: the
            // bridge is a separate process and its output is still input, and a
            // topic that could carry a space or an `=` is a topic that can
            // invent a second field on the `msg` row this becomes.
            if !is_topic(v) {
                return Err(DELIVER_USAGE.to_string());
            }
            f.topic = Some(v.to_string());
        } else if let Some(v) = kv(tok, "verdict") {
            // CLOSED, like `trust=`: a receipt's word is one of three, and an
            // agent never reads a verdict token this endpoint did not classify.
            if !VERDICTS.contains(&v) {
                return Err(DELIVER_USAGE.to_string());
            }
            f.verdict = Some(v.to_string());
        } else if let Some(v) = kv(tok, "len") {
            match v.parse::<usize>() {
                Ok(n) if n <= len_max => declared_len = Some(n),
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
            post_id = v.parse().ok();
        } else if let Some(v) = kv(tok, "off") {
            off = v.parse().ok();
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
            why: None,
        };
        let payload = match off {
            Some(n) => format!("{post_id} off={n}"),
            None => format!("{post_id} off=-"),
        };
        owed = ctx.record_fact("post-landed", payload, fact);
    }
    inbox.trim_retired_posts();
    drop(inbox);
    if owed {
        wake();
    }
    ctx.fabric().changed.notify_all();
    ctx.fabric_changed();
    Ok(())
}

const OUTBOX_USAGE: &str = "ERR usage: outbox [<max>] [earlier-receipts=1] | outbox sent <sid> <post-id> off=<n|-> \
                            [dup=1] [reason=<word>]\n";

const DEAD_DEFAULT: &str = "undeliverable";

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
    match rest.split_once(' ') {
        Some(("sent", tail)) => cmd_outbox_sent_waking(store, tail, wake),
        _ if rest.trim() == "sent" => OUTBOX_USAGE.to_string(),
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
        match tok.parse::<usize>() {
            Ok(n) => max = n,
            Err(_) => return OUTBOX_USAGE.to_string(),
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
            // Older bridges ignore unknown receipt fields. They must never
            // publish an old log's offset merely because they skipped its tag.
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
    format!("OK {}\n{payload}", payload.len())
}

/// `outbox sent <sid> <post-id> off=<n|->`.
pub fn cmd_outbox_sent(store: &impl AsStore, rest: &str) -> String {
    cmd_outbox_sent_waking(store, rest, &|_| {})
}

/// [`cmd_outbox_sent`], calling `wake` with the session's id when the session log's readers are owed a wake (a `mail-landed` mark), once the fabric lock is released.
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
    let Ok(post_id) = id.parse::<u64>() else {
        return OUTBOX_USAGE.to_string();
    };
    let Some(off_raw) = kv(off_tok, "off") else {
        return OUTBOX_USAGE.to_string();
    };
    let off = match off_raw {
        "-" => None,
        v => match v.parse::<u64>() {
            Ok(n) => Some(n),
            Err(_) => return OUTBOX_USAGE.to_string(),
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
    if inbox.dedup_set.contains(&off) || inbox.row_at(off).is_some() {
        let existing = inbox.row_at(off).map(|r| r.id);
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
        // AND COUNTED ONCE: its offset is remembered as delivered, so a bridge
        // that starts and walks this topic again from an earlier unhandled row
        // ([`InboxRow::refill`]) is answered `OK 0` — taken once, held no
        // more — rather than refused, and counted, a second time.
        if topic.is_some() {
            inbox.dropped += 1;
            remember_offset(&mut inbox, off);
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
        on_log: true,
    };
    evict_for(&mut inbox);
    inbox.rows.push_back(row);
    inbox.bus_head = inbox.bus_head.max(off);
    if is_addressed {
        inbox.addressed_head = Some(inbox.addressed_head.map_or(off, |h| h.max(off)));
    }
    inbox.oldest_on_bus = Some(inbox.oldest_on_bus.map_or(off, |o| o.min(off)));
    remember_offset(&mut inbox, off);
    let fact = FabricFact::MailIn {
        kind: &kind,
        from: &from,
        off,
        inbox: id,
    };
    let owed = ctx.record_fact(
        "inbox",
        format!("{id} from={from} kind={kind} off={off}"),
        fact,
    );
    drop(inbox);
    ctx.fabric().changed.notify_all();
    ctx.fabric_changed();
    if owed {
        wake();
    }
    format!("OK {id}\n")
}

fn quota_key(from: &str) -> &str {
    from.split_once('@').map_or(from, |(_, node)| node)
}

fn remember_offset(inbox: &mut Inbox, off: u64) {
    inbox.dedup.push_back(off);
    inbox.dedup_set.insert(off);
    while inbox.dedup.len() > DEDUP_CAP {
        if let Some(old) = inbox.dedup.pop_front() {
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
            let seen = ctx.fabric().lock().seen;
            let mut out = format!("OK {} seen={seen}\n", topics.len());
            for (t, e) in topics {
                out.push_str(&format!("topic {t} since={}\n", e.since));
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
            let added = ctx
                .fabric()
                .topic_add_recording(t, &since, &|kind, payload, fact| {
                    let _ = ctx.record_fact(kind, payload, fact);
                });
            drop(unfenced);
            let Ok(added) = added else {
                // Nothing changed, so nothing is pushed or woken.
                return format!("ERR topics full (max {MAX_TOPICS}): drop one first\n");
            };
            ctx.fabric().changed.notify_all();
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
            let dropped = ctx
                .fabric()
                .topic_drop_recording(t, &|kind, payload, fact| {
                    let _ = ctx.record_fact(kind, payload, fact);
                });
            if dropped {
                for row in unfenced
                    .rows
                    .iter_mut()
                    .filter(|r| r.topic.as_deref() == Some(t))
                {
                    row.refill = false;
                }
            }
            drop(unfenced);
            ctx.fabric().changed.notify_all();
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

/// The reason a keeper meta gives a carried hold whose own reason it had no room for ([`KeeperCarry::rungs`]).
#[cfg(unix)]
pub const REASON_NOT_CARRIED: &str = "not-carried";

/// Every standing hold as the carries write them ([`render_hold`] rows), in [`Holds::standing`]'s order.
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
    match rest.split_once(' ') {
        Some(("get", tail)) => cmd_inbox_get(ctx, tail),
        Some(("seen", tail)) => cmd_inbox_seen(ctx, tail),
        _ if matches!(rest.trim(), "get" | "seen") => INBOX_USAGE.to_string(),
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
            match v.parse() {
                Ok(n) => since = n,
                Err(_) => return INBOX_USAGE.to_string(),
            }
        } else if tok == "--peek" {
            peek = true;
        } else if tok == "--meta" {
            meta_only = true;
        } else if let Ok(n) = tok.parse::<usize>() {
            limit = Some(n);
        } else {
            return INBOX_USAGE.to_string();
        }
    }

    let holder = holder_token(ctx);
    let mut inbox = ctx.fabric().lock();
    let hold_flag = u8::from(inbox.holds.is_held());
    let selected = inbox.rows.iter().filter(|r| r.id > since).count();
    let mut skip = limit.map_or(0, |n| selected.saturating_sub(n));

    let mut body = String::new();
    let mut sent = 0usize;
    let mut pending = 0usize;
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
    let mut out = format!(
        "OK {sent} hold={hold_flag} holder={holder} seen={} bus_head={} oldest_on_bus={oldest} \
         dropped={} pending={pending}\n",
        inbox.seen, inbox.bus_head, inbox.dropped,
    );
    drop(inbox);
    out.push_str(&body);
    out
}

fn render_row_into(out: &mut String, row: &InboxRow, meta_only: bool) {
    use std::fmt::Write as _;
    let _ = write!(
        out,
        "msg {} off={} t={} from={} kind={} trust={}",
        row.id, row.off, row.t_ms, row.from, row.kind, row.trust
    );
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
        return match off.parse::<u64>() {
            Ok(off) => inbox_get_at(ctx, off),
            Err(_) => INBOX_USAGE.to_string(),
        };
    }
    let Ok(id) = arg.parse::<u64>() else {
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
    inbox_get_at_within(ctx, off, std::time::Duration::from_millis(FETCH_WAIT_MS))
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
            let _ = ctx.record_fact("fetch", format!("off={off}"), FabricFact::Fetch { off });
        }
    }
    let deadline = std::time::Instant::now() + wait;
    loop {
        let Some(i) = inbox.fetches.iter().position(|(o, _)| *o == off) else {
            // Evicted from under a parked read: only an answered slot is ever
            // evicted, and this one was pending — but a reader that cannot
            // tell must say "nothing" rather than hang.
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
        inbox = aterm_control::park::park_guarded(
            &ctx.fabric().changed,
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
    let Some(id) = toks.next().and_then(|t| t.parse::<u64>().ok()) else {
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
    let mut verdict_tail = String::new();
    if let Some(v) = decided {
        let row = &mut inbox.rows[at];
        row.decision = Some(v.to_string());
        verdict_tail = format!(" verdict={v} kind={} from={}", row.kind, row.from);
        if matches!(row.kind.as_str(), "ask" | "task") {
            let (kind, from) = (row.kind.clone(), row.from.clone());
            owe_receipt(&mut inbox, off, v, &kind, &from, earlier_log);
        }
    }
    let seen_before = inbox.seen;
    inbox.seen = inbox.seen.max(id);
    for row in &mut inbox.rows {
        if row.id <= id {
            row.listed = true;
        }
    }
    let seen = inbox.seen;
    let floor_at = addressed_floor(&inbox);
    let floor = floor_at.map_or_else(|| "-".to_string(), |f| f.to_string());
    let payload = format!("{id} off={off} floor={floor} seen={seen}{verdict_tail}");
    let row = &inbox.rows[at];
    let fact = FabricFact::MailSeen {
        inbox: id,
        off,
        floor: floor_at,
        seen,
        verdict: decided,
        kind: &row.kind,
        from: &row.from,
        moved: seen > seen_before || decided.is_some(),
    };
    let _ = ctx.record_fact("inbox-seen", payload, fact);
    drop(inbox);
    ctx.fabric().changed.notify_all();
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
                          kind=<ask|answer|task|report|note|ack|control> [re=<n>] [dl=<ms>] \
                          [via=<p>] [key=<token>] [--wait[=<ms>]] [--wait-ack[=<ms>]] \
                          ([--] <text> | len=<n> + <n> raw bytes)\n";

const POST_LEN_USAGE: &str = "ERR usage: post len=<n> must be a byte count\n";

/// Parse a `post` `len=` value.
pub fn post_len(v: &str) -> Result<usize, String> {
    match v.parse::<usize>() {
        Ok(n) if n <= BODY_MAX => Ok(n),
        _ if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => Err(post_body_too_large()),
        _ => Err(POST_LEN_USAGE.to_string()),
    }
}

fn post_body_too_large() -> String {
    format!("ERR too large: a post body is capped at {BODY_MAX} bytes\n")
}

/// `post`'s end of options.
pub const POST_OPTIONS_END: &str = "--";

/// `post`'s leading option tokens out of `toks`.
pub fn post_options<'a>(toks: impl Iterator<Item = &'a str>) -> impl Iterator<Item = &'a str> {
    toks.take_while(|tok| *tok != POST_OPTIONS_END && post_option_token(tok))
}

fn post_option_token(tok: &str) -> bool {
    tok == "--wait"
        || tok.starts_with("--wait=")
        || tok == "--wait-ack"
        || tok.starts_with("--wait-ack=")
        || ["to=", "kind=", "re=", "dl=", "via=", "key=", "len="]
            .iter()
            .any(|k| tok.starts_with(k))
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
        if !post_option_token(tok) {
            return Some(at);
        }
        idx = at + tok.len();
    }
    None
}

const WAIT_DEFAULT_MS: u64 = 30_000;
const WAIT_MAX_MS: u64 = 600_000;

/// `post` — queue one outbound message and, for the kinds that need a correlation id, wait for the offset it landed at.
pub fn cmd_post(ctx: &dyn FabricSession, rest: &str, body: Option<Vec<u8>>) -> String {
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
            match v.parse() {
                Ok(n) => re = Some(n),
                Err(_) => return POST_USAGE.to_string(),
            }
        } else if let Some(v) = kv(tok, "dl") {
            match v.parse() {
                Ok(n) => dl = Some(n),
                Err(_) => return POST_USAGE.to_string(),
            }
        } else if let Some(v) = kv(tok, "via") {
            via = Some(v.to_string());
        } else if let Some(v) = kv(tok, "key") {
            // BOUNDED AT THE DOOR, like `to`: it is printed onto the `outbox`
            // line and into the bridge's state dir, so its charset is the
            // charset of the ids those already carry, and nothing longer.
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
            match v.parse::<u64>() {
                Ok(n) => wait = Some(Some(n.min(WAIT_MAX_MS))),
                Err(_) => return POST_USAGE.to_string(),
            }
        } else if tok == "--wait-ack" {
            wait_ack = Some(None);
        } else if let Some(v) = kv(tok, "--wait-ack") {
            match v.parse::<u64>() {
                Ok(n) => wait_ack = Some(Some(n.min(WAIT_MAX_MS))),
                Err(_) => return POST_USAGE.to_string(),
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
    let body = match (body, declared_len) {
        (Some(bytes), _) => bytes,
        (None, Some(_)) => {
            // A `len=` with no frame behind it: the caller announced bytes the
            // serve loop never saw. Refuse rather than post an empty message.
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
    let to_tok = pct_encode(&to);
    inbox.posts.push_back(PostRow {
        id,
        to: to_tok.clone(),
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
    let mut payload = format!("{id} to={to_tok} kind={kind}");
    if let Some(re) = re {
        payload.push_str(&format!(" re={re}"));
    }
    if let Some(dl) = dl {
        payload.push_str(&format!(" dl={dl}"));
    }
    if let Some(v) = &via {
        payload.push_str(&format!(" via={v}"));
    }
    if let Some(k) = &key {
        payload.push_str(&format!(" key={k}"));
    }
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
    let _ = ctx.record_fact("post", payload, fact);
    drop(inbox);
    ctx.fabric().changed.notify_all();
    ctx.fabric_changed();

    let Some(wait) = wait else {
        return format!("OK {id}\n");
    };
    if wait_is_futile() {
        return fabric_wait_refusal(id);
    }
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_millis(wait.unwrap_or(WAIT_DEFAULT_MS));
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
        guard = aterm_control::park::park_guarded(
            &ctx.fabric().changed,
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

fn wait_receipt_on_log(
    ctx: &dyn FabricSession,
    id: u64,
    dup: &str,
    post: &OffsetWatch,
    bound_ms: u64,
) -> String {
    let off = post.off;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(bound_ms);
    let mut guard = ctx.fabric().lock();
    loop {
        // The post's own record must still be on the log: a move that kept
        // it (a moved generation, below the fork) keeps the wait.
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
        guard = aterm_control::park::park_guarded(
            &ctx.fabric().changed,
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
    deadline.min(std::time::Instant::now() + horizon)
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

/// `await inbox since=<id> [kinds=<k,...>] [topic=<t>] | await inbox re=<off> [since=<id>] [kinds=<k,...>]`.
pub fn cmd_await_inbox(ctx: &dyn FabricSession, args: &[&str], timeout_ms: u64) -> String {
    const USAGE: &str = "ERR usage: await inbox since=<id> [kinds=<k,...>] [topic=<t>] \
                         [timeout=<ms>] | await inbox re=<off> [since=<id>] [kinds=<k,...>] \
                         [timeout=<ms>]\n";
    let mut since: Option<u64> = None;
    let mut re: Option<u64> = None;
    let mut kinds: Option<Vec<String>> = None;
    let mut topic: Option<String> = None;
    for tok in args {
        if let Some(v) = kv(tok, "since") {
            match v.parse() {
                Ok(n) => since = Some(n),
                Err(_) => return USAGE.to_string(),
            }
        } else if let Some(v) = kv(tok, "re") {
            match v.parse() {
                Ok(n) => re = Some(n),
                Err(_) => return USAGE.to_string(),
            }
        } else if let Some(v) = kv(tok, "kinds") {
            let list: Vec<String> = v.split(',').map(str::to_string).collect();
            if list
                .iter()
                .any(|k| !KINDS.contains(&k.as_str()) && k != "hold")
            {
                return USAGE.to_string();
            }
            kinds = Some(list);
        } else if let Some(v) = kv(tok, "topic") {
            // THE GRAMMAR `post to=say:<t>` AND `topic add` TAKE: a word outside
            // it is a topic no row can carry, so a wait on it could only time out.
            if !is_topic(v) {
                return USAGE.to_string();
            }
            topic = Some(v.to_string());
        } else {
            return USAGE.to_string();
        }
    }
    let since = match (since, re) {
        (Some(s), _) => s,
        (None, Some(_)) => 0,
        (None, None) => return USAGE.to_string(),
    };
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

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let mut guard = ctx.fabric().lock();
    if since > guard.last_msg_id {
        return format!("ERR bad since high={}\n", guard.last_msg_id);
    }
    let hold_at_arm = guard.holds.is_held();
    let named = re.map(|off| guard.watch(off, true));
    loop {
        // The named post's record must still be on the log: a moved
        // generation that kept it (below the fork) keeps the wait.
        if let Some(post) = &named
            && !post.on_log()
        {
            return format!("ERR log-replaced re={}\n", post.off);
        }
        if let Some(id) = guard.rows.iter().filter(|r| latches(r)).map(|r| r.id).max() {
            return format!("OK inbox {id}\n");
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
        guard = aterm_control::park::park_guarded(
            &ctx.fabric().changed,
            guard,
            deadline,
            aterm_control::park::Lane::SERVED,
        );
    }
}

#[cfg(test)]
mod test_support {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Condvar, Mutex, RwLock};
    use std::time::{Duration, Instant};

    use aterm_session::SessionId;

    use super::{FabricFact, FabricSession, Registry, SessionFabric, Timeline};

    /// What every record rings, as aterm-gui's timeline rings its session's watchers on each append.
    #[derive(Default)]
    pub(crate) struct Watchers {
        rung: Mutex<u64>,
        changed: Condvar,
    }

    impl Watchers {
        fn ring(&self) {
            *self.rung.lock().unwrap_or_else(|p| p.into_inner()) += 1;
            self.changed.notify_all();
        }
    }

    /// One watcher of a session's [`Watchers`].
    pub(crate) struct Subscription {
        watchers: Arc<Watchers>,
        seen: std::cell::Cell<u64>,
    }

    impl Subscription {
        /// A watcher of `watchers`, rung from now on.
        pub(crate) fn single(watchers: &Arc<Watchers>) -> Self {
            let seen = *watchers.rung.lock().unwrap_or_else(|p| p.into_inner());
            Self {
                watchers: Arc::clone(watchers),
                seen: std::cell::Cell::new(seen),
            }
        }

        /// Whether the session rang, waiting at most `bound` for a ring.
        pub(crate) fn wait(&self, bound: Duration) -> bool {
            let until = Instant::now() + bound;
            let mut rung = self.watchers.rung.lock().unwrap_or_else(|p| p.into_inner());
            loop {
                if *rung > self.seen.get() {
                    self.seen.set(*rung);
                    return true;
                }
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return false;
                }
                rung = self
                    .watchers
                    .changed
                    .wait_timeout(rung, left)
                    .unwrap_or_else(|p| p.into_inner())
                    .0;
            }
        }
    }

    /// One timeline record.
    pub(crate) struct Event {
        pub(crate) kind: &'static str,
        pub(crate) payload: String,
    }

    /// A session's timeline.
    #[derive(Default)]
    pub(crate) struct TestTimeline {
        events: Vec<Event>,
        pub(crate) facts: Vec<String>,
    }

    impl TestTimeline {
        /// Every record (the real timeline's `since(None)`).
        pub(crate) fn since(&self, after: Option<u64>) -> impl DoubleEndedIterator<Item = &Event> {
            assert!(after.is_none(), "the tests read the whole timeline");
            self.events.iter()
        }
    }

    impl Timeline for TestTimeline {
        fn record(&mut self, kind: &'static str, payload: String) {
            self.events.push(Event { kind, payload });
        }

        fn record_fact(
            &mut self,
            kind: &'static str,
            payload: String,
            fact: FabricFact<'_>,
        ) -> bool {
            self.facts.push(format!("{kind}: {fact:?}"));
            self.record(kind, payload);
            true
        }
    }

    /// A session: its id, its fabric endpoint, its timeline and the watchers every record rings.
    pub(crate) struct SessionCtx {
        pub(crate) self_id: SessionId,
        pub(crate) fabric: Arc<SessionFabric>,
        pub(crate) timeline: Arc<Mutex<TestTimeline>>,
        pub(crate) watchers: Arc<Watchers>,
    }

    impl FabricSession for SessionCtx {
        fn self_id(&self) -> &SessionId {
            &self.self_id
        }
        fn fabric(&self) -> &SessionFabric {
            &self.fabric
        }
        fn record(&self, kind: &'static str, payload: String) {
            self.timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record(kind, payload);
            self.watchers.ring();
        }
        fn record_fact(&self, kind: &'static str, payload: String, fact: FabricFact<'_>) -> bool {
            let owed = self
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record_fact(kind, payload, fact);
            self.watchers.ring();
            owed
        }
        fn fabric_changed(&self) {}
        fn holder_token(&self) -> Option<String> {
            None
        }
    }

    /// What `test_handle` hands a test.
    pub(crate) struct TestHandle {
        pub(crate) sid: SessionId,
        pub(crate) ctx: Arc<SessionCtx>,
        local_id: u64,
    }

    /// A fresh, unregistered session with process-local id `local_id`, on the test timing value (the fence and notice waits at their test lengths).
    pub(crate) fn test_handle(local_id: u64) -> TestHandle {
        aterm_gui_timing::select_fast();
        let sid = SessionId::generate();
        TestHandle {
            sid: sid.clone(),
            ctx: Arc::new(SessionCtx {
                self_id: sid,
                fabric: Arc::default(),
                timeline: Arc::default(),
                watchers: Arc::default(),
            }),
            local_id,
        }
    }

    /// The registry: live sessions by local id.
    #[derive(Default)]
    pub(crate) struct TestStore {
        by_local: BTreeMap<u64, Arc<SessionCtx>>,
        /// Every `link set-aside` count handed over ([`Registry::torn_state_set_aside`]), in order.
        pub(crate) set_aside: std::sync::Mutex<Vec<u64>>,
        /// Whether each of those came from a bridge that says when its quarantine is empty, in the same order.
        pub(crate) told_when_empty: std::sync::Mutex<Vec<bool>>,
        /// Every `link quarantine` count handed over ([`Registry::torn_state_held`]), in order.
        pub(crate) held: std::sync::Mutex<Vec<u64>>,
    }

    impl TestStore {
        /// Register a session; a first registration records `spawned`, as the real store's does.
        pub(crate) fn register(&mut self, h: TestHandle) {
            if !self.by_local.values().any(|c| c.self_id == h.sid) {
                h.ctx.record("spawned", "state=alive".to_string());
            }
            self.by_local.insert(h.local_id, h.ctx);
        }

        /// Drop the session with process-local id `local`, as a closed tab's.
        pub(crate) fn deregister_local(&mut self, local: u64) {
            self.by_local.remove(&local);
        }
    }

    impl Registry for TestStore {
        fn sessions(&self) -> Vec<Arc<dyn FabricSession>> {
            self.by_local
                .values()
                .map(|c| Arc::clone(c) as Arc<dyn FabricSession>)
                .collect()
        }
        fn session(&self, sid: &SessionId) -> Option<Arc<dyn FabricSession>> {
            self.by_local
                .values()
                .find(|c| &c.self_id == sid)
                .map(|c| Arc::clone(c) as Arc<dyn FabricSession>)
        }
        fn bridge_lost_advised(&self, _sessions: usize) {}
        fn torn_state_set_aside(&self, files: u64, told_when_empty: bool) {
            self.set_aside
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(files);
            self.told_when_empty
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(told_when_empty);
        }
        fn torn_state_held(&self, files: u64) {
            self.held
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(files);
        }
    }

    /// The tests' registry handle.
    pub(crate) type Store = Arc<RwLock<TestStore>>;

    /// A fresh, empty registry.
    pub(crate) fn new_store() -> Store {
        Arc::new(RwLock::new(TestStore::default()))
    }
}
#[cfg(test)]
#[path = "fabric_person_tests.rs"]
mod person_tests;
#[cfg(test)]
#[path = "fabric_remote_tests.rs"]
mod remote_tests;

#[cfg(test)]
mod topic_tests {
    use super::{TopicEntry, parse_topics, render_topics};

    #[test]
    fn the_carry_round_trips_and_a_malformed_row_invents_nothing() {
        let entry = |since: &str| TopicEntry {
            since: since.to_string(),
        };
        let live = vec![
            ("build.failed".to_string(), entry("head")),
            ("sat-comp".to_string(), entry("@42")),
        ];
        let wire = render_topics(&live);
        assert_eq!(wire, ["build.failed head", "sat-comp @42"]);
        assert_eq!(parse_topics(&wire), live);
        assert_eq!(
            parse_topics(&["old.topic @3 7".to_string()]),
            vec![("old.topic".to_string(), entry("@3"))]
        );

        let hostile: Vec<String> = [
            "Build head",        // not a topic
            "a/b head",          // a subject segment
            "ok yesterday",      // not a `since` token
            "ok @notanumber",    // nor is this
            "ok @+5",            // an offset has no sign
            "ok head x",         // a third field that is not a number
            "noseparator",       // no `since` at all
            "build.failed head", // …and one good row, so the drop is selective
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        assert_eq!(
            parse_topics(&hostile),
            vec![("build.failed".to_string(), entry("head"))],
        );
    }
}

#[cfg(test)]
mod inbox_hold {

    use super::*;
    use crate::test_support::{SessionCtx, Store, new_store, test_handle};
    use aterm_types::fabric_grammar::{PRINCIPAL_MAX, PRINCIPAL_NAME_MAX, VIA_MAX_HOPS};

    use super::with_link_reset as with_link;

    fn registered(store: &Store) -> (String, std::sync::Arc<SessionCtx>) {
        registered_as(store, 1)
    }

    fn attach_up(store: &Store, generation: BridgeGeneration) {
        bridge_attached(generation);
        assert!(link_report(store, Some(generation), true, "", Some(1)));
    }

    fn on_lane<T>(generation: BridgeGeneration, f: impl FnOnce() -> T) -> T {
        struct Restore(Option<BridgeGeneration>);
        impl Drop for Restore {
            fn drop(&mut self) {
                LANE_GENERATION.with(|lane| lane.set(self.0));
            }
        }
        let _restore = Restore(lane_generation());
        set_lane_generation(generation);
        f()
    }

    pub(super) fn registered_as(
        store: &Store,
        local_id: u64,
    ) -> (String, std::sync::Arc<SessionCtx>) {
        let h = test_handle(local_id);
        let sid = h.sid.as_str().to_string();
        let ctx = h.ctx.clone();
        store.write().unwrap_or_else(|p| p.into_inner()).register(h);
        (sid, ctx)
    }

    pub(super) fn deliver(
        store: &Store,
        sid: &str,
        off: u64,
        from: &str,
        kind: &str,
        text: &str,
    ) -> String {
        cmd_deliver(
            store,
            &format!("{sid} off={off} from={from} kind={kind} trust=agent text={text}"),
        )
    }

    pub(super) fn broadcast(store: &Store, sid: &str, off: u64, topic: &str, text: &str) -> String {
        cmd_deliver(
            store,
            &format!(
                "{sid} off={off} from=s-peer@n-lab kind=note trust=agent topic={topic} text={text}"
            ),
        )
    }

    #[test]
    fn mail_since_answers_metadata_a_cursor_and_the_oldest_row_held() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        for (n, (from, kind)) in [
            ("h-andrew", "task"),
            ("s-peer", "note"),
            ("h-andrew", "ask"),
        ]
        .iter()
        .enumerate()
        {
            let off = 100 + n as u64;
            assert!(deliver(&store, &sid, off, from, kind, "the%20words").starts_with("OK"));
        }

        let all = ctx.fabric.mail_since(0, &MailFilter::default());
        assert_eq!(all.rows.len(), 3, "{:?}", all.rows);
        assert_eq!(all.high, 3);
        assert_eq!(all.present, 3, "the ring holds all three");
        assert_eq!(
            all.rows[0],
            MailLine {
                id: 1,
                off: 100,
                from: "h-andrew".to_string(),
                kind: "task".to_string(),
                re: None,
                topic: None,
            }
        );

        let after = ctx.fabric.mail_since(2, &MailFilter::default());
        assert_eq!(after.rows.iter().map(|r| r.id).collect::<Vec<_>>(), [3]);

        let kinds = MailFilter {
            kinds: vec!["task".to_string(), "ask".to_string()],
            from: None,
            topic: None,
        };
        let got = ctx.fabric.mail_since(0, &kinds);
        assert_eq!(got.rows.iter().map(|r| r.id).collect::<Vec<_>>(), [1, 3]);
        assert_eq!(got.high, 3, "a filtered-out row still moves the cursor");
        let human = MailFilter {
            kinds: Vec::new(),
            from: Some("human".to_string()),
            topic: None,
        };
        assert_eq!(
            ctx.fabric
                .mail_since(0, &human)
                .rows
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            [1, 3]
        );
        let exact = MailFilter {
            kinds: Vec::new(),
            from: Some("s-peer".to_string()),
            topic: None,
        };
        assert_eq!(
            ctx.fabric
                .mail_since(0, &exact)
                .rows
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            [2]
        );
        assert_eq!(sender_class("h-a"), "human");
        assert_eq!(sender_class("s-a"), "agent");
        assert_eq!(sender_class("a-a"), "service");
        assert_eq!(sender_class("weird"), "other");
    }

    #[test]
    fn mail_since_reports_what_was_evicted_even_when_the_front_row_is_pinned() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert!(deliver(&store, &sid, 100, "h-andrew", "task", "x").starts_with("OK"));
        let overflow = RING_CAP + 100;
        for n in 1..overflow {
            let off = 1000 + n as u64;
            let from = format!("s-p{n}");
            let reply = deliver(&store, &sid, off, &from, "note", "x");
            assert!(reply.starts_with("OK"), "row {n}: {reply}");
        }
        let since = ctx.fabric.mail_since(0, &MailFilter::default());
        assert_eq!(since.high, overflow as u64, "every row was issued");
        assert_eq!(
            since.present, RING_CAP as u64,
            "the ring holds its cap, no more"
        );
        let lost = since.high - since.present;
        assert_eq!(lost, (overflow - RING_CAP) as u64);
        assert!(lost > 0, "this fixture must actually lose rows");
        assert_eq!(
            ctx.fabric.lock().rows.front().map(|r| r.id),
            Some(1),
            "the human row is pinned at the front, which is the trap"
        );
    }

    fn bridge_hold(store: &Store, rest: &str) -> String {
        cmd_hold(store, rest, HoldIssuer::Bridge)
    }

    fn owner_hold(store: &Store, rest: &str) -> String {
        cmd_hold(store, rest, HoldIssuer::Owner)
    }

    #[cfg(unix)]
    #[test]
    fn a_committing_update_holds_every_hold_back_and_drops_none() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let serial = ctx.fabric.fence_hold(true);
        assert_eq!(
            cmd_hold_within(
                &store,
                &format!("{sid} on reason=stop"),
                HoldIssuer::Owner,
                Some(std::time::Duration::from_millis(50)),
            ),
            HOLD_BUSY
        );
        assert_eq!(ctx.fabric.hold(), None);
        assert_eq!(ctx.fabric.hold_serial(), serial, "nothing moved");

        let before = ctx.fabric.fence_waits();
        let waiter = {
            let ctx = std::sync::Arc::clone(&ctx);
            std::thread::spawn(move || {
                apply_hold(
                    &ctx,
                    HoldAct::Set(Hold {
                        reason: "fabric-lost".to_string(),
                        origin: "fleet".to_string(),
                    }),
                    HoldIssuer::Bridge,
                    None,
                    &|| {},
                )
            })
        };
        assert_parked_on_the_fence(
            &ctx.fabric,
            before,
            || waiter.is_finished(),
            "the sweep waits out the Commit",
        );
        assert_eq!(ctx.fabric.hold(), None);
        assert_eq!(ctx.fabric.hold_serial(), serial);
        ctx.fabric.lift_hold_fence();
        assert_eq!(
            waiter.join().expect("the sweep"),
            Applied::Changed { held: true }
        );
        assert_eq!(
            ctx.fabric.hold().map(|hold| hold.origin),
            Some("fleet".to_string())
        );
        assert_ne!(ctx.fabric.hold_serial(), serial);
        assert_eq!(owner_hold(&store, &format!("{sid} off")), HOLD_DENIED);
    }

    #[test]
    fn post_and_deliver_hold_their_words_to_the_fabric_grammar() {
        let name = |n: usize| "a".repeat(n);
        let store = new_store();
        let (sid, ctx) = registered(&store);

        let long = format!("h-{}", name(PRINCIPAL_NAME_MAX + 1));
        assert_eq!(
            cmd_post(&ctx, &format!("to={long} kind=note hi"), None),
            POST_USAGE
        );
        assert_eq!(
            cmd_deliver(
                &store,
                &format!("{sid} off=1 from={long} kind=note trust=human text=x")
            ),
            DELIVER_USAGE
        );
        assert!(valid_principal(&format!(
            "s-{}@n-{}",
            name(PRINCIPAL_NAME_MAX),
            name(PRINCIPAL_NAME_MAX)
        )));
        assert!(!valid_principal(&format!(
            "s-{}@n-{}",
            name(PRINCIPAL_NAME_MAX),
            name(PRINCIPAL_NAME_MAX + 1)
        )));

        let hop = format!("s-{}", name(PRINCIPAL_NAME_MAX));
        let over = vec![hop.as_str(); VIA_MAX_HOPS + 1].join(",");
        assert_eq!(
            cmd_post(&ctx, &format!("to=@s-b kind=note via={over} x"), None),
            POST_USAGE
        );
        assert_eq!(
            cmd_post(&ctx, "to=@s-b kind=note via=s-a@n-b x", None),
            POST_USAGE
        );
        assert_eq!(
            cmd_deliver(
                &store,
                &format!("{sid} off=1 from=h-a kind=note trust=human via={over} text=x")
            ),
            DELIVER_USAGE
        );
        assert_eq!(
            cmd_post(&ctx, "to=@s-b kind=note via=s-a x", None),
            "OK 1\n"
        );
    }

    #[test]
    fn a_queued_post_counts_its_via_and_a_retired_one_keeps_neither() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let via = vec!["s-aaaaaaaa"; VIA_MAX_HOPS].join(",");
        let body = "x".repeat(1000);
        assert_eq!(
            cmd_post(&ctx, &format!("to=@s-b kind=note via={via} {body}"), None),
            "OK 1\n"
        );
        let (queued, bytes) = ctx.fabric.lock().queued_load();
        assert_eq!(queued, 1);
        assert_eq!(
            bytes,
            body.len() + via.len(),
            "the byte bound counts the body AND the relay chain"
        );

        assert_eq!(
            cmd_outbox_sent(&store, &format!("{sid} 1 off=91")),
            "OK\n",
            "the bridge published it"
        );
        let inbox = ctx.fabric.lock();
        let row = inbox.posts.iter().find(|p| p.id == 1).expect("the row");
        assert_eq!(row.off, Some(91));
        assert!(row.body.is_empty(), "the body goes at retirement");
        assert_eq!(
            row.via, None,
            "and so does the relay chain: a retired row is kept only to resolve \
             an old answer's `re-id=`, and 512 of them are held for the life of \
             the session"
        );
        assert_eq!(row.caller_sized_bytes(), 0);
        drop(inbox);
        assert_eq!(ctx.fabric.lock().queued_load(), (0, 0));
    }

    #[test]
    fn an_address_carries_one_at_sign_and_not_a_run_of_them() {
        let store = new_store();
        let (_sid, ctx) = registered(&store);
        assert_eq!(cmd_post(&ctx, "to=@s-abc kind=note hi", None), "OK 1\n");
        assert_eq!(
            cmd_post(&ctx, "to=@s-abc@n-lab kind=note hi", None),
            "OK 2\n"
        );
        assert_eq!(cmd_post(&ctx, "to=h-andrew kind=note hi", None), "OK 3\n");
        assert_eq!(cmd_post(&ctx, "to=say kind=note hi", None), "OK 4\n");
        assert_eq!(cmd_post(&ctx, "to=@@s-abc kind=note hi", None), POST_USAGE);
        let padded = format!("{}s-abc", "@".repeat(4096));
        assert_eq!(
            cmd_post(&ctx, &format!("to={padded} kind=note hi"), None),
            POST_USAGE
        );
        let inbox = ctx.fabric.lock();
        assert_eq!(inbox.posts.len(), 4, "the two padded ones never queued");
        for row in &inbox.posts {
            assert!(
                row.to.len() <= 2 * PRINCIPAL_MAX + 2,
                "`to` is bounded by the principal grammar: {}",
                row.to.len()
            );
        }
    }

    #[test]
    fn the_door_measures_the_row_the_fold_will_measure() {
        let store = new_store();
        let (_sid, ctx) = registered(&store);
        let fills = OUTBOX_BYTES_MAX / BODY_MAX - 1;
        for i in 0..fills {
            assert_eq!(
                cmd_post(
                    &ctx,
                    &format!("to=@s-abc kind=note len={BODY_MAX}"),
                    Some(vec![b'x'; BODY_MAX])
                ),
                format!("OK {}\n", i + 1)
            );
        }
        assert_eq!(
            ctx.fabric.lock().queued_load(),
            (fills, OUTBOX_BYTES_MAX - BODY_MAX)
        );

        let via = format!("s-{}", "a".repeat(18));
        assert_eq!(via.len(), 20);
        assert!(
            cmd_post(
                &ctx,
                &format!("to=@s-abc kind=note via={via} len={}", BODY_MAX - 10),
                Some(vec![b'x'; BODY_MAX - 10])
            )
            .starts_with("ERR outbox full"),
            "the incoming row's relay chain counts too"
        );

        assert!(
            cmd_post(
                &ctx,
                &format!("to=@s-abc kind=note len={}", 100 * 1024),
                Some(vec![0xff; 100 * 1024])
            )
            .starts_with("ERR outbox full"),
            "a frame of non-UTF-8 is weighed as the row it becomes"
        );

        assert_eq!(
            cmd_post(
                &ctx,
                &format!("to=@s-abc kind=note len={}", BODY_MAX - 10),
                Some(vec![b'x'; BODY_MAX - 10])
            ),
            format!("OK {}\n", fills + 1)
        );
        assert_eq!(
            ctx.fabric.lock().queued_load(),
            (fills + 1, OUTBOX_BYTES_MAX - 10)
        );
    }

    fn rows(reply: &str) -> Vec<&str> {
        reply.lines().skip(1).collect()
    }

    #[test]
    fn the_header_count_is_every_row_that_follows() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert_eq!(
            deliver(&store, &sid, 10, "h-andrew", "task", "go"),
            "OK 1\n"
        );
        assert_eq!(cmd_post(&ctx, "to=@s-abc kind=note hello", None), "OK 1\n");
        let reply = cmd_inbox(&ctx, "");
        let header = reply.lines().next().expect("a header");
        assert!(header.starts_with("OK 2 "), "{header}");
        assert_eq!(rows(&reply).len(), 2, "{reply}");
        assert!(rows(&reply)[0].starts_with("msg 1 off=10 "), "{reply}");
        assert!(
            rows(&reply)[1].starts_with("post 1 to=@s-abc kind=note off=-"),
            "{reply}"
        );
        for field in [
            "hold=0",
            "holder=-",
            "seen=0",
            "bus_head=10",
            "dropped=0",
            "pending=0",
        ] {
            assert!(header.contains(field), "header lacks {field}: {header}");
        }
    }

    #[test]
    fn deliver_is_idempotent_on_the_offset() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert_eq!(deliver(&store, &sid, 90312, "h-a", "ask", "one"), "OK 1\n");
        assert_eq!(
            deliver(&store, &sid, 90312, "h-a", "ask", "one"),
            "OK 1\n",
            "a redelivered offset answers the id it first got"
        );
        assert_eq!(
            deliver(&store, &sid, 90312, "h-a", "ask", "two"),
            "OK 1\n",
            "the offset decides, not the payload"
        );
        assert_eq!(rows(&cmd_inbox(&ctx, "--peek")).len(), 1);
        assert_eq!(deliver(&store, &sid, 90313, "h-a", "ask", "next"), "OK 2\n");
        assert_eq!(rows(&cmd_inbox(&ctx, "--peek")).len(), 2);
    }

    #[test]
    fn a_walk_again_neither_duplicates_a_held_row_nor_counts_a_refusal_twice() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert_eq!(
            ctx.fabric.topic_add("news", "head", &ctx.timeline),
            Ok(true)
        );
        assert_eq!(deliver(&store, &sid, 1, "h-a", "task", "keep"), "OK 1\n");
        let cap = u64::try_from(DEDUP_CAP).unwrap();
        for i in 0..cap {
            let from = format!("s-x@n-p{i}");
            assert!(deliver(&store, &sid, 100 + i, &from, "note", "n").starts_with("OK "));
        }
        assert!(
            !ctx.fabric.lock().dedup_set.contains(&1),
            "the window forgot it"
        );
        assert_eq!(
            deliver(&store, &sid, 1, "h-a", "task", "keep"),
            "OK 1\n",
            "the ring still holds it: the id it got, no second row"
        );
        assert_eq!(
            ctx.fabric.lock().rows.iter().filter(|r| r.off == 1).count(),
            1
        );

        let fresh = new_store();
        let (bsid, bctx) = registered(&fresh);
        assert_eq!(
            bctx.fabric.topic_add("news", "head", &bctx.timeline),
            Ok(true)
        );
        let quota = u64::try_from(SENDER_QUOTA).unwrap();
        for off in 0..quota {
            assert!(broadcast(&fresh, &bsid, 5000 + off, "news", "b").starts_with("OK "));
        }
        let refused = 5000 + quota;
        assert_eq!(
            broadcast(&fresh, &bsid, refused, "news", "b"),
            "ERR quota\n"
        );
        assert_eq!(bctx.fabric.lock().dropped, 1);
        assert_eq!(
            broadcast(&fresh, &bsid, refused, "news", "b"),
            "OK 0\n",
            "a walk again: taken once, held no more"
        );
        assert_eq!(bctx.fabric.lock().dropped, 1, "counted once");
        assert_eq!(broadcast(&fresh, &bsid, 5000, "news", "b"), "OK 1\n");
        assert_eq!(
            broadcast(&fresh, &bsid, refused + 1, "news", "b"),
            "ERR quota\n",
            "an offset neither holds is judged as new"
        );
        for off in 0..quota {
            let reply = deliver(&fresh, &bsid, 9000 + off, "s-peer@n-far", "note", "a");
            assert!(reply.starts_with("OK "), "{reply}");
        }
        let addressed = 9000 + quota;
        for _ in 0..2 {
            assert_eq!(
                deliver(&fresh, &bsid, addressed, "s-peer@n-far", "note", "a"),
                "ERR quota\n"
            );
        }
        assert_eq!(
            bctx.fabric.lock().dropped,
            2,
            "the second broadcast refusal"
        );
    }

    #[test]
    fn a_broadcast_row_is_marked_refilled_only_under_a_bridge_that_said_so() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            assert_eq!(
                ctx.fabric.topic_add("news", "head", &ctx.timeline),
                Ok(true)
            );
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let refill_of = |off: u64| {
                ctx.fabric
                    .lock()
                    .rows
                    .iter()
                    .find(|r| r.off == off)
                    .map(|r| r.refill)
            };
            assert!(broadcast(&store, &sid, 10, "news", "b").starts_with("OK "));
            assert!(cmd_hello("lane=2 build=x", Some(generation)).starts_with("OK "));
            assert!(broadcast(&store, &sid, 11, "news", "b").starts_with("OK "));
            assert!(cmd_hello("lane=2 refill=in", Some(generation)).starts_with("OK "));
            assert!(broadcast(&store, &sid, 12, "news", "b").starts_with("OK "));
            assert!(cmd_hello("lane=2 refill=in,say,later", Some(generation)).starts_with("OK "));
            assert!(broadcast(&store, &sid, 13, "news", "b").starts_with("OK "));
            assert!(deliver(&store, &sid, 14, "h-a", "note", "a").starts_with("OK "));
            assert_eq!(
                [10, 11, 12, 13, 14].map(refill_of),
                [
                    Some(false),
                    Some(false),
                    Some(false),
                    Some(true),
                    Some(false)
                ]
            );
            assert!(bridge_refills_broadcasts());
            let stale = next_bridge_generation();
            assert!(cmd_hello("lane=2", Some(stale)).starts_with("OK stale=1"));
            assert!(
                bridge_refills_broadcasts(),
                "a ghost lane's hello is recorded nowhere"
            );
        });
    }

    #[test]
    fn the_mirror_is_acknowledged_only_to_a_recorded_hello() {
        with_link(|| {
            let store = new_store();
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            assert_eq!(fabric_mirror_facts(), "", "off: the line as it was");
            let off = cmd_hello("lane=2 build=x", Some(generation));
            assert!(off.starts_with("OK ") && !off.contains("log="), "{off}");
            assert!(!bridge_mirrors_logs());
            let on = cmd_hello("lane=2 build=x log=turns", Some(generation));
            assert!(on.trim_end().ends_with(" log=turns"), "{on}");
            assert!(bridge_mirrors_logs());
            assert_eq!(fabric_mirror_facts(), " bridge_log=turns mirror=starting");
            assert_eq!(cmd_link(&store, "mirror budget", Some(generation)), "OK\n");
            assert_eq!(fabric_mirror_facts(), " bridge_log=turns mirror=budget");
            assert_eq!(
                cmd_link(&store, "mirror stopped reason=refused", Some(generation)),
                "OK\n"
            );
            assert_eq!(
                fabric_mirror_facts(),
                " bridge_log=turns mirror=stopped:refused"
            );
            let ghost = next_bridge_generation();
            assert!(lane_owns(Some(generation)) && !lane_owns(Some(ghost)));
            let stale = cmd_hello("lane=2 log=turns", Some(ghost));
            assert!(
                stale.starts_with("OK stale=1") && !stale.contains("log="),
                "a ghost lane is never told the mirror is served: {stale}"
            );
            assert_eq!(cmd_link(&store, "mirror on", Some(ghost)), "OK stale=1\n");
            assert_eq!(
                fabric_mirror_facts(),
                " bridge_log=turns mirror=stopped:refused",
                "a ghost lane's report records nothing"
            );
            assert!(cmd_link(&store, "mirror sideways", Some(generation)).starts_with("ERR usage"));
            let next = next_bridge_generation();
            attach_up(&store, next);
            assert!(!bridge_mirrors_logs());
            assert_eq!(fabric_mirror_facts(), "");
        });
    }

    #[test]
    fn an_auto_hello_asks_for_nothing_and_says_unretained() {
        with_link(|| {
            let store = new_store();
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let auto = cmd_hello("lane=5 build=x log=auto", Some(generation));
            assert!(auto.starts_with("OK ") && !auto.contains("log="), "{auto}");
            assert!(!bridge_mirrors_logs(), "auto asks for nothing yet");
            assert_eq!(fabric_mirror_facts(), " bridge_log=auto mirror=starting");
            assert_eq!(
                cmd_link(&store, "mirror unretained", Some(generation)),
                "OK\n"
            );
            assert_eq!(fabric_mirror_facts(), " bridge_log=auto mirror=unretained");
            assert!(!bridge_mirrors_logs());
            let turns = cmd_hello("lane=5 build=x log=turns", Some(generation));
            assert!(turns.trim_end().ends_with(" log=turns"), "{turns}");
            assert!(bridge_mirrors_logs());
            assert_eq!(cmd_link(&store, "mirror on", Some(generation)), "OK\n");
            assert_eq!(fabric_mirror_facts(), " bridge_log=turns mirror=on");
        });
    }

    #[test]
    fn a_set_aside_count_reaches_the_registry_only_from_the_owning_lane() {
        struct ReplaceDuringPublication {
            owner: BridgeGeneration,
            next: BridgeGeneration,
            received: Mutex<Vec<(u64, bool)>>,
        }

        impl Registry for ReplaceDuringPublication {
            fn sessions(&self) -> Vec<Arc<dyn FabricSession>> {
                Vec::new()
            }
            fn session(&self, _: &SessionId) -> Option<Arc<dyn FabricSession>> {
                None
            }
            fn bridge_lost_advised(&self, _: usize) {}
            fn torn_state_set_aside(&self, files: u64, _: bool) {
                let transition = match LINK.log_transition.try_lock() {
                    Ok(guard) => Some(guard),
                    Err(std::sync::TryLockError::Poisoned(error)) => Some(error.into_inner()),
                    Err(std::sync::TryLockError::WouldBlock) => None,
                };
                if let Some(guard) = transition {
                    drop(guard);
                    bridge_attached(self.next);
                }
                self.received
                    .lock()
                    .unwrap()
                    .push((files, lane_owns(Some(self.owner))));
            }
            fn torn_state_held(&self, _: u64) {}
        }

        with_link(|| {
            let store = new_store();
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let taken = || store.read().unwrap().set_aside.lock().unwrap().clone();
            assert_eq!(
                cmd_link(&store, "set-aside files=3", Some(generation)),
                "OK\n"
            );
            assert_eq!(taken(), vec![3]);
            let ghost = next_bridge_generation();
            assert_eq!(
                cmd_link(&store, "set-aside files=2", Some(ghost)),
                "OK stale=1\n"
            );
            assert_eq!(cmd_link(&store, "set-aside files=2", None), "OK stale=1\n");
            for bad in [
                "set-aside",
                "set-aside files=0",
                "set-aside files=-1",
                "set-aside files=x",
                "set-aside n=2",
                "set-aside files=2 more",
            ] {
                assert!(
                    cmd_link(&store, bad, Some(generation)).starts_with("ERR usage"),
                    "{bad}"
                );
            }
            assert_eq!(taken(), vec![3], "nothing but the owner's one line");

            let next = next_bridge_generation();
            let replacing = Arc::new(RwLock::new(ReplaceDuringPublication {
                owner: generation,
                next,
                received: Mutex::new(Vec::new()),
            }));
            assert_eq!(
                cmd_link(&replacing, "set-aside files=4", Some(generation)),
                "OK\n"
            );
            assert_eq!(
                *replacing.read().unwrap().received.lock().unwrap(),
                vec![(4, true)],
                "replacement cannot cross the owning lane's publication"
            );
            attach_up(&store, next);
            assert_eq!(
                cmd_link(&store, "set-aside files=2", Some(generation)),
                "OK stale=1\n"
            );
            assert_eq!(cmd_link(&store, "set-aside files=2", Some(next)), "OK\n");
            assert_eq!(taken(), vec![3, 2]);
        });
    }

    #[test]
    fn a_set_aside_says_whether_its_bridge_tells_when_the_quarantine_is_empty() {
        with_link(|| {
            let store = new_store();
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let told = || {
                store
                    .read()
                    .unwrap()
                    .told_when_empty
                    .lock()
                    .unwrap()
                    .clone()
            };
            assert_eq!(
                cmd_link(&store, "set-aside files=1", Some(generation)),
                "OK\n"
            );
            assert_eq!(told(), [false], "no hello: lane 1, which never tells");
            assert!(cmd_hello("lane=4 build=x", Some(generation)).starts_with("OK "));
            assert_eq!(
                cmd_link(&store, "set-aside files=1", Some(generation)),
                "OK\n"
            );
            assert_eq!(told(), [false, false], "lane 4 never tells");
            let lane = aterm_types::control_verbs::LINK_QUARANTINE_LANE;
            assert!(
                cmd_hello(&format!("lane={lane} build=x"), Some(generation)).starts_with("OK ")
            );
            assert_eq!(
                cmd_link(&store, "set-aside files=1", Some(generation)),
                "OK\n"
            );
            assert_eq!(told(), [false, false, true], "lane {lane} tells");
            let next = next_bridge_generation();
            attach_up(&store, next);
            assert_eq!(cmd_link(&store, "set-aside files=1", Some(next)), "OK\n");
            assert_eq!(
                told(),
                [false, false, true, false],
                "the next bridge has said no hello yet"
            );
        });
    }

    #[test]
    fn a_quarantine_count_reaches_the_registry_only_from_the_owning_lane() {
        with_link(|| {
            let store = new_store();
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let held = || store.read().unwrap().held.lock().unwrap().clone();
            for (line, n) in [("quarantine torn=2", 2), ("quarantine torn=0", 0)] {
                assert_eq!(cmd_link(&store, line, Some(generation)), "OK\n", "{line}");
                assert_eq!(held().last(), Some(&n), "{line}");
            }
            let ghost = next_bridge_generation();
            assert_eq!(
                cmd_link(&store, "quarantine torn=1", Some(ghost)),
                "OK stale=1\n"
            );
            assert_eq!(cmd_link(&store, "quarantine torn=1", None), "OK stale=1\n");
            for bad in [
                "quarantine",
                "quarantine torn=-1",
                "quarantine torn=x",
                "quarantine files=0",
                "quarantine torn=0 more",
            ] {
                assert!(
                    cmd_link(&store, bad, Some(generation)).starts_with("ERR usage"),
                    "{bad}"
                );
            }
            assert_eq!(held(), vec![2, 0], "nothing but the owner's two lines");
            assert!(
                store.read().unwrap().set_aside.lock().unwrap().is_empty(),
                "a count held is never one set aside"
            );
        });
    }

    #[test]
    fn a_broadcast_on_a_topic_the_session_does_not_hold_is_refused() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert_eq!(
            broadcast(&store, &sid, 10, "news", "unasked"),
            "ERR not-subscribed\n",
            "an empty set receives nothing"
        );
        assert_eq!(
            ctx.fabric.topic_add("news", "head", &ctx.timeline),
            Ok(true)
        );
        assert_eq!(broadcast(&store, &sid, 11, "news", "wanted"), "OK 1\n");
        assert_eq!(
            broadcast(&store, &sid, 12, "other", "elsewhere"),
            "ERR not-subscribed\n",
            "one topic held is not every topic"
        );
        assert!(ctx.fabric.topic_drop("news", &ctx.timeline));
        assert_eq!(
            broadcast(&store, &sid, 13, "news", "after-the-drop"),
            "ERR not-subscribed\n"
        );
        assert_eq!(
            broadcast(&store, &sid, 11, "news", "wanted"),
            "OK 1\n",
            "a redelivered offset still answers the id it first got"
        );
        assert_eq!(
            deliver(&store, &sid, 14, "h-a", "task", "addressed"),
            "OK 2\n",
            "the set gates broadcasts, not addressed mail"
        );
        let header = cmd_inbox(&ctx, "--peek");
        assert!(
            header.starts_with("OK 2 ") && header.contains(" dropped=0 "),
            "the two taken rows and no loss counted for the refusals: {header}"
        );
        assert_eq!(ctx.fabric.topic_add("news", "@13", &ctx.timeline), Ok(true));
        assert_eq!(
            broadcast(&store, &sid, 13, "news", "after-the-drop"),
            "OK 3\n"
        );
    }

    #[test]
    fn the_watermarks_move_only_where_they_should() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        for off in 1..=3 {
            deliver(&store, &sid, off, "h-a", "task", "x");
        }
        assert!(cmd_inbox(&ctx, "--peek").starts_with("OK 3 hold=0 holder=- seen=0 "));
        assert!(cmd_inbox(&ctx, "--peek").starts_with("OK 3 hold=0 holder=- seen=0 "));
        {
            let inbox = ctx.fabric.lock();
            assert!(inbox.rows.iter().all(|r| !r.listed), "--peek listed a row");
        }
        assert!(cmd_inbox(&ctx, "").contains(" seen=0 "));
        {
            let inbox = ctx.fabric.lock();
            assert!(inbox.rows.iter().all(|r| r.listed));
        }
        assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=2\n");
        assert_eq!(
            cmd_inbox_seen(&ctx, "1 deferred"),
            "OK seen=2\n",
            "the handled watermark never goes backwards"
        );
        assert_eq!(cmd_inbox_seen(&ctx, "99"), "ERR no such message\n");
        assert_eq!(cmd_inbox_seen(&ctx, "2 shrugged"), INBOX_USAGE);
        assert!(cmd_inbox(&ctx, "--peek").contains(" seen=2 "));
        assert!(cmd_topic(&ctx, "ls").starts_with("OK 0 seen=2\n"));
        assert_eq!(rows(&cmd_inbox(&ctx, "since=2 --peek")).len(), 1);
        let one = cmd_inbox(&ctx, "1 --peek");
        assert!(one.starts_with("OK 1 "), "{one}");
        assert!(rows(&one)[0].starts_with("msg 3 "), "newest kept: {one}");
    }

    #[test]
    fn a_long_body_is_cut_in_the_row_and_whole_through_inbox_get() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let body = "z".repeat(TEXT_PREVIEW_MAX + 40);
        assert_eq!(deliver(&store, &sid, 5, "h-a", "report", &body), "OK 1\n");
        let row = rows(&cmd_inbox(&ctx, "--peek"))[0].to_string();
        assert!(row.contains(&format!(" len={} ", body.len())), "{row}");
        assert!(row.contains(" more=1 "), "{row}");
        let text = row.split(" text=").nth(1).expect("a text field");
        assert_eq!(text.len(), TEXT_PREVIEW_MAX, "cut at the preview bound");
        let meta = rows(&cmd_inbox(&ctx, "--peek --meta"))[0].to_string();
        assert!(!meta.contains(" text="), "--meta carries no body: {meta}");
        assert!(meta.contains(" more=1"), "{meta}");
        let full = cmd_inbox_get(&ctx, "1");
        assert_eq!(full, format!("OK {}\n{body}", body.len()));
        assert_eq!(cmd_inbox_get(&ctx, "77"), "ERR no such message\n");
        assert!(cmd_inbox(&ctx, "--peek").contains(" seen=0 "));
    }

    #[test]
    fn every_fabric_site_hands_its_facts_to_the_one_hook() {
        use std::time::Duration;
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let woke = std::cell::Cell::new(0u32);
        let wake = |s: &SessionId| {
            assert_eq!(s.as_str(), sid, "the session's own id");
            woke.set(woke.get() + 1);
        };
        let line = format!("{sid} off=7 from=h-a kind=task trust=human text=SECRET%20BODY");
        assert_eq!(cmd_deliver_waking(&store, &line, &wake), "OK 1\n");
        assert_eq!(cmd_deliver_waking(&store, &line, &wake), "OK 1\n");
        let watcher = test_support::Subscription::single(&ctx.watchers);
        let _ = cmd_post(&ctx, "to=@s-peer kind=ask --wait=0 SECRET-POST", None);
        assert!(watcher.wait(Duration::ZERO), "the post's record rang");
        let landed = format!("{sid} landed=1 off=9");
        assert_eq!(cmd_deliver_waking(&store, &landed, &wake), "OK\n");
        assert_eq!(cmd_deliver_waking(&store, &landed, &wake), "OK\n");
        let _ = watcher.wait(Duration::ZERO);
        let _ = cmd_inbox_seen(&ctx, "1 handled");
        assert!(watcher.wait(Duration::ZERO), "the seen's record rang");
        let _ = cmd_inbox_seen(&ctx, "1 handled");
        let hold = |rest: &str| {
            cmd_hold_waking(&store, &format!("{sid} {rest}"), HoldIssuer::Owner, &wake)
        };
        assert_eq!(hold("on reason=demo"), "OK hold=1\n");
        assert_eq!(hold("on reason=demo"), "OK hold=1\n");
        assert_eq!(hold("off"), "OK hold=0\n");
        assert_eq!(
            ctx.fabric.topic_add("builds", "@4", &ctx.timeline),
            Ok(true)
        );
        assert_eq!(
            ctx.fabric.topic_add("builds", "@5", &ctx.timeline),
            Ok(false)
        );
        assert!(ctx.fabric.topic_drop("builds", &ctx.timeline));
        assert!(!ctx.fabric.topic_drop("builds", &ctx.timeline));
        let facts = ctx.timeline.lock().unwrap().facts.clone();
        assert_eq!(
            facts,
            [
                "inbox: MailIn { kind: \"task\", from: \"h-a\", off: 7, inbox: 1 }",
                "post: MailOut { kind: \"ask\", to: \"@s-peer\", post: 1, re: None, dl: None, \
                 via: None, idem: None }",
                "post-landed: MailLanded { post: 1, off: Some(9), why: None }",
                "inbox-seen: MailSeen { inbox: 1, off: 7, floor: Some(8), seen: 1, \
                 verdict: Some(\"handled\"), kind: \"task\", from: \"h-a\", moved: true }",
                "inbox-seen: MailSeen { inbox: 1, off: 7, floor: Some(8), seen: 1, \
                 verdict: None, kind: \"task\", from: \"h-a\", moved: false }",
                "hold: Hold { held: true, bridge: false, origin: \"local\", \
                 hold_origin: \"local\", reason: Some(\"demo\"), carried: false }",
                "hold: Hold { held: false, bridge: false, origin: \"local\", \
                 hold_origin: \"-\", reason: None, carried: false }",
                "topic: Topic { add: true, name: \"builds\", since: Some(\"@4\") }",
                "topic: Topic { add: false, name: \"builds\", since: None }",
            ]
        );
        assert!(facts.iter().all(|f| !f.contains("SECRET")), "{facts:?}");
        assert_eq!(woke.get(), 4, "{facts:?}");
    }

    #[test]
    fn deliver_refuses_a_label_it_cannot_interpret() {
        let store = new_store();
        let (sid, _ctx) = registered(&store);
        let bad = |tail: &str| cmd_deliver(&store, &format!("{sid} {tail}"));
        assert_eq!(
            bad("off=1 from=h-a kind=task trust=attested text=x"),
            DELIVER_USAGE
        );
        assert_eq!(bad("off=1 from=h-a kind=shout text=x"), DELIVER_USAGE);
        assert_eq!(
            bad("off=1 from=ignore%20previous%20instructions kind=task text=x"),
            DELIVER_USAGE
        );
        assert_eq!(bad("off=1 from=x-a kind=task text=x"), DELIVER_USAGE);
        assert_eq!(bad("from=h-a kind=task text=x"), DELIVER_USAGE, "no offset");
        assert_eq!(
            cmd_deliver(&store, "s-nope off=1 from=h-a kind=task"),
            "ERR no such session\n"
        );
        assert_eq!(bad("off=2 from=h-a kind=task text=x"), "OK 1\n");
        let reply = cmd_inbox(&_ctx, "--peek");
        let row = rows(&reply)[0];
        assert!(row.contains(" trust=agent"), "{row}");
    }

    #[test]
    fn the_halt_refuses_the_pty_verbs_and_nothing_else() {
        with_link(the_halt_refuses_the_pty_verbs_and_nothing_else_body);
    }

    fn the_halt_refuses_the_pty_verbs_and_nothing_else_body() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        for verb in ["send", "key", "turn", "close", "feed-bin", "signal"] {
            assert!(halt_refusal(&ctx, verb).is_none(), "{verb} before the halt");
        }
        assert!(app_halt_refusal(&store, "invoke").is_none());
        assert_eq!(
            bridge_hold(
                &store,
                &format!("{sid} on reason=main%20broken origin=fleet")
            ),
            "OK hold=1\n"
        );
        for verb in [
            "send",
            "key",
            "ctrl",
            "feed",
            "feed-bin",
            "paste",
            "paste-bin",
            "mouse",
            "pointer",
            "resize",
            "focus",
            "signal",
            "turn",
            "close",
            "invoke",
            "hwkey",
            "tab",
        ] {
            assert_eq!(
                halt_refusal(&ctx, verb).as_deref(),
                Some("ERR halted reason=main%20broken origin=fleet\n"),
                "{verb} must be refused under a halt"
            );
        }
        assert_eq!(
            app_halt_refusal(&store, "hwkey").as_deref(),
            Some("ERR halted reason=main%20broken origin=fleet\n"),
            "hwkey posts a real NSEvent on the physical-keypress path: a halt that let it \
             through would stop the polite routes and leave the indistinguishable one"
        );
        assert_eq!(
            app_halt_refusal(&store, "invoke").as_deref(),
            Some("ERR halted reason=main%20broken origin=fleet\n"),
            "the App lane is where `invoke` is actually refused"
        );
        assert!(app_halt_refusal(&store, "hover").is_none());
        for verb in [
            "post",
            "inbox",
            "inbox seen",
            "meta",
            "lease",
            "text",
            "status",
            "await",
            "subscribe",
            "spawn",
        ] {
            assert!(
                halt_refusal(&ctx, verb).is_none(),
                "{verb} must stay answerable under a halt"
            );
        }
        assert_eq!(bridge_hold(&store, &format!("{sid} off")), "OK hold=0\n");
        assert!(halt_refusal(&ctx, "turn").is_none(), "the halt lifted");
        assert_eq!(bridge_hold(&store, &format!("{sid} sideways")), HOLD_USAGE);
        assert_eq!(
            bridge_hold(&store, &format!("{sid} on origin=nowhere")),
            HOLD_USAGE
        );
        assert_eq!(bridge_hold(&store, "s-nope on"), "ERR no such session\n");
    }

    #[test]
    fn a_halt_reason_cannot_carry_anything_but_pct_ascii() {
        assert_eq!(reason_token(""), "-");
        assert_eq!(reason_token("   "), "-");
        assert_eq!(reason_token("main%20broken"), "main%20broken");
        assert_eq!(
            reason_token("ok\nERR forged"),
            "okERRforged",
            "a newline can never split one reply into two"
        );
        assert_eq!(reason_token("caf\u{e9} \u{202e}rap"), "cafrap");
        let long = "x".repeat(REASON_MAX + 50);
        assert_eq!(reason_token(&long).len(), REASON_MAX);
        let straddle = format!("{}%41", "y".repeat(REASON_MAX - 2));
        let out = reason_token(&straddle);
        assert!(!out.ends_with('%'), "{out}");
        assert_eq!(out.len(), REASON_MAX - 2, "{out}");
        assert_eq!(reason_token("100%sure"), "100%25sure");
        assert_eq!(reason_token("a%g"), "a%25g");
        assert_eq!(reason_token("a%2"), "a");
        assert_eq!(reason_token("a%"), "a");
        assert_eq!(reason_token("%%41%4"), "%25%41");
        let flood = reason_token(&"%".repeat(500));
        assert_eq!(flood, "%25".repeat(REASON_MAX / 3), "a flood is escapes");
        for raw in ["%", "a%", "a%4", "%zz%41", &"%".repeat(500), &straddle] {
            let out = reason_token(raw);
            let b = out.as_bytes();
            let escapes = b.iter().filter(|&&c| c == b'%').count();
            assert!(
                (0..b.len()).filter(|&i| b[i] == b'%').all(|i| {
                    b.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                        && b.get(i + 2).is_some_and(u8::is_ascii_hexdigit)
                }),
                "{raw:?}: a `%` that starts no escape in {out:?}"
            );
            assert!(
                out.len() <= REASON_MAX && escapes <= REASON_MAX / 3,
                "{out}"
            );
        }
    }

    #[test]
    fn losing_the_bridge_leaves_every_session_running_with_an_advisory() {
        with_link(|| {
            assert!(!holds_on_bridge_lost(), "the default halts nothing");
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let (held_sid, held_ctx) = registered_as(&store, 2);
            let untouched = test_handle(3);
            let untouched_ctx = untouched.ctx.clone();
            store
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .register(untouched);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            assert_eq!(deliver(&store, &sid, 7, "h-a", "task", "go"), "OK 1\n");
            assert_eq!(
                bridge_hold(&store, &format!("{held_sid} on reason=person")),
                "OK hold=1\n"
            );

            assert_eq!(bridge_lost(&store, generation), 2, "two governed sessions");
            assert_eq!(fabric_state(), "disconnected");
            assert!(halt_refusal(&ctx, "turn").is_none(), "nothing halted");
            assert!(ctx.fabric.hold().is_none());
            assert_eq!(
                ctx.fabric.hold_advisory().map(|h| (h.reason, h.origin)),
                Some(("fabric-lost".to_string(), "fleet".to_string()))
            );
            assert!(
                ctx.timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .since(None)
                    .any(|e| e.kind == "hold-advisory"),
                "recorded on the timeline"
            );
            assert_eq!(
                halt_refusal(&held_ctx, "turn").as_deref(),
                Some("ERR halted reason=person origin=fleet\n")
            );
            assert!(held_ctx.fabric.hold_advisory().is_none());
            assert!(halt_refusal(&untouched_ctx, "turn").is_none());
            assert!(untouched_ctx.fabric.hold_advisory().is_none());
            let next = next_bridge_generation();
            attach_up(&store, next);
            assert!(ctx.fabric.hold_advisory().is_none());
            assert!(halt_refusal(&ctx, "turn").is_none());
        });
    }

    #[test]
    fn a_bye_is_taken_only_from_the_bridge_that_owns_the_link() {
        with_link(|| {
            let store = new_store();
            let old = next_bridge_generation();
            bridge_attached(old);
            let live = next_bridge_generation();
            bridge_attached(live);
            let before = byes_taken();
            assert_eq!(cmd_link(&store, "bye quit", Some(old)), "OK stale=1\n");
            assert_eq!(cmd_link(&store, "bye quit", None), "OK stale=1\n");
            assert!(!await_bye_taken(before, std::time::Duration::ZERO));
            assert_eq!(cmd_link(&store, "bye handoff", Some(live)), "OK\n");
            assert!(await_bye_taken(before, std::time::Duration::ZERO));
            for bad in ["bye", "bye exit", "bye quit now"] {
                assert_eq!(cmd_link(&store, bad, Some(live)), LINK_USAGE, "{bad}");
            }
        });
    }

    #[test]
    fn the_link_is_the_bridges_to_report_and_stalled_refuses_a_wait_at_once() {
        with_link(|| {
            let store = new_store();
            let (_sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            bridge_attached(generation);
            assert_eq!(fabric_state(), "stalled");
            assert_eq!(
                fabric_link_facts(),
                (LINK_STARTING.to_string(), None, None),
                "before the first report: no ack, no age"
            );
            assert_eq!(fabric_status_tail(), "fabric_rtt_ms=- fabric_link_age_ms=-");

            assert_eq!(cmd_link(&store, "up rtt=3", Some(generation)), "OK\n");
            assert_eq!(fabric_state(), "connected");
            let (reason, rtt, age) = fabric_link_facts();
            assert_eq!((reason.as_str(), rtt), ("-", Some(3)));
            assert!(
                age.is_some_and(
                    |a| std::time::Duration::from_millis(a) < aterm_spec::wait::MUST_HAPPEN
                ),
                "a fresh ack: {age:?}"
            );
            assert!(
                fabric_status_tail().starts_with("fabric_rtt_ms=3 fabric_link_age_ms="),
                "{}",
                fabric_status_tail()
            );
            assert_eq!(
                cmd_post(&ctx, "to=h-andrew kind=ask --wait=0 up?", None),
                "ERR timeout id=1\n"
            );

            assert_eq!(
                cmd_link(&store, "down reason=refused", Some(generation)),
                "OK\n"
            );
            assert_eq!(fabric_state(), "stalled");
            let (reason, rtt, _) = fabric_link_facts();
            assert_eq!(
                (reason.as_str(), rtt),
                ("refused", Some(3)),
                "the reason is the bridge's word; the last rtt is kept as history"
            );
            let started = std::time::Instant::now();
            assert_eq!(
                cmd_post(&ctx, "to=h-andrew kind=ask --wait=600000 down?", None),
                "ERR fabric stalled id=2 queued=1\n"
            );
            assert!(
                started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
                "a stalled wait is refused at once, not after its timeout: {:?}",
                started.elapsed()
            );

            for bad in [
                "",
                "up",
                "up rtt=x",
                "down",
                "down rtt=1",
                "sideways",
                "up rtt=1 reason=x",
            ] {
                assert_eq!(
                    cmd_link(&store, bad, Some(generation)),
                    LINK_USAGE,
                    "{bad:?}"
                );
            }
            assert_eq!(
                cmd_link(&store, "down reason=NO%20ACK", Some(generation)),
                "OK\n"
            );
            assert_eq!(
                fabric_link_facts().0,
                "20",
                "a reason is lowercase ascii, digits and '-' only"
            );
            assert_eq!(
                cmd_link(&store, "down reason=!!!", Some(generation)),
                "OK\n"
            );
            assert_eq!(fabric_link_facts().0, "unknown");
        });
    }

    #[test]
    fn a_bridge_that_owes_an_answer_reads_stale_and_a_quiet_one_does_not() {
        with_link(|| {
            let store = new_store();
            let (_sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            bridge_attached(generation);
            assert_eq!(cmd_link(&store, "up rtt=3", Some(generation)), "OK\n");
            assert_eq!(fabric_state(), "connected");

            settle_work_owed(&store.as_store());
            backdate_work_owed(stale_after() * 100);
            assert_eq!(
                fabric_state(),
                "connected",
                "an idle bridge owes nothing: no work, no debt, no `stale`"
            );
            assert!(!wait_is_futile(), "a quiet link must still take a wait");

            assert_eq!(cmd_post(&ctx, "to=h-andrew kind=note one", None), "OK 1\n");
            assert_eq!(
                fabric_state(),
                "connected",
                "a fresh debt is not a stale one"
            );
            assert!(!wait_is_futile());

            backdate_work_owed(STALE_AFTER);
            assert_eq!(fabric_state(), "stale");
            assert!(wait_is_futile());
            let started = std::time::Instant::now();
            assert_eq!(
                cmd_post(&ctx, "to=h-andrew kind=ask --wait=600000 anyone?", None),
                "ERR fabric stale id=2 queued=1\n",
                "queued=1: the post IS in the outbox and the bridge drains it \
                 the moment it moves again — `stale` is `wait, do not re-post`"
            );
            assert!(
                started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
                "a stale wait is refused at once, not after its timeout: {:?}",
                started.elapsed()
            );

            assert_eq!(cmd_link(&store, "up rtt=4", Some(generation)), "OK\n");
            assert_eq!(fabric_state(), "connected");
            assert!(!wait_is_futile());

            assert_eq!(
                cmd_post(&ctx, "to=h-andrew kind=note three", None),
                "OK 3\n"
            );
            backdate_work_owed(stale_after());
            assert_eq!(fabric_state(), "stale");
            let next = next_bridge_generation();
            bridge_attached(next);
            assert_eq!(
                fabric_state(),
                "stalled",
                "an attach is `stalled`, not `stale`"
            );
            assert_eq!(cmd_link(&store, "up rtt=1", Some(next)), "OK\n");
            assert_eq!(
                fabric_state(),
                "connected",
                "the new incarnation has not been asked for anything yet"
            );
        });
    }

    #[test]
    fn a_landing_re_arms_the_debt_while_another_post_is_still_outstanding() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            bridge_attached(generation);
            assert_eq!(cmd_link(&store, "up rtt=3", Some(generation)), "OK\n");
            assert_eq!(fabric_state(), "connected");

            assert_eq!(cmd_post(&ctx, "to=h-andrew kind=note one", None), "OK 1\n");
            assert_eq!(cmd_post(&ctx, "to=h-andrew kind=note two", None), "OK 2\n");

            assert_eq!(cmd_outbox_sent(&store, &format!("{sid} 1 off=7")), "OK\n");
            assert_eq!(
                fabric_state(),
                "connected",
                "a fresh re-stamp is not a stale one"
            );
            backdate_work_owed(stale_after());
            assert_eq!(
                fabric_state(),
                "stale",
                "post 2 is owed and the bridge has said nothing since"
            );
            assert!(wait_is_futile());

            assert_eq!(cmd_outbox_sent(&store, &format!("{sid} 2 off=8")), "OK\n");
            assert_eq!(fabric_state(), "connected");
            backdate_work_owed(stale_after() * 100);
            assert_eq!(
                fabric_state(),
                "connected",
                "an empty outbox owes nothing, however long ago the last ack was"
            );

            assert_eq!(
                cmd_post(&ctx, "to=h-andrew kind=note three", None),
                "OK 3\n"
            );
            backdate_work_owed(stale_after());
            assert_eq!(fabric_state(), "stale");
            assert_eq!(
                cmd_outbox_sent(&store, &format!("{sid} 3 off=- reason=undeliverable")),
                "OK\n"
            );
            assert_eq!(fabric_state(), "connected");
        });
    }

    #[test]
    fn the_receipt_wait_wakes_at_the_stale_horizon_rather_than_at_its_bound() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            bridge_attached(generation);
            assert_eq!(cmd_link(&store, "up rtt=3", Some(generation)), "OK\n");

            assert_eq!(cmd_post(&ctx, "to=h-andrew kind=note one", None), "OK 1\n");
            assert_eq!(cmd_post(&ctx, "to=h-andrew kind=note two", None), "OK 2\n");
            assert_eq!(cmd_outbox_sent(&store, &format!("{sid} 1 off=7")), "OK\n");
            backdate_work_owed(stale_after() - std::time::Duration::from_millis(300));

            let started = std::time::Instant::now();
            assert_eq!(
                wait_receipt(&ctx, 1, 7, "", WAIT_MAX_MS),
                "ERR fabric stale id=1 off=7\n",
                "the post LANDED, so the reply names its offset and not `queued=1`"
            );
            assert!(
                started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
                "the receipt wait sat out its bound instead of waking at the \
                 stale horizon: {:?}",
                started.elapsed()
            );
        });
    }

    #[test]
    fn a_parked_fetch_wakes_at_the_stale_horizon_rather_than_at_its_bound() {
        with_link(|| {
            let store = new_store();
            let (_sid, ctx) = registered(&store);
            attach_up(&store, next_bridge_generation());
            assert_eq!(
                inbox_get_at_within(&ctx, 7, std::time::Duration::from_millis(50)),
                "ERR timeout off=7\n",
                "nothing owed: no debt can go stale inside the park"
            );

            assert_eq!(cmd_post(&ctx, "to=h-andrew kind=note one", None), "OK 1\n");
            backdate_work_owed(stale_after() - std::time::Duration::from_millis(300));
            let started = std::time::Instant::now();
            assert_eq!(
                inbox_get_at_within(&ctx, 7, std::time::Duration::from_millis(WAIT_MAX_MS)),
                "ERR fabric stale off=7\n"
            );
            assert!(
                started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
                "the fetch sat out its bound instead of waking at the stale \
                 horizon: {:?}",
                started.elapsed()
            );
        });
    }

    #[test]
    fn a_wait_parked_with_nothing_owed_answers_stale_when_a_later_debt_goes_stale() {
        use std::time::Duration;
        with_link(|| {
            // A fetch parked with nothing owed looks again a whole horizon later: shorten the
            // horizon so the test waits 300 ms of it, not the instance's 6 s.
            STALE_AFTER_IN_TEST.store(300, Ordering::Relaxed);
            let store = new_store();
            let (_sid, ctx) = registered(&store);
            let (_peer_sid, peer) = registered_as(&store, 2);
            attach_up(&store, next_bridge_generation());
            assert_eq!(
                inbox_get_at_within(&ctx, 7, Duration::from_millis(50)),
                "ERR timeout off=7\n",
                "no debt armed: the read ends at its bound"
            );

            let watcher = test_support::Subscription::single(&ctx.watchers);
            let started = std::time::Instant::now();
            let reader = {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    inbox_get_at_within(&ctx, 7, Duration::from_millis(WAIT_MAX_MS))
                })
            };
            assert!(
                watcher.wait(aterm_spec::wait::MUST_HAPPEN),
                "the parked fetch woke its watchers"
            );
            {
                let inbox = ctx.fabric.lock();
                assert!(
                    inbox
                        .fetches
                        .iter()
                        .any(|(o, slot)| *o == 7 && matches!(slot, FetchSlot::Pending)),
                    "the fetch released its guard, so it is parked"
                );
                assert_eq!(stale_in(), None, "and it parked with nothing owed");
            }

            assert_eq!(cmd_post(&peer, "to=h-andrew kind=note two", None), "OK 1\n");
            backdate_work_owed(stale_after());
            assert_eq!(fabric_state(), "stale");
            assert_eq!(
                reader.join().expect("the reader finished"),
                "ERR fabric stale off=7\n"
            );
            assert!(
                started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
                "the fetch sat out its bound instead of looking again by the \
                 horizon of the debt armed after it parked: {:?}",
                started.elapsed()
            );
        });
    }

    #[test]
    fn a_ghost_lanes_link_report_is_dropped_and_a_lost_bridge_stays_lost() {
        with_link(|| {
            let store = new_store();
            let (_sid, _ctx) = registered(&store);
            let first = next_bridge_generation();
            attach_up(&store, first);
            let second = next_bridge_generation();
            bridge_attached(second);
            assert_eq!(
                fabric_state(),
                "stalled",
                "the replacement has not reported yet"
            );
            assert_eq!(
                cmd_link(&store, "up rtt=1", Some(first)),
                "OK stale=1\n",
                "a ghost's report is dropped"
            );
            assert_eq!(fabric_state(), "stalled");
            assert_eq!(cmd_link(&store, "up rtt=1", None), "OK stale=1\n");
            let (other_sid, _) = registered_as(&store, 2);
            assert_eq!(
                deliver(
                    &store,
                    &other_sid,
                    1,
                    "s-peer",
                    "note",
                    "unscoped%20fixture"
                ),
                "OK 1\n"
            );
            assert_eq!(
                fabric_state(),
                "stalled",
                "an unrelated unscoped delivery cannot supply this bridge's link evidence"
            );
            assert_eq!(cmd_link(&store, "up rtt=2", Some(second)), "OK\n");
            assert_eq!(fabric_state(), "connected");

            assert_eq!(
                bridge_lost(&store, second),
                1,
                "the delivered fixture session is governed"
            );
            assert_eq!(fabric_state(), "disconnected");
            assert_eq!(fabric_link_facts().0, LINK_BRIDGE_LOST);
            assert_eq!(
                cmd_link(&store, "up rtt=2", Some(second)),
                "OK stale=1\n",
                "a report after the guard fired cannot resurrect the link"
            );
            assert_eq!(fabric_state(), "disconnected");

            assert_eq!(cmd_link(&store, "up rtt=2", None), "OK stale=1\n");
        });
    }

    fn link_generation_admission_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            FabricLinkGenerationAdmission {
                const Buggy = 0;
                var owner = 2;
                var actor = 0;
                var lost = 0;
                var accepted = 0;
                action PickCurrent when (actor == 0 && accepted == 0) { actor = 2; }
                action PickStale when (actor == 0 && accepted == 0) { actor = 1; }
                action Lose when (lost == 0) { lost = 1; accepted = 0; }
                action Connect when (accepted == 0 && lost == 0 && owner > 0 &&
                    (actor == owner || (Buggy == 1 && actor == 0))) { accepted = 1; }
                invariant ExplicitLiveOwnerOnly:
                    accepted == 0 || (actor == owner && actor > 0 && lost == 0);
            }
        }
    }

    #[test]
    fn link_reports_and_legacy_evidence_require_the_explicit_live_owner() {
        with_link(|| {
            let store = new_store();
            let model = link_generation_admission_model();
            for lost in [false, true] {
                for actor in 0..=2 {
                    for report in [false, true] {
                        let stale = next_bridge_generation();
                        let current = next_bridge_generation();
                        bridge_attached(current);
                        if lost {
                            bridge_lost(&store, current);
                        }
                        let generation = match actor {
                            0 => None,
                            1 => Some(stale),
                            _ => Some(current),
                        };
                        let mut state = model.init_state();
                        state.insert("actor", actor);
                        state.insert("lost", i64::from(lost));
                        let admitted = model.action_enabled("Connect", &state);
                        if report {
                            assert_eq!(
                                link_report(&store, generation, true, "", Some(2)),
                                admitted
                            );
                        } else {
                            link_evidence(generation);
                        }
                        assert_eq!(
                            fabric_state() == "connected",
                            admitted,
                            "report={report}, {state:?}"
                        );
                        if lost {
                            assert_eq!(fabric_state(), "disconnected");
                        }
                        if actor == 0 && !lost {
                            assert!(!generation.is_some_and(|g| g.0 != current.0));
                            state.insert("accepted", 1);
                            assert!(!model.check_invariant("ExplicitLiveOwnerOnly", &state));
                        }
                    }
                }
            }
        });
    }

    #[test]
    fn every_stored_link_transition_reaches_the_hook_once_in_order() {
        static HEARD: Mutex<Vec<LinkMove>> = Mutex::new(Vec::new());
        fn hear(m: LinkMove) {
            HEARD.lock().unwrap_or_else(|p| p.into_inner()).push(m);
        }
        on_link_moved(hear);
        with_link(|| {
            HEARD.lock().unwrap_or_else(|p| p.into_inner()).clear();
            let store = new_store();
            let g = next_bridge_generation();
            bridge_attached(g);
            assert_eq!(cmd_link(&store, "up rtt=3", Some(g)), "OK\n");
            assert_eq!(cmd_link(&store, "up rtt=4", Some(g)), "OK\n");
            assert_eq!(cmd_link(&store, "down reason=refused", Some(g)), "OK\n");
            assert_eq!(cmd_link(&store, "down reason=gone", Some(g)), "OK\n");
            let ghost = next_bridge_generation();
            assert!(!link_report(&store, Some(ghost), true, "", Some(1)));
            bridge_lost(&store, g);
            bridge_lost(&store, g);
            let quiet = next_bridge_generation();
            bridge_attached(quiet);
            link_evidence(Some(quiet));
            let heard: Vec<(&str, u64, LinkSaid)> = HEARD
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .iter()
                .filter(|m| [g.0, quiet.0].contains(&m.generation))
                .map(|m| (m.state, m.generation, m.said))
                .collect();
            assert_eq!(
                heard,
                [
                    ("stalled", g.0, LinkSaid::Attached),
                    ("connected", g.0, LinkSaid::Report),
                    ("stalled", g.0, LinkSaid::Report),
                    ("disconnected", g.0, LinkSaid::Lost),
                    ("stalled", quiet.0, LinkSaid::Attached),
                    ("connected", quiet.0, LinkSaid::Evidence),
                ]
            );
        });
    }

    #[test]
    fn review_an_old_bridge_that_delivers_mail_reads_connected() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            bridge_attached(generation);
            bridge_attached(generation);
            assert_eq!(fabric_state(), "stalled");
            assert_eq!(fabric_link_facts().0, LINK_STARTING);
            assert_eq!(
                cmd_post(&ctx, "to=h-andrew kind=ask --wait=0 up?", None),
                "ERR timeout id=1\n",
                "under `starting` the wait parks and runs its course; it is not refused"
            );
            let delivered = on_lane(generation, || {
                deliver(&store, &sid, 3, "s-peer", "note", "hi")
            });
            assert!(delivered.starts_with("OK"), "{delivered}");
            assert_eq!(fabric_state(), "connected");
            let (reason, rtt, age) = fabric_link_facts();
            assert_eq!(
                (reason.as_str(), rtt),
                ("-", None),
                "no round trip was ever reported; the delivery is the ack"
            );
            assert!(
                age.is_some_and(
                    |a| std::time::Duration::from_millis(a) < aterm_spec::wait::MUST_HAPPEN
                ),
                "{age:?}"
            );
            let tail = fabric_status_tail();
            assert!(
                tail.starts_with("fabric_rtt_ms=- fabric_link_age_ms=")
                    && !tail.ends_with("fabric_link_age_ms=-"),
                "{tail}"
            );

            assert_eq!(
                cmd_link(&store, "down reason=closed", Some(generation)),
                "OK\n"
            );
            assert_eq!(fabric_state(), "stalled");
            let delivered = on_lane(generation, || {
                deliver(&store, &sid, 4, "s-peer", "note", "again")
            });
            assert!(delivered.starts_with("OK"), "{delivered}");
            assert_eq!(fabric_state(), "stalled");
            assert_eq!(
                cmd_post(&ctx, "to=h-andrew kind=ask --wait=0 up?", None),
                "ERR fabric stalled id=2 queued=1\n"
            );
            let next = next_bridge_generation();
            bridge_attached(next);
            assert_eq!(fabric_state(), "stalled");
            assert_eq!(fabric_link_facts().0, LINK_STARTING);
            let delivered = on_lane(next, || deliver(&store, &sid, 5, "s-peer", "note", "third"));
            assert!(delivered.starts_with("OK"), "{delivered}");
            assert_eq!(fabric_state(), "connected");
        });
    }

    #[test]
    fn a_parked_waiter_is_woken_by_a_down_report_from_starting_or_up() {
        for was_up in [false, true] {
            with_link(|| {
                let store = new_store();
                let (_sid, ctx) = registered(&store);
                let generation = next_bridge_generation();
                if was_up {
                    attach_up(&store, generation);
                } else {
                    bridge_attached(generation);
                    assert_eq!(fabric_link_facts().0, LINK_STARTING);
                }
                let poster = {
                    let ctx = ctx.clone();
                    std::thread::spawn(move || {
                        cmd_post(
                            &ctx,
                            "to=@s-peer kind=ask --wait=600000 which branch?",
                            None,
                        )
                    })
                };
                for spin in 0.. {
                    assert!(spin < 5_000_000, "the post never reached the outbox");
                    if cmd_inbox(&ctx, "--peek").contains("post 1 ") {
                        break;
                    }
                    std::thread::yield_now();
                }
                assert_eq!(
                    cmd_link(&store, "down reason=no-socket", Some(generation)),
                    "OK\n"
                );
                assert_eq!(
                    poster.join().expect("the poster thread"),
                    "ERR fabric stalled id=1 queued=1\n",
                    "was_up={was_up}: a down report must wake a parked waiter"
                );
            });
        }
    }

    #[test]
    fn a_lost_bridges_advisory_is_one_fact() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            bridge_attached(generation);
            assert_eq!(bridge_hold(&store, &format!("{sid} off")), "OK hold=0\n");
            assert_eq!(bridge_lost(&store, generation), 1);
            let facts = ctx.timeline.lock().unwrap().facts.clone();
            let advised: Vec<_> = facts
                .iter()
                .filter(|f| f.starts_with("hold-advisory: "))
                .collect();
            assert_eq!(
                advised,
                [&format!(
                    "hold-advisory: Advisory {{ origin: \"fleet\", reason: \"fabric-lost\", \
                     generation: {} }}",
                    generation.0
                )],
                "{facts:?}"
            );
        });
    }

    #[test]
    fn a_session_the_bridge_only_unheld_is_still_governed() {
        with_link(|| {
            set_hold_on_bridge_lost(true);
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            bridge_attached(generation);
            assert_eq!(bridge_hold(&store, &format!("{sid} off")), "OK hold=0\n");
            assert_eq!(bridge_lost(&store, generation), 1);
            assert!(halt_refusal(&ctx, "turn").is_some());
        });
    }

    #[test]
    fn a_bridge_hold_over_a_local_one_stands_against_the_owner() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let hold = |reason: &str, origin: &str| Hold {
                reason: reason.to_string(),
                origin: origin.to_string(),
            };
            let standing = || ctx.fabric.holds().standing().collect::<Vec<_>>();
            assert_eq!(owner_hold(&store, &format!("{sid} off")), "OK hold=0\n");
            assert_eq!(
                owner_hold(&store, &format!("{sid} on reason=mine origin=local")),
                "OK hold=1\n"
            );

            let local = vec![hold("mine", "local")];
            let serial = ctx.fabric.hold_serial();
            for lift in [format!("{sid} off"), format!("{sid} off origin=fleet")] {
                assert_eq!(bridge_hold(&store, &lift), "OK hold=1\n", "{lift}");
                assert_eq!(standing(), local, "{lift}");
            }
            assert_eq!(ctx.fabric.hold_serial(), serial, "nothing moved");
            assert_eq!(
                halt_refusal(&ctx, "turn").as_deref(),
                Some("ERR halted reason=mine origin=local\n")
            );
            assert_eq!(
                bridge_hold(&store, &format!("{sid} off origin=local")),
                "OK hold=0\n"
            );
            assert!(halt_refusal(&ctx, "turn").is_none());
            assert_eq!(
                bridge_hold(&store, &format!("{sid} on reason=stop")),
                "OK hold=1\n"
            );
            assert_eq!(
                bridge_hold(&store, &format!("{sid} off origin=local")),
                "OK hold=1\n"
            );
            assert_eq!(
                halt_refusal(&ctx, "turn").as_deref(),
                Some("ERR halted reason=stop origin=fleet\n")
            );
            assert_eq!(bridge_hold(&store, &format!("{sid} off")), "OK hold=0\n");

            assert_eq!(
                owner_hold(&store, &format!("{sid} on reason=mine")),
                "OK hold=1\n"
            );
            assert_eq!(
                bridge_hold(&store, &format!("{sid} on reason=fabric-lost")),
                "OK hold=1\n"
            );
            assert_eq!(
                standing(),
                [hold("fabric-lost", "fleet"), hold("mine", "local")]
            );
            assert_eq!(owner_hold(&store, &format!("{sid} off")), "OK hold=1\n");
            assert_eq!(
                halt_refusal(&ctx, "turn").as_deref(),
                Some("ERR halted reason=fabric-lost origin=fleet\n")
            );
        });
    }

    fn single_slot(standing: Option<Hold>, act: &HoldAct<'_>) -> Option<Hold> {
        match act {
            HoldAct::Set(hold) => Some(hold.clone()),
            HoldAct::Lift(origin) => standing.filter(|hold| hold.origin != *origin),
            HoldAct::LiftAll => None,
            HoldAct::SetRemote(_) | HoldAct::LiftRemote(_) => standing,
        }
    }

    #[test]
    fn a_fleet_lift_leaves_the_owners_stop_standing() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let mine = Hold {
                reason: "mine".to_string(),
                origin: "local".to_string(),
            };
            let stop = Hold {
                reason: "stop".to_string(),
                origin: "fleet".to_string(),
            };
            let acts = [
                HoldAct::Set(mine.clone()),
                HoldAct::Set(stop.clone()),
                HoldAct::Lift("fleet"),
            ];
            assert_eq!(
                acts.iter().fold(None, single_slot),
                None,
                "NEGATIVE CONTROL: one slot loses the owner's stop to this sequence"
            );

            assert_eq!(
                owner_hold(&store, &format!("{sid} on reason=mine")),
                "OK hold=1\n"
            );
            assert_eq!(
                bridge_hold(&store, &format!("{sid} on reason=stop")),
                "OK hold=1\n"
            );
            assert_eq!(
                ctx.fabric.holds().standing().collect::<Vec<_>>(),
                [stop, mine.clone()],
                "both stand"
            );
            assert_eq!(
                ctx.fabric.holds().origins(),
                "fleet,local",
                "`hold_origin=` names both"
            );
            assert_eq!(
                bridge_hold(&store, &format!("{sid} off")),
                "OK hold=1\n",
                "the fleet's lift answers that the session is still held"
            );

            assert_eq!(ctx.fabric.hold(), Some(mine), "the owner's stop stands");
            assert_eq!(ctx.fabric.holds().origins(), "local");
            assert_eq!(
                halt_refusal(&ctx, "send").as_deref(),
                Some("ERR halted reason=mine origin=local\n")
            );
            assert!(cmd_inbox(&ctx, "--peek").starts_with("OK 0 hold=1 "));
            let events: Vec<String> = ctx
                .timeline
                .lock()
                .unwrap()
                .since(None)
                .filter(|e| e.kind == "hold")
                .map(|e| e.payload.to_string())
                .collect();
            assert_eq!(
                events,
                [
                    "1 reason=mine origin=local hold_origin=local",
                    "1 reason=stop origin=fleet hold_origin=fleet,local",
                    "1 reason=mine origin=local hold_origin=local",
                ],
                "every event says what stands after it"
            );
            assert_eq!(owner_hold(&store, &format!("{sid} off")), "OK hold=0\n");
            assert!(halt_refusal(&ctx, "send").is_none());
            let last = ctx
                .timeline
                .lock()
                .unwrap()
                .since(None)
                .rfind(|e| e.kind == "hold")
                .map(|e| e.payload.clone());
            assert_eq!(
                last.as_deref(),
                Some("0 reason=- origin=local hold_origin=-")
            );
        });
    }

    #[test]
    fn a_reconnect_after_the_sweep_leaves_the_owners_stop_standing() {
        with_link(|| {
            set_hold_on_bridge_lost(true);
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let other = test_handle(2);
            let (other_sid, other_ctx) = (other.sid.as_str().to_string(), other.ctx.clone());
            store
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .register(other);
            let generation = next_bridge_generation();
            bridge_attached(generation);
            for s in [&sid, &other_sid] {
                assert_eq!(bridge_hold(&store, &format!("{s} off")), "OK hold=0\n");
            }
            assert_eq!(
                owner_hold(&store, &format!("{sid} on reason=mine")),
                "OK hold=1\n"
            );

            assert_eq!(bridge_lost(&store, generation), 2, "two governed sessions");
            assert_eq!(ctx.fabric.holds().origins(), "fleet,local");
            assert_eq!(
                halt_refusal(&ctx, "send").as_deref(),
                Some("ERR halted reason=fabric-lost origin=fleet\n")
            );
            assert_eq!(other_ctx.fabric.holds().origins(), "fleet");

            let next = next_bridge_generation();
            bridge_attached(next);
            assert_eq!(bridge_hold(&store, &format!("{sid} off")), "OK hold=1\n");
            assert_eq!(
                bridge_hold(&store, &format!("{other_sid} off")),
                "OK hold=0\n"
            );
            assert_eq!(
                halt_refusal(&ctx, "send").as_deref(),
                Some("ERR halted reason=mine origin=local\n"),
                "the owner's stop survived the sweep and its lift"
            );
            assert!(
                halt_refusal(&other_ctx, "send").is_none(),
                "NEGATIVE CONTROL: the same lift frees a session with no hold of the owner's"
            );
        });
    }

    #[test]
    fn await_inbox_latches_forward_only_and_ignores_notes_by_default() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert_eq!(deliver(&store, &sid, 1, "h-a", "task", "old"), "OK 1\n");
        assert_eq!(cmd_await_inbox(&ctx, &["since=1"], 5), "OK timeout\n");
        assert_eq!(deliver(&store, &sid, 2, "h-a", "note", "fyi"), "OK 2\n");
        assert_eq!(
            cmd_await_inbox(&ctx, &["since=1"], 5),
            "OK timeout\n",
            "a note does not wake an agent by itself"
        );
        assert_eq!(
            cmd_await_inbox(&ctx, &["since=1", "kinds=note"], 5),
            "OK inbox 2\n",
            "unless the caller asked for notes"
        );
        assert_eq!(deliver(&store, &sid, 3, "h-a", "ask", "?"), "OK 3\n");
        assert_eq!(cmd_await_inbox(&ctx, &["since=1"], 5), "OK inbox 3\n");
        assert_eq!(
            cmd_await_inbox(&ctx, &["since=3"], 5),
            "OK timeout\n",
            "monotone: the row it already latched on cannot latch it again"
        );
        assert_eq!(
            cmd_await_inbox(&ctx, &["since=1", "kinds=answer"], 5),
            "OK timeout\n"
        );
        assert_eq!(
            bridge_hold(&store, &format!("{sid} on reason=x")),
            "OK hold=1\n"
        );
        assert_eq!(
            cmd_await_inbox(&ctx, &["since=3", "kinds=hold"], 5),
            "OK timeout\n",
            "a halt already in force is not news"
        );
        let waiter = {
            let ctx = ctx.clone();
            std::thread::spawn(move || cmd_await_inbox(&ctx, &["since=3", "kinds=hold"], 60_000))
        };
        let mut on = true;
        while !waiter.is_finished() {
            let arg = if on { "on reason=x" } else { "off" };
            bridge_hold(&store, &format!("{sid} {arg}"));
            on = !on;
            std::thread::yield_now();
        }
        let latched = waiter.join().expect("the waiter finished");
        assert!(
            latched == "OK inbox hold=1\n" || latched == "OK inbox hold=0\n",
            "a hold TRANSITION latches the wait, not the timeout: {latched}"
        );
        bridge_hold(&store, &format!("{sid} off"));
        assert_eq!(
            cmd_await_inbox(&ctx, &["since=3", "kinds=task"], 5),
            "OK timeout\n"
        );
        assert!(cmd_await_inbox(&ctx, &[], 5).starts_with("ERR usage"));
        assert!(cmd_await_inbox(&ctx, &["since=1", "kinds=nope"], 5).starts_with("ERR usage"));
    }

    #[test]
    fn await_inbox_topic_latches_only_on_that_topics_broadcasts() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        for topic in ["news", "lab"] {
            assert_eq!(ctx.fabric.topic_add(topic, "head", &ctx.timeline), Ok(true));
        }
        let shout = |off: u64, topic: &str| {
            cmd_deliver(
                &store,
                &format!(
                    "{sid} off={off} from=s-peer@n-lab kind=note trust=agent topic={topic} \
                     text=shout"
                ),
            )
        };
        assert_eq!(deliver(&store, &sid, 1, "h-a", "task", "go"), "OK 1\n");
        assert_eq!(shout(2, "news"), "OK 2\n");
        assert_eq!(shout(3, "lab"), "OK 3\n");
        assert_eq!(
            cmd_await_inbox(&ctx, &["since=0", "topic=lab"], 5),
            "OK inbox 3\n",
            "a note on the topic latches with no kinds="
        );
        assert_eq!(
            cmd_await_inbox(&ctx, &["since=0", "topic=news"], 5),
            "OK inbox 2\n",
            "the newest row on THAT topic, not the newest row"
        );
        assert_eq!(
            cmd_await_inbox(&ctx, &["since=0", "topic=lab", "kinds=task,ask"], 5),
            "OK timeout\n",
            "kinds= still narrows"
        );
        assert_eq!(
            cmd_await_inbox(&ctx, &["since=0", "topic=lab", "kinds=note"], 5),
            "OK inbox 3\n"
        );
        assert_eq!(deliver(&store, &sid, 4, "h-a", "task", "more"), "OK 4\n");
        assert_eq!(deliver(&store, &sid, 5, "h-a", "note", "fyi"), "OK 5\n");
        assert_eq!(shout(6, "news"), "OK 6\n");
        assert_eq!(
            cmd_await_inbox(&ctx, &["since=3", "topic=lab"], 5),
            "OK timeout\n",
            "an addressed row and another topic's broadcast do not latch it"
        );
        assert_eq!(cmd_await_inbox(&ctx, &["since=3"], 5), "OK inbox 4\n");
        for bad in ["topic=", "topic=Lab", "topic=a/b", "topic=-x"] {
            assert!(
                cmd_await_inbox(&ctx, &["since=0", bad], 5).starts_with("ERR usage"),
                "{bad}"
            );
        }
        assert!(
            cmd_await_inbox(&ctx, &["topic=lab"], 5).starts_with("ERR usage"),
            "a topic is not a one-off: since= is still required"
        );
    }

    #[test]
    fn an_accepted_post_wakes_before_its_wait_parks_and_a_refused_one_never() {
        use std::time::Duration;
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            attach_up(&store, next_bridge_generation());
            let watcher = test_support::Subscription::single(&ctx.watchers);
            assert!(cmd_post(&ctx, "to=@s-b kind=shout hi", None).starts_with("ERR usage"));
            assert!(!watcher.wait(Duration::ZERO), "a refused post wakes nobody");
            assert_eq!(cmd_post(&ctx, "to=h-andrew kind=note hi", None), "OK 1\n");
            assert!(watcher.wait(Duration::ZERO), "an accepted post wakes");

            let poster = {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    cmd_post(&ctx, "to=h-andrew kind=task --wait=30000 ship it", None)
                })
            };
            assert!(
                watcher.wait(stale_after() / 2),
                "the wake arrives while the post's wait is parked"
            );
            assert!(
                !poster.is_finished(),
                "nothing has landed, so the post is still parked in its wait"
            );
            assert_eq!(
                cmd_deliver(&store, &format!("{sid} landed=2 off=42")),
                "OK\n"
            );
            assert_eq!(poster.join().expect("the poster finished"), "OK 2 off=42\n");
        });
    }

    #[test]
    fn inbox_seen_reports_the_addressed_floor_and_no_broadcast_moves_it() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert_eq!(ctx.fabric.topic_add("t", "head", &ctx.timeline), Ok(true));
        let last_seen = || {
            ctx.timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .since(None)
                .rfind(|e| e.kind == "inbox-seen")
                .map(|e| e.payload.to_string())
                .unwrap_or_default()
        };
        assert_eq!(broadcast(&store, &sid, 1000, "t", "b0"), "OK 1\n");
        assert_eq!(cmd_inbox_seen(&ctx, "1"), "OK seen=1\n");
        assert_eq!(
            last_seen(),
            "1 off=1000 floor=- seen=1",
            "no addressed row yet: no floor to say"
        );

        assert_eq!(broadcast(&store, &sid, 1100, "t", "b1"), "OK 2\n");
        assert_eq!(deliver(&store, &sid, 900, "h-a", "task", "a1"), "OK 3\n");
        assert_eq!(cmd_inbox_seen(&ctx, "2"), "OK seen=2\n");
        assert_eq!(
            last_seen(),
            "2 off=1100 floor=900 seen=2",
            "the unhandled task holds the floor below the broadcast's offset"
        );
        assert_eq!(cmd_inbox_seen(&ctx, "3 handled"), "OK seen=3\n");
        assert_eq!(
            last_seen(),
            "3 off=900 floor=901 seen=3 verdict=handled kind=task from=h-a"
        );

        assert_eq!(deliver(&store, &sid, 2000, "h-a", "task", "a2"), "OK 4\n");
        assert_eq!(broadcast(&store, &sid, 1200, "t", "b2"), "OK 5\n");
        assert_eq!(cmd_inbox_seen(&ctx, "5"), "OK seen=5\n");
        assert_eq!(
            last_seen(),
            "5 off=1200 floor=2001 seen=5",
            "the handled task at 2000 is behind the floor, whatever the broadcast's offset"
        );
    }

    #[test]
    fn deliver_wakes_a_mail_watch() {
        use std::time::Duration;
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let watcher = test_support::Subscription::single(&ctx.watchers);
        assert_eq!(
            deliver(&store, &sid, 10, "h-andrew", "task", "one"),
            "OK 1\n"
        );
        assert!(watcher.wait(Duration::ZERO), "the delivery woke the watch");
        assert_eq!(
            deliver(&store, &sid, 10, "h-andrew", "task", "one"),
            "OK 1\n"
        );
        assert!(!watcher.wait(Duration::ZERO), "a redelivery wakes nobody");
    }

    #[test]
    fn inbox_seen_wakes_its_watchers_with_its_row_and_a_refused_form_never() {
        use std::time::Duration;
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert_eq!(
            deliver(&store, &sid, 10, "h-andrew", "task", "one"),
            "OK 1\n"
        );
        let watcher = test_support::Subscription::single(&ctx.watchers);
        let last_kind = || {
            ctx.timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .since(None)
                .next_back()
                .map(|e| e.kind)
        };
        for (form, reply) in [
            ("seen 9", "ERR no such message\n".to_string()),
            ("seen x", INBOX_USAGE.to_string()),
            ("seen 1 later", INBOX_USAGE.to_string()),
            ("get 1", "OK 3\none".to_string()),
            ("--peek", cmd_inbox(&ctx, "--peek")),
        ] {
            assert_eq!(cmd_inbox_verb(&ctx, form), reply, "{form}");
        }
        assert!(
            !watcher.wait(Duration::ZERO),
            "nothing recorded, nobody woken"
        );
        assert_eq!(cmd_inbox_verb(&ctx, "seen 1"), "OK seen=1\n");
        assert!(
            watcher.wait(Duration::ZERO),
            "a bare seen moves the floor: woken"
        );
        assert_eq!(last_kind(), Some("inbox-seen"), "by the row it announces");
        assert_eq!(cmd_inbox_verb(&ctx, "seen 1 handled"), "OK seen=1\n");
        assert!(
            watcher.wait(Duration::ZERO),
            "a verdict owes a receipt: woken"
        );
    }

    #[test]
    fn a_parked_fetch_wakes_its_watchers_before_it_parks_and_a_ring_read_never() {
        use std::time::Duration;
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            attach_up(&store, next_bridge_generation());
            assert_eq!(
                deliver(&store, &sid, 10, "h-andrew", "task", "one"),
                "OK 1\n"
            );
            let watcher = test_support::Subscription::single(&ctx.watchers);
            assert_eq!(
                cmd_inbox_verb(&ctx, "get @10"),
                "OK 3 off=10 from=h-andrew kind=task trust=agent\none"
            );
            assert!(!watcher.wait(Duration::ZERO), "a ring read woke the bridge");

            let reader = {
                let ctx = ctx.clone();
                std::thread::spawn(move || cmd_inbox_verb(&ctx, "get @7"))
            };
            assert!(
                watcher.wait(aterm_spec::wait::MUST_HAPPEN),
                "a parked fetch woke its watchers"
            );
            assert!(
                cmd_outbox(&store, "").contains(&format!("fetch sid={sid} off=7\n")),
                "woken with the fetch on the outbox peek"
            );
            assert_eq!(
                cmd_deliver(&store, &format!("{sid} fetched=7 err=no-record")),
                "OK\n"
            );
            assert_eq!(
                reader.join().expect("the reader finished"),
                "ERR no such record off=7\n",
                "the wake came before the park, so the park took the answer"
            );
        });
    }

    #[test]
    fn post_key_is_bounded_rides_the_outbox_and_a_dup_retirement_reaches_the_wait() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            assert!(valid_key("k-1") && valid_key("0123:abcd:99") && valid_key("a.b_c"));
            assert!(valid_key(&"k".repeat(KEY_MAX)));
            assert!(!valid_key(""));
            assert!(!valid_key(&"k".repeat(KEY_MAX + 1)));
            assert!(!valid_key("k/1") && !valid_key("k@1") && !valid_key("k=1"));
            assert!(!valid_key("kä"), "not ASCII");
            for bad in ["key= x", "key=k/1 x", "key=a@b x"] {
                assert_eq!(
                    cmd_post(&ctx, &format!("to=@s-b kind=note {bad}"), None),
                    POST_USAGE,
                    "{bad}"
                );
            }
            assert_eq!(
                cmd_post(
                    &ctx,
                    &format!("to=@s-b kind=note key={} x", "k".repeat(KEY_MAX + 1)),
                    None
                ),
                POST_USAGE
            );

            assert_eq!(
                cmd_post(&ctx, "to=@s-b kind=note key=k-1 hi", None),
                "OK 1\n"
            );
            let (_, posts) = parse_outbox(&cmd_outbox(&store, ""));
            assert_eq!(posts.len(), 1);
            assert!(
                posts[0].0.ends_with(" key=k-1 len=2"),
                "the key rides the outbox line, before len=: {:?}",
                posts[0].0
            );
            let (_, again) = parse_outbox(&cmd_outbox(&store, ""));
            assert_eq!(
                again, posts,
                "a peek: the key is there on the next drain too"
            );
            assert!(
                cmd_inbox(&ctx, "--peek").contains("post 1 to=@s-b kind=note off=- len=2 key=k-1"),
                "{}",
                cmd_inbox(&ctx, "--peek")
            );
            assert_eq!(cmd_post(&ctx, "to=@s-b kind=note plain", None), "OK 2\n");
            let (_, both) = parse_outbox(&cmd_outbox(&store, ""));
            assert!(!both[1].0.contains("key="), "{:?}", both[1].0);

            assert_eq!(
                cmd_outbox_sent(&store, &format!("{sid} 1 off=- dup=1")),
                OUTBOX_USAGE
            );
            assert_eq!(
                cmd_outbox_sent(&store, &format!("{sid} 1 off=42 dup=1")),
                "OK\n"
            );
            {
                let inbox = ctx.fabric.lock();
                let row = inbox.posts.iter().find(|p| p.id == 1).expect("post 1 kept");
                assert_eq!((row.off, row.dup), (Some(42), true));
                assert_eq!(row.key, None, "the key goes with the body at retirement");
            }
            assert_eq!(cmd_outbox_sent(&store, &format!("{sid} 1 off=42")), "OK\n");
            assert!(ctx.fabric.lock().posts.iter().any(|p| p.id == 1 && p.dup));

            attach_up(&store, next_bridge_generation());
            let poster = {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    cmd_post(
                        &ctx,
                        "to=h-andrew kind=task key=k-2 --wait=30000 ship it",
                        None,
                    )
                })
            };
            loop {
                let reply = cmd_outbox_sent(&store, &format!("{sid} 3 off=42 dup=1"));
                if reply == "OK\n" {
                    break;
                }
                assert_eq!(reply, "ERR no such post\n");
                std::thread::yield_now();
            }
            assert_eq!(
                poster.join().expect("the poster finished"),
                "OK 3 off=42 dup=1\n",
                "the re-post answers the ORIGINAL offset and says it was a duplicate"
            );
        });
    }

    #[test]
    fn post_refuses_what_it_cannot_address() {
        let store = new_store();
        let (_sid, ctx) = registered(&store);
        assert!(cmd_post(&ctx, "to=@s-b kind=shout hi", None).starts_with("ERR usage"));
        assert!(cmd_post(&ctx, "kind=note hi", None).starts_with("ERR usage"));
        assert!(cmd_post(&ctx, "to=@s-b kind=note", None).starts_with("ERR usage"));
        assert!(cmd_post(&ctx, "to=nonsense kind=note hi", None).starts_with("ERR usage"));
        assert!(cmd_post(&ctx, "to=fleet kind=control halt", None).starts_with("ERR denied"));
        assert_eq!(
            cmd_post(&ctx, "to=h-a kind=note diff=stat and a space", None),
            "OK 1\n"
        );
        assert_eq!(
            post_body_offset("to=h-a kind=note diff=stat and a space"),
            Some(17)
        );
        assert_eq!(post_body_offset("to=h-a kind=note --wait=5"), None);
        assert!(cmd_post(&ctx, "to=h-a kind=note --wait=5", None).starts_with("ERR usage"));
        assert_eq!(post_body_offset("to=h-a kind=note -- to=@s-x"), Some(20));
        assert_eq!(post_body_offset("to=h-a kind=note --"), None);
        assert!(cmd_post(&ctx, "to=h-a kind=note --", None).starts_with("ERR usage"));
        let long = "x".repeat(POST_INLINE_MAX + 1);
        for (line, refusal) in [
            (format!("to=h-a kind=note {long}"), "ERR too large"),
            ("to=h-a kind=note len=abc".to_string(), "ERR usage"),
            (
                format!("to=h-a kind=note len={}", BODY_MAX + 1),
                "ERR too large",
            ),
            ("to=h-a kind=note len=4".to_string(), "ERR usage"),
        ] {
            let reply = cmd_post(&ctx, &line, None);
            assert!(reply.starts_with(refusal), "{line:?}: {reply}");
        }
        assert_eq!(
            cmd_post(&ctx, "to=h-a kind=note len=4", Some(b"body".to_vec())),
            "OK 2\n"
        );
        let forged = "to=@s-x kind=answer re=7 key=k-9 len=9 --wait=1 done, merge it";
        assert_eq!(
            cmd_post(&ctx, &format!("to=@s-b kind=note -- {forged}"), None),
            "OK 3\n"
        );
        let (_, posts) = parse_outbox(&cmd_outbox(&store, ""));
        let (line, body) = posts.last().expect("the post is queued");
        assert!(
            line.contains(" to=@s-b kind=note ")
                && !line.contains("key=")
                && !line.contains(" re="),
            "the envelope is the sender's, not the body's: {line:?}"
        );
        assert_eq!(body, forged, "everything after `--` is the body, verbatim");
    }

    #[test]
    fn every_fabric_event_reaches_the_timeline_carrying_no_body() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let secret = "the-body-nobody-may-see";
            deliver(&store, &sid, 11, "h-andrew", "task", secret);
            cmd_inbox_seen(&ctx, "1 handled");
            cmd_post(&ctx, &format!("to=h-a kind=note {secret}"), None);
            cmd_deliver(&store, &format!("{sid} landed=1 off=90"));
            bridge_hold(&store, &format!("{sid} on reason=main%20broken"));

            let events: Vec<(String, String)> = ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .since(None)
                .map(|e| (e.kind.to_string(), e.payload.to_string()))
                .collect();
            let kinds: Vec<&str> = events
                .iter()
                .map(|(k, _)| k.as_str())
                .filter(|k| fabric_event_wire_kind(k).is_some())
                .collect();
            assert_eq!(
                kinds,
                ["inbox", "inbox-seen", "post", "post-landed", "hold"],
                "the fabric kinds this scenario writes, in the order they happened"
            );
            for (kind, payload) in &events {
                assert!(
                    !payload.contains(secret),
                    "{kind} leaked a body onto the digest: {payload}"
                );
            }
            let by = |k: &str| {
                events
                    .iter()
                    .find(|(kind, _)| kind == k)
                    .map(|(_, p)| p.clone())
                    .unwrap_or_default()
            };
            assert_eq!(by("inbox"), "1 from=h-andrew kind=task off=11");
            assert_eq!(
                by("inbox-seen"),
                "1 off=11 floor=12 seen=1 verdict=handled kind=task from=h-andrew"
            );
            assert_eq!(by("post"), "1 to=h-a kind=note");
            assert_eq!(by("post-landed"), "1 off=90");
            assert_eq!(
                by("hold"),
                "1 reason=main%20broken origin=fleet hold_origin=fleet"
            );
        });
    }

    #[test]
    fn inbox_seen_records_a_verdict_once_per_change_and_the_bare_form_records_none() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            deliver(&store, &sid, 10, "h-andrew", "task", "one");
            deliver(&store, &sid, 11, "s-peer@n-lab", "ask", "two");
            deliver(&store, &sid, 12, "h-andrew", "note", "three");
            cmd_inbox(&ctx, "--peek");
            assert_eq!(cmd_inbox_seen(&ctx, "2"), "OK seen=2\n");
            assert_eq!(cmd_inbox_seen(&ctx, "3 handled"), "OK seen=3\n");
            assert_eq!(cmd_inbox_seen(&ctx, "3 handled"), "OK seen=3\n");
            assert_eq!(cmd_inbox_seen(&ctx, "2 deferred"), "OK seen=3\n");
            assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=3\n");
            assert_eq!(cmd_inbox_seen(&ctx, "2 later"), INBOX_USAGE);
            let seen: Vec<String> = ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .since(None)
                .filter(|e| e.kind == "inbox-seen")
                .map(|e| e.payload.to_string())
                .collect();
            assert_eq!(
                seen,
                [
                    "2 off=11 floor=12 seen=2",
                    "3 off=12 floor=13 seen=3 verdict=handled kind=note from=h-andrew",
                    "3 off=12 floor=13 seen=3",
                    "2 off=11 floor=13 seen=3 verdict=deferred kind=ask from=s-peer@n-lab",
                    "2 off=11 floor=13 seen=3 verdict=handled kind=ask from=s-peer@n-lab",
                ],
                "one verdict per row per CHANGE; the bare form and a repeat carry none"
            );
        });
    }

    #[test]
    fn an_ack_row_carries_its_verdict_and_await_inbox_re_latches_on_it() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note do it", None), "OK 1\n");
            assert_eq!(
                cmd_deliver(&store, &format!("{sid} landed=1 off=70")),
                "OK\n"
            );
            assert_eq!(
                cmd_deliver(
                    &store,
                    &format!("{sid} off=80 from=s-peer@n-lab kind=ack re=70 verdict=maybe text=x")
                ),
                DELIVER_USAGE
            );
            let waiter = {
                let ctx = ctx.clone();
                std::thread::spawn(move || cmd_await_inbox(&ctx, &["re=70"], 60_000))
            };
            let other = {
                let ctx = ctx.clone();
                std::thread::spawn(move || cmd_await_inbox(&ctx, &["re=71", "since=0"], 200))
            };
            assert_eq!(
                cmd_deliver(
                    &store,
                    &format!(
                        "{sid} off=81 from=s-peer@n-lab kind=ack re=70 verdict=refused \
                         text=ack%20re%3D70%20verdict%3Drefused"
                    )
                ),
                "OK 1\n"
            );
            assert_eq!(waiter.join().expect("the waiter finished"), "OK inbox 1\n");
            assert_eq!(other.join().expect("the other finished"), "OK timeout\n");
            let listing = cmd_inbox(&ctx, "--peek --meta");
            let row = listing.lines().nth(1).expect("the ack row");
            assert!(
                row.starts_with("msg 1 off=81 ")
                    && row.contains(" kind=ack trust=agent re=70 re-id=1 verdict=refused "),
                "{row}"
            );
            assert_eq!(
                cmd_await_inbox(&ctx, &["re=70", "since=1"], 100),
                "OK timeout\n"
            );
            assert!(cmd_await_inbox(&ctx, &["kinds=ack"], 100).starts_with("ERR usage"));
        });
    }

    #[test]
    fn post_wait_ack_returns_the_verdict_or_the_expiry_and_is_bounded_by_dl() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            attach_up(&store, next_bridge_generation());
            assert_eq!(
                cmd_post(&ctx, "to=@s-peer kind=note --wait-ack hello", None),
                POST_USAGE
            );
            let poster = {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    cmd_post(&ctx, "to=@s-peer kind=task --wait-ack=30000 ship it", None)
                })
            };
            loop {
                if cmd_deliver(&store, &format!("{sid} landed=1 off=42")) == "OK\n" {
                    break;
                }
                std::thread::yield_now();
            }
            assert_eq!(
                cmd_deliver(
                    &store,
                    &format!(
                        "{sid} off=50 from=s-peer@n-lab kind=ack re=42 verdict=handled text=ok"
                    )
                ),
                "OK 1\n"
            );
            assert_eq!(
                poster.join().expect("the poster finished"),
                "OK 1 off=42 ack=handled msg=1\n"
            );
            let poster = {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    cmd_post(&ctx, "to=@s-peer kind=ask dl=30000 --wait-ack why?", None)
                })
            };
            loop {
                if cmd_deliver(&store, &format!("{sid} landed=2 off=43")) == "OK\n" {
                    break;
                }
                std::thread::yield_now();
            }
            assert_eq!(
                cmd_deliver(
                    &store,
                    &format!("{sid} off=51 from=n-lab kind=expired re=43 dl=30000 text=expired")
                ),
                "OK 2\n"
            );
            assert_eq!(
                poster.join().expect("the poster finished"),
                "ERR expired id=2 off=43\n"
            );
            const TEST_GRACE_MS: u64 = 1;
            let poster = {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    TEST_EXPIRY_GRACE_MS.with(|grace| grace.set(Some(TEST_GRACE_MS)));
                    cmd_post(&ctx, "to=@s-peer kind=task dl=150 --wait-ack quick", None)
                })
            };
            loop {
                if cmd_deliver(&store, &format!("{sid} landed=3 off=44")) == "OK\n" {
                    break;
                }
                std::thread::yield_now();
            }
            assert_eq!(
                poster.join().expect("the poster finished"),
                "ERR timeout id=3 off=44\n"
            );
            let poster = {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    cmd_post(&ctx, "to=@s-peer kind=ask dl=100 --wait-ack late?", None)
                })
            };
            loop {
                if cmd_deliver(&store, &format!("{sid} landed=4 off=45")) == "OK\n" {
                    break;
                }
                std::thread::yield_now();
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            assert_eq!(
                cmd_deliver(
                    &store,
                    &format!("{sid} off=52 from=n-lab kind=expired re=45 dl=100 text=expired")
                ),
                "OK 3\n"
            );
            assert_eq!(
                poster.join().expect("the poster finished"),
                "ERR expired id=4 off=45\n"
            );
            let poster = {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    TEST_EXPIRY_GRACE_MS.with(|grace| grace.set(Some(WAIT_MAX_MS)));
                    cmd_post(
                        &ctx,
                        "to=@s-peer kind=ask dl=100 --wait-ack=200 exact",
                        None,
                    )
                })
            };
            loop {
                if cmd_deliver(&store, &format!("{sid} landed=5 off=46")) == "OK\n" {
                    break;
                }
                std::thread::yield_now();
            }
            let started = std::time::Instant::now();
            assert_eq!(
                poster.join().expect("the poster finished"),
                "ERR timeout id=5 off=46\n"
            );
            assert!(
                started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
                "an explicit bound gets no grace: {:?}",
                started.elapsed()
            );
        });
    }

    #[test]
    fn a_decided_ask_owes_a_receipt_on_the_outbox_until_the_bridge_retires_it() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            deliver(&store, &sid, 10, "h-andrew", "task", "one");
            deliver(&store, &sid, 11, "s-peer@n-lab", "ask", "two");
            deliver(&store, &sid, 12, "s-peer@n-lab", "note", "three");
            let receipts = |store: &Store| -> Vec<String> {
                let reply = cmd_outbox(store, "");
                let (_, mut rest) = reply.split_once('\n').expect("a header");
                let mut out = Vec::new();
                while let Some((line, tail)) = rest.split_once('\n') {
                    rest = tail;
                    if line.starts_with("receipt ") {
                        out.push(line.to_string());
                    } else if let Some((_, n)) = line.rsplit_once(" len=") {
                        let n: usize = n.parse().expect("a post's len=");
                        rest = &rest[n..];
                    }
                }
                out
            };
            assert!(receipts(&store).is_empty(), "nothing decided, nothing owed");
            assert_eq!(cmd_inbox_seen(&ctx, "3"), "OK seen=3\n");
            assert_eq!(cmd_inbox_seen(&ctx, "3 handled"), "OK seen=3\n");
            assert!(
                receipts(&store).is_empty(),
                "a bare seen and a note owe nothing"
            );
            assert_eq!(cmd_inbox_seen(&ctx, "2 deferred"), "OK seen=3\n");
            assert_eq!(cmd_inbox_seen(&ctx, "2 deferred"), "OK seen=3\n");
            assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=3\n");
            assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=3\n");
            let owed = receipts(&store);
            assert_eq!(
                owed,
                [
                    format!(
                        "receipt sid={sid} rid=1 off=11 verdict=deferred kind=ask from=s-peer@n-lab"
                    ),
                    format!(
                        "receipt sid={sid} rid=2 off=10 verdict=handled kind=task from=h-andrew"
                    ),
                    format!(
                        "receipt sid={sid} rid=3 off=11 verdict=handled kind=ask from=s-peer@n-lab"
                    ),
                ],
                "one per decision, in order; the repeated word owes nothing"
            );
            assert_eq!(
                receipts(&store),
                owed,
                "a PEEK: the next drain lists them again"
            );
            for bad in [
                "receipt=x off=5",
                "receipt=1",
                "receipt=1 off=x",
                "receipt=1 off=5 more=1",
            ] {
                assert_eq!(
                    cmd_deliver(&store, &format!("{sid} {bad}")),
                    DELIVER_USAGE,
                    "{bad}"
                );
            }
            assert_eq!(
                cmd_deliver(&store, &format!("{sid} receipt=1 off=90")),
                "OK\n"
            );
            assert_eq!(
                cmd_deliver(&store, &format!("{sid} receipt=1 off=90")),
                "OK\n",
                "a retried retirement is OK, so the bridge's pin is never stranded"
            );
            assert_eq!(
                cmd_deliver(&store, &format!("{sid} receipt=3 off=-")),
                "OK\n"
            );
            assert_eq!(
                receipts(&store),
                [format!(
                    "receipt sid={sid} rid=2 off=10 verdict=handled kind=task from=h-andrew"
                )]
            );
            assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note after", None), "OK 1\n");
            let reply = cmd_outbox(&store, "");
            let post_at = reply.find("post sid=").expect("the post");
            let receipt_at = reply.find("receipt sid=").expect("the receipt");
            assert!(post_at < receipt_at, "{reply}");
            for n in 0..u64::try_from(RECEIPTS_OWED_MAX).expect("fits") + 5 {
                let word = if n % 2 == 0 { "refused" } else { "handled" };
                assert_eq!(cmd_inbox_seen(&ctx, &format!("2 {word}")), "OK seen=3\n");
            }
            let owed = receipts(&store);
            assert_eq!(owed.len(), RECEIPTS_OWED_MAX);
            assert!(
                !owed.iter().any(|l| l.contains(" rid=2 ")),
                "the oldest owed receipt is the one dropped"
            );
        });
    }

    #[test]
    fn inbox_get_at_an_offset_answers_the_ring_or_the_bus_and_the_header_names_the_oldest() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            attach_up(&store, next_bridge_generation());
            let await_fetch = |off: u64| {
                let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
                loop {
                    let peek = cmd_outbox(&store, "");
                    if peek.contains(&format!("fetch sid={sid} off={off}\n")) {
                        return;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "no `fetch off={off}` on the outbox peek within a minute; drain:\n{peek}"
                    );
                    std::thread::yield_now();
                }
            };
            let total = RING_CAP as u64 + 8;
            for i in 0..total {
                let off = 1000 + i;
                assert_eq!(
                    deliver(
                        &store,
                        &sid,
                        off,
                        "s-peer@n-lab",
                        "note",
                        &format!("body-{off}")
                    ),
                    format!("OK {}\n", i + 1)
                );
                if i % 60 == 59 {
                    cmd_inbox(&ctx, "--meta");
                }
            }
            let newest = 1000 + total - 1;
            let header = cmd_inbox(&ctx, "0 --peek --meta");
            assert!(
                header.contains(&format!(
                    " bus_head={newest} oldest_on_bus=@1000 dropped=8 "
                )),
                "the evicted rows' oldest offset is still the oldest on the bus: {header}"
            );
            assert_eq!(
                cmd_inbox_get(&ctx, &format!("@{newest}")),
                format!("OK 9 off={newest} from=s-peer@n-lab kind=note trust=agent\nbody-{newest}")
            );
            let reader = {
                let ctx = ctx.clone();
                std::thread::spawn(move || cmd_inbox_get(&ctx, "@1000"))
            };
            await_fetch(1000);
            assert_eq!(
                cmd_deliver(
                    &store,
                    &format!(
                        "{sid} fetched=1000 from=s-peer@n-lab kind=note trust=agent \
                         text=body-1000"
                    )
                ),
                "OK\n"
            );
            assert_eq!(
                reader.join().expect("the reader finished"),
                "OK 9 off=1000 from=s-peer@n-lab kind=note trust=agent\nbody-1000"
            );
            assert_eq!(
                cmd_inbox_get(&ctx, "@1000"),
                "OK 9 off=1000 from=s-peer@n-lab kind=note trust=agent\nbody-1000"
            );
            assert!(!cmd_outbox(&store, "").contains("fetch "));
            assert_eq!(cmd_inbox_get(&ctx, "1"), "ERR no such message\n");
            let reader = {
                let ctx = ctx.clone();
                std::thread::spawn(move || cmd_inbox_get(&ctx, "@7"))
            };
            await_fetch(7);
            assert_eq!(
                cmd_deliver(&store, &format!("{sid} fetched=7 err=no-record")),
                "OK\n"
            );
            assert_eq!(
                reader.join().expect("the reader finished"),
                "ERR no such record off=7\n"
            );
            let reader = {
                let ctx = ctx.clone();
                std::thread::spawn(move || cmd_inbox_get(&ctx, "@8"))
            };
            await_fetch(8);
            assert_eq!(
                cmd_deliver(&store, &format!("{sid} fetched=8 err=retained floor=40")),
                "OK\n"
            );
            assert_eq!(
                reader.join().expect("the reader finished"),
                "ERR retained-from=40 off=8\n"
            );
            assert_eq!(
                cmd_deliver(&store, &format!("{sid} fetched=8 err=retained floor=x")),
                DELIVER_USAGE
            );
            assert_eq!(cmd_inbox_get(&ctx, "@x"), INBOX_USAGE);

            let reader = {
                let ctx = ctx.clone();
                std::thread::spawn(move || cmd_inbox_get(&ctx, "@1002"))
            };
            await_fetch(1002);
            let line = |tail: &str| {
                cmd_deliver(
                    &store,
                    &format!("{sid} fetched=1002 from=s-peer@n-lab kind=note trust=agent {tail}"),
                )
            };
            assert_eq!(line("more=1 text=abc"), "OK\n");
            assert!(
                cmd_outbox(&store, "").contains(&format!("fetch sid={sid} off=1002\n")),
                "a partial answer is still waited on, so still listed"
            );
            assert_eq!(line("at=3 more=1 text=def"), "OK\n");
            assert_eq!(line("at=6 text=gh%20i"), "OK\n");
            assert_eq!(
                reader.join().expect("the reader finished"),
                "OK 10 off=1002 from=s-peer@n-lab kind=note trust=agent\nabcdefgh i"
            );
            let reader = {
                let ctx = ctx.clone();
                std::thread::spawn(move || cmd_inbox_get(&ctx, "@1003"))
            };
            await_fetch(1003);
            let line = |tail: &str| {
                cmd_deliver(
                    &store,
                    &format!("{sid} fetched=1003 from=s-peer@n-lab kind=note trust=agent {tail}"),
                )
            };
            assert_eq!(line("more=1 text=abc"), "OK\n");
            assert_eq!(line("at=2 text=xyz"), "ERR fetch out of order\n");
            assert_eq!(
                reader.join().expect("the reader finished"),
                "ERR fetch failed reason=malformed off=1003\n",
                "a refused answer ends the read now, not at its 10 s timeout"
            );
            let reader = {
                let ctx = ctx.clone();
                std::thread::spawn(move || cmd_inbox_get(&ctx, "@1004"))
            };
            await_fetch(1004);
            let big = BODY_MAX + 44_000;
            assert_eq!(
                cmd_deliver(
                    &store,
                    &format!(
                        "{sid} fetched=1004 from=s-peer@n-lab kind=note trust=agent len={big} \
                         text=head"
                    )
                ),
                "OK\n"
            );
            assert_eq!(
                reader.join().expect("the reader finished"),
                format!(
                    "OK 4 off=1004 from=s-peer@n-lab kind=note trust=agent truncated=1 \
                     len={big}\nhead"
                )
            );
            cmd_inbox(&ctx, "--meta");
            assert!(
                cmd_deliver(
                    &store,
                    &format!(
                        "{sid} off=5000 from=s-peer@n-lab kind=note trust=agent len=12 text=cut-"
                    )
                )
                .starts_with("OK ")
            );
            let reader = {
                let ctx = ctx.clone();
                std::thread::spawn(move || cmd_inbox_get(&ctx, "@5000"))
            };
            await_fetch(5000);
            assert_eq!(
                cmd_deliver(
                    &store,
                    &format!(
                        "{sid} fetched=5000 from=s-peer@n-lab kind=note trust=agent \
                         text=cut-no-more%21"
                    )
                ),
                "OK\n"
            );
            assert_eq!(
                reader.join().expect("the reader finished"),
                "OK 12 off=5000 from=s-peer@n-lab kind=note trust=agent\ncut-no-more!"
            );
            assert!(
                cmd_deliver(
                    &store,
                    &format!(
                        "{sid} off=5001 from=s-peer@n-lab kind=note trust=agent len=40 text=prefix"
                    )
                )
                .starts_with("OK ")
            );
            let reader = {
                let ctx = ctx.clone();
                std::thread::spawn(move || cmd_inbox_get(&ctx, "@5001"))
            };
            await_fetch(5001);
            assert_eq!(
                cmd_deliver(&store, &format!("{sid} fetched=5001 err=unreadable")),
                "OK\n"
            );
            assert_eq!(
                reader.join().expect("the reader finished"),
                "OK 6 off=5001 from=s-peer@n-lab kind=note trust=agent truncated=1 len=40\nprefix"
            );
        });
    }

    #[test]
    fn after_a_log_switch_the_refill_floor_is_the_new_logs() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let tell = |ident: String| cmd_link(&store, &format!("log {ident}"), Some(generation));
            let last_seen = || {
                ctx.timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .since(None)
                    .rfind(|e| e.kind == "inbox-seen")
                    .map(|e| e.payload.to_string())
                    .unwrap_or_default()
            };
            assert_eq!(tell(format!("{}.0", "aa".repeat(16))), "OK\n");
            assert_eq!(deliver(&store, &sid, 7, "h-a", "note", "a0"), "OK 1\n");
            assert_eq!(deliver(&store, &sid, 900, "h-a", "note", "a1"), "OK 2\n");
            assert_eq!(cmd_inbox_seen(&ctx, "1"), "OK seen=1\n");
            assert_eq!(last_seen(), "1 off=7 floor=900 seen=1");
            assert_eq!(tell(format!("{}.0", "bb".repeat(16))), "OK forgot=1\n");
            assert_eq!(deliver(&store, &sid, 950, "h-a", "note", "b0"), "OK 3\n");
            assert_eq!(cmd_inbox_seen(&ctx, "1"), "OK seen=1\n");
            assert_eq!(
                last_seen(),
                "1 off=7 floor=950 seen=1",
                "A's unhandled row is no floor on B"
            );
            assert_eq!(cmd_inbox_seen(&ctx, "3"), "OK seen=3\n");
            assert_eq!(last_seen(), "3 off=950 floor=951 seen=3");
        });
    }

    #[test]
    fn a_log_switch_forgets_every_offset_the_endpoint_holds() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let a = format!("{}.0", "aa".repeat(16));
            let b = format!("{}.3", "bb".repeat(16));
            let tell = |ident: &str| cmd_link(&store, &format!("log {ident}"), Some(generation));
            assert_eq!(tell(&a), "OK\n", "the first log forgets nothing");
            assert_eq!(
                deliver(&store, &sid, 7, "s-peer@n-lab", "note", "on-a"),
                "OK 1\n"
            );
            assert_eq!(
                deliver(&store, &sid, 7, "s-peer@n-lab", "note", "on-a"),
                "OK 1\n",
                "on one log an offset is one record"
            );
            assert_eq!(tell(&a), "OK\n", "the same log again forgets nothing");
            let parked = {
                let ctx = ctx.clone();
                std::thread::spawn(move || cmd_inbox_get(&ctx, "@1000"))
            };
            let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            while !cmd_outbox(&store, "").contains(&format!("fetch sid={sid} off=1000\n")) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the read never parked"
                );
                std::thread::yield_now();
            }

            assert_eq!(tell(&b), "OK forgot=1\n");
            assert_eq!(
                parked.join().expect("the reader finished"),
                "ERR fetch failed reason=log-replaced off=1000\n"
            );
            assert_eq!(
                deliver(&store, &sid, 7, "s-peer@n-lab", "note", "on-b"),
                "OK 2\n",
                "B's record at 7 is not A's"
            );
            assert_eq!(
                cmd_inbox_get(&ctx, "@7"),
                "OK 4 off=7 from=s-peer@n-lab kind=note trust=agent\non-b"
            );
            assert!(
                cmd_inbox(&ctx, "0 --peek --meta").contains(" bus_head=7 oldest_on_bus=@7 "),
                "the span is B's"
            );

            assert_eq!(cmd_link(&store, "log nope", Some(generation)), LINK_USAGE);
            assert_eq!(
                cmd_link(&store, &format!("log {a} extra"), Some(generation)),
                LINK_USAGE
            );
            let stale = next_bridge_generation();
            assert_eq!(
                cmd_link(&store, &format!("log {a}"), Some(stale)),
                "OK stale=1\n"
            );
            assert_eq!(
                deliver(&store, &sid, 7, "s-peer@n-lab", "note", "on-b"),
                "OK 2\n",
                "a stale lane forgot nothing"
            );
        });
    }

    #[test]
    fn log_switch_retires_old_receipt_debt_without_erasing_the_mail() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let tell = |hex: &str| {
                cmd_link(
                    &store,
                    &format!("log {}.0", hex.repeat(32)),
                    Some(generation),
                )
            };
            assert_eq!(tell("a"), "OK\n");
            assert_eq!(
                deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "old"),
                "OK 1\n"
            );
            assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
            assert!(cmd_outbox(&store, "").contains("receipt sid="));
            assert_eq!(tell("b"), "OK forgot=1\n");
            assert!(
                !cmd_outbox(&store, "").contains("receipt sid="),
                "A's receipt cannot acknowledge an unrelated record at that offset on B"
            );
            assert_eq!(
                cmd_inbox_get(&ctx, "1"),
                "OK 3\nold",
                "historical mail remains readable"
            );
            assert_eq!(
                ctx.fabric.lock().rows[0].decision.as_deref(),
                Some("handled")
            );
            assert_eq!(
                deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "new"),
                "OK 2\n"
            );
            assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=2\n");
            assert!(
                cmd_outbox(&store, "")
                    .contains(&format!("receipt sid={sid} rid=2 off=7 verdict=handled")),
                "B's receipt remains owed"
            );
        });
    }

    #[test]
    fn log_switch_a_later_historical_verdict_is_local() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let tell = |hex: &str| {
                cmd_link(
                    &store,
                    &format!("log {}.0", hex.repeat(32)),
                    Some(generation),
                )
            };
            assert_eq!(tell("a"), "OK\n");
            assert_eq!(
                deliver(&store, &sid, 7, "s-peer@n-lab", "task", "old"),
                "OK 1\n"
            );
            assert_eq!(tell("b"), "OK forgot=1\n");
            assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
            assert!(
                !cmd_outbox(&store, "").contains("receipt sid="),
                "deciding historical mail must not publish a receipt on the new log"
            );
            assert_eq!(
                ctx.fabric.lock().rows[0].decision.as_deref(),
                Some("handled")
            );
            assert_eq!(
                deliver(&store, &sid, 8, "s-peer@n-lab", "task", "new"),
                "OK 2\n"
            );
            assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=2\n");
            assert!(cmd_outbox(&store, "").contains("off=8 verdict=handled"));
        });
    }

    #[test]
    fn log_switch_receipt_waits_do_not_match_another_logs_offset() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let tell = |hex: &str| {
                cmd_link(
                    &store,
                    &format!("log {}.0", hex.repeat(32)),
                    Some(generation),
                )
            };
            let ack = || {
                cmd_deliver(
                    &store,
                    &format!(
                        "{sid} off=20 from=s-peer@n-lab kind=ack trust=agent re=7 verdict=handled text=receipt"
                    ),
                )
            };
            assert_eq!(tell("a"), "OK\n");
            assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note old", None), "OK 1\n");
            assert_eq!(cmd_outbox_sent(&store, &format!("{sid} 1 off=7")), "OK\n");
            assert_eq!(ack(), "OK 1\n");
            assert_eq!(
                wait_receipt(&ctx, 1, 7, "", 0),
                "OK 1 off=7 ack=handled msg=1\n",
                "same-log receipt is valid"
            );
            assert_eq!(tell("b"), "OK forgot=1\n");
            assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note new", None), "OK 2\n");
            assert_eq!(cmd_outbox_sent(&store, &format!("{sid} 2 off=7")), "OK\n");
            assert_eq!(
                wait_receipt(&ctx, 2, 7, "", 0),
                "ERR timeout id=2 off=7\n",
                "A's receipt cannot satisfy B's post"
            );
            assert_eq!(
                cmd_await_inbox(&ctx, &["re=7"], 0),
                "OK timeout\n",
                "offset lookup is scoped to the current log too"
            );
            assert_eq!(ack(), "OK 2\n");
            assert_eq!(
                wait_receipt(&ctx, 2, 7, "", 0),
                "OK 2 off=7 ack=handled msg=2\n"
            );
            assert_eq!(
                wait_receipt(&ctx, 1, 7, "", 0),
                "ERR log-replaced id=1 off=7\n",
                "B's receipt cannot satisfy A's post"
            );
        });
    }

    #[test]
    fn log_switch_keeps_a_queued_posts_new_log_thread() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let tell = |hex: &str| {
                cmd_link(
                    &store,
                    &format!("log {}.0", hex.repeat(32)),
                    Some(generation),
                )
            };
            assert_eq!(tell("a"), "OK\n");
            assert_eq!(
                cmd_post(&ctx, "to=@s-peer kind=note queued", None),
                "OK 1\n"
            );
            assert_eq!(tell("b"), "OK forgot=1\n");
            assert!(
                cmd_outbox(&store, "").contains("post sid="),
                "an unlanded new message remains queued"
            );
            assert_eq!(cmd_outbox_sent(&store, &format!("{sid} 1 off=7")), "OK\n");
            assert_eq!(
                cmd_deliver(
                    &store,
                    &format!(
                        "{sid} off=8 from=s-peer@n-lab kind=answer trust=agent re=7 text=reply"
                    )
                ),
                "OK 1\n"
            );
            assert_eq!(
                ctx.fabric.lock().rows.back().unwrap().re_id,
                Some(1),
                "the post landed on B even though it was queued while A was connected"
            );
        });
    }

    #[test]
    fn log_switch_a_queued_reply_cannot_reuse_a_historical_reference() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let tell = |hex: &str| {
                cmd_link(
                    &store,
                    &format!("log {}.0", hex.repeat(32)),
                    Some(generation),
                )
            };
            assert_eq!(tell("a"), "OK\n");
            assert_eq!(
                deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "old"),
                "OK 1\n"
            );
            assert_eq!(
                cmd_post(&ctx, "to=@s-peer kind=answer re=7 queued-reply", None),
                "OK 1\n"
            );
            assert_eq!(tell("b"), "OK forgot=1\n");
            assert_eq!(
                wait_landing(&ctx, 1, std::time::Instant::now()).err(),
                Some("ERR log-replaced id=1\n".to_string()),
                "the queued reply is reported dead, not published as a reply to B's unrelated offset"
            );
            assert!(!cmd_outbox(&store, "").contains("post sid="));
            assert_eq!(cmd_inbox_get(&ctx, "1"), "OK 3\nold");
        });
    }

    #[cfg(unix)]
    #[test]
    fn log_switch_waits_for_the_commit_receipt_fence_and_retries() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let tell = |hex: &str| {
                cmd_link(
                    &store,
                    &format!("log {}.0", hex.repeat(32)),
                    Some(generation),
                )
            };
            assert_eq!(tell("a"), "OK\n");
            assert_eq!(
                deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "old"),
                "OK 1\n"
            );
            assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
            let before = ctx.fabric.carry_serial();
            ctx.fabric.fence_hold(true);
            assert_eq!(
                tell("b"),
                HOLD_BUSY,
                "a committing successor still owns the carried receipt"
            );
            assert_eq!(
                ctx.fabric.carry_serial(),
                before,
                "no carried state changed under the fence"
            );
            assert!(cmd_outbox(&store, "").contains("receipt sid="));
            ctx.fabric.lift_hold_fence();
            assert_eq!(
                tell("b"),
                "OK forgot=1\n",
                "the same identity retries the unapplied switch"
            );
            assert!(!cmd_outbox(&store, "").contains("receipt sid="));
            assert_eq!(tell("b"), "OK\n", "a completed switch is idempotent");
        });
    }

    #[test]
    fn log_switch_a_successors_first_attach_checks_the_carried_log() {
        let carry = with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            assert_eq!(
                cmd_link(
                    &store,
                    &format!("log {}.0", "a".repeat(32)),
                    Some(generation)
                ),
                "OK\n"
            );
            assert_eq!(
                deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "old"),
                "OK 1\n"
            );
            assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
            assert_eq!(
                cmd_post(&ctx, "to=@s-peer kind=answer re=7 reply", None),
                "OK 1\n"
            );
            assert_eq!(
                cmd_post(&ctx, "to=@s-peer kind=note independent", None),
                "OK 2\n"
            );
            let mut budget = CARRY_MAIL_BYTES;
            ctx.fabric.handoff_carry(&mut budget)
        });
        let wire = aterm_toml::to_string(&carry).expect("outgoing carry");
        let legacy = wire
            .lines()
            .filter(|l| !l.starts_with("bus_log ="))
            .collect::<Vec<_>>()
            .join("\n");
        let invalid = format!("bus_log = \"not-an-identity\"\n{legacy}");
        for (name, source, hex, switched) in [
            ("same log", wire.as_str(), "a", false),
            ("different log", wire.as_str(), "b", true),
            ("legacy unknown log", legacy.as_str(), "a", true),
            ("invalid carried log", invalid.as_str(), "a", true),
        ] {
            let carried: FabricCarry = aterm_toml::from_str(source).expect("incoming carry");
            with_link(|| {
                let store = new_store();
                let (_sid, ctx) = registered(&store);
                ctx.fabric.seed_carry(&carried, &ctx.timeline);
                let generation = next_bridge_generation();
                attach_up(&store, generation);
                assert_eq!(
                    cmd_link(
                        &store,
                        &format!("log {}.0", hex.repeat(32)),
                        Some(generation)
                    ),
                    if switched { "OK forgot=1\n" } else { "OK\n" },
                    "{name}"
                );
                let outbox = cmd_outbox(&store, "");
                assert_eq!(
                    outbox.contains("receipt sid="),
                    !switched,
                    "{name}: {outbox}"
                );
                assert_eq!(outbox.contains("id=1 "), !switched, "{name}: {outbox}");
                assert!(
                    outbox.contains("id=2 ") && outbox.contains("independent"),
                    "unreferenced carried mail stays queued ({name}): {outbox}"
                );
            });
        }
    }

    #[test]
    fn log_switch_a_late_fresh_session_keeps_same_log_receipts_and_dedup() {
        with_link(|| {
            let store = new_store();
            registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let tell = || {
                cmd_link(
                    &store,
                    &format!("log {}.0", "a".repeat(32)),
                    Some(generation),
                )
            };
            assert_eq!(tell(), "OK\n");
            let late = test_handle(2);
            let sid = late.sid.as_str().to_string();
            let ctx = late.ctx.clone();
            register_on_log(&ctx, || {
                store
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .register(late);
            });
            assert_eq!(
                deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "live"),
                "OK 1\n"
            );
            assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
            assert_eq!(
                cmd_post(&ctx, "to=@s-peer kind=answer re=7 reply", None),
                "OK 1\n"
            );
            assert_eq!(tell(), "OK\n", "reattaching to A keeps A's live references");
            let outbox = cmd_outbox(&store, "");
            assert!(outbox.contains("off=7 verdict=handled"), "{outbox}");
            assert!(
                outbox.contains("re=7") && outbox.contains("reply"),
                "{outbox}"
            );
            assert_eq!(
                deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "live"),
                "OK 1\n"
            );
            assert_eq!(
                ctx.fabric.lock().rows.len(),
                1,
                "same-log replay is deduplicated"
            );
            assert_eq!(
                cmd_inbox_get(&ctx, "@7"),
                "OK 4 off=7 from=s-peer@n-lab kind=ask trust=agent\nlive"
            );
        });
    }

    #[test]
    fn log_switch_a_late_adoption_checks_carry_before_its_first_outbox() {
        let carry = with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            assert_eq!(
                cmd_link(
                    &store,
                    &format!("log {}.0", "a".repeat(32)),
                    Some(generation)
                ),
                "OK\n"
            );
            assert_eq!(
                deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "old"),
                "OK 1\n"
            );
            assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
            assert_eq!(
                cmd_post(&ctx, "to=@s-peer kind=answer re=7 reply", None),
                "OK 1\n"
            );
            assert_eq!(
                cmd_post(&ctx, "to=@s-peer kind=note independent", None),
                "OK 2\n"
            );
            ctx.fabric.handoff_carry(&mut CARRY_MAIL_BYTES.clone())
        });
        let wire = aterm_toml::to_string(&carry).expect("outgoing carry");
        let legacy = wire
            .lines()
            .filter(|line| !line.starts_with("bus_log ="))
            .collect::<Vec<_>>()
            .join("\n");
        for (name, source, target, same) in [
            ("same known log", wire.as_str(), "a", true),
            ("replacement log", wire.as_str(), "b", false),
            ("unknown legacy log", legacy.as_str(), "b", false),
        ] {
            with_link(|| {
                let store = new_store();
                registered(&store);
                let generation = next_bridge_generation();
                attach_up(&store, generation);
                assert_eq!(
                    cmd_link(
                        &store,
                        &format!("log {}.0", target.repeat(32)),
                        Some(generation)
                    ),
                    "OK\n"
                );
                let late = test_handle(2);
                let ctx = late.ctx.clone();
                let incoming: FabricCarry = aterm_toml::from_str(source).expect("incoming carry");
                ctx.fabric.seed_carry(&incoming, &ctx.timeline);
                register_on_log(&ctx, || {
                    store
                        .write()
                        .unwrap_or_else(|p| p.into_inner())
                        .register(late);
                });
                let outbox = cmd_outbox(&store, "");
                assert_eq!(outbox.contains("receipt sid="), same, "{name}: {outbox}");
                assert_eq!(outbox.contains("re=7"), same, "{name}: {outbox}");
                assert!(outbox.contains("independent"), "{name}: {outbox}");
                assert_eq!(
                    wait_landing(&ctx, 1, std::time::Instant::now()).err(),
                    if same {
                        Some("ERR timeout id=1\n".to_string())
                    } else {
                        Some("ERR log-replaced id=1\n".to_string())
                    },
                    "{name}"
                );
            });
        }
    }

    #[test]
    fn a_post_queued_across_a_log_switch_threads_its_reply_on_the_new_log() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let tell = |ident: String| cmd_link(&store, &format!("log {ident}"), Some(generation));
            assert_eq!(tell(format!("{}.0", "aa".repeat(16))), "OK\n");
            assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note on a", None), "OK 1\n");
            assert_eq!(
                cmd_deliver(&store, &format!("{sid} landed=1 off=70")),
                "OK\n"
            );
            assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note on b", None), "OK 2\n");
            assert_eq!(tell(format!("{}.1", "aa".repeat(16))), "OK forgot=1\n");
            assert_eq!(
                cmd_deliver(&store, &format!("{sid} landed=2 off=70")),
                "OK\n"
            );
            assert_eq!(
                cmd_deliver(
                    &store,
                    &format!("{sid} off=71 from=s-peer@n-lab kind=note re=70 trust=agent text=re")
                ),
                "OK 1\n"
            );
            let listing = cmd_inbox(&ctx, "--peek --meta");
            let reply = listing
                .lines()
                .find(|l| l.contains(" re=70"))
                .unwrap_or_else(|| panic!("no reply row: {listing}"));
            assert!(reply.contains(" re=70 re-id=2 "), "{reply}");
        });
    }

    fn await_parked(store: &Store, sid: &str, off: u64) {
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        loop {
            let peek = cmd_outbox(store, "");
            if peek.contains(&format!("fetch sid={sid} off={off}\n")) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no `fetch off={off}` on the outbox peek within a minute; drain:\n{peek}"
            );
            std::thread::yield_now();
        }
    }

    fn last_inbox_seen(ctx: &SessionCtx) -> String {
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .since(None)
            .rfind(|e| e.kind == "inbox-seen")
            .map(|e| e.payload.clone())
            .unwrap_or_default()
    }

    #[test]
    fn a_moved_generation_keeps_what_names_a_record_below_the_fork() {
        let a0 = format!("{}.0", "aa".repeat(16));
        let a1 = format!("{}.1", "aa".repeat(16));
        for lineage in [true, false] {
            with_link(|| {
                let store = new_store();
                let (sid, ctx) = registered(&store);
                let generation = next_bridge_generation();
                attach_up(&store, generation);
                let case = format!("lineage={lineage}");
                let tell = |line: &str| cmd_link(&store, &format!("log {line}"), Some(generation));
                let read = |off: u64| {
                    let ctx = ctx.clone();
                    std::thread::spawn(move || cmd_inbox_get(&ctx, &format!("@{off}")))
                };
                let fetched = |off: u64, text: &str| {
                    cmd_deliver(
                        &store,
                        &format!(
                            "{sid} fetched={off} from=s-peer@n-lab kind=note trust=agent text={text}"
                        ),
                    )
                };
                let ack = |off: u64, re: u64| {
                    cmd_deliver(
                        &store,
                        &format!(
                            "{sid} off={off} from=s-peer@n-lab kind=ack trust=agent re={re} \
                             verdict=handled text=ack"
                        ),
                    )
                };
                let reply_row = |re: u64| {
                    let listing = cmd_inbox(&ctx, "--peek --meta");
                    listing
                        .lines()
                        .find(|l| l.contains(&format!(" re={re}")))
                        .unwrap_or_else(|| panic!("{case}: no row re={re}: {listing}"))
                        .to_string()
                };

                assert_eq!(tell(&a0), "OK\n", "{case}");
                assert_eq!(
                    deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "below"),
                    "OK 1\n"
                );
                assert_eq!(
                    deliver(&store, &sid, 40, "s-peer@n-lab", "ask", "above"),
                    "OK 2\n"
                );
                assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
                assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=2\n");
                assert_eq!(
                    cmd_post(&ctx, "to=@s-peer kind=answer re=7 reply-below", None),
                    "OK 1\n"
                );
                assert_eq!(
                    cmd_post(&ctx, "to=@s-peer kind=answer re=40 reply-above", None),
                    "OK 2\n"
                );
                for (id, off) in [(3, 9), (4, 41)] {
                    assert_eq!(
                        cmd_post(&ctx, &format!("to=@s-peer kind=note landed-at-{off}"), None),
                        format!("OK {id}\n")
                    );
                    assert_eq!(
                        cmd_deliver(&store, &format!("{sid} landed={id} off={off}")),
                        "OK\n"
                    );
                }
                let hang_ms = u64::try_from(aterm_spec::wait::MUST_HAPPEN.as_millis())
                    .expect("a minute in milliseconds");
                let waiter = {
                    let ctx = ctx.clone();
                    std::thread::spawn(move || wait_receipt(&ctx, 3, 9, "", hang_ms))
                };
                for off in [3, 44] {
                    let reader = read(off);
                    await_parked(&store, &sid, off);
                    assert_eq!(fetched(off, &format!("old-{off}")), "OK\n");
                    let answer = reader.join().expect("the reader finished");
                    assert!(answer.ends_with(&format!("\nold-{off}")), "{answer}");
                }
                let parked_below = read(5);
                await_parked(&store, &sid, 5);
                let parked_above = read(45);
                await_parked(&store, &sid, 45);

                let moved = if lineage {
                    format!("{a1} continues=20 was={a0}")
                } else {
                    a1.clone()
                };
                assert_eq!(tell(&moved), "OK forgot=1\n", "{case}");
                assert_eq!(tell(&a1), "OK\n", "{case}: the move is applied once");

                let header = cmd_inbox(&ctx, "0 --peek --meta");
                let span = if lineage {
                    " bus_head=7 oldest_on_bus=@3 "
                } else {
                    " bus_head=0 oldest_on_bus=- "
                };
                assert!(header.contains(span), "{case}: {header}");
                assert_eq!(cmd_inbox_seen(&ctx, "2"), "OK seen=2\n");
                assert_eq!(
                    last_inbox_seen(&ctx),
                    if lineage {
                        "2 off=40 floor=20 seen=2"
                    } else {
                        "2 off=40 floor=- seen=2"
                    },
                    "{case}"
                );

                assert_eq!(
                    parked_above.join().expect("the reader finished"),
                    "ERR fetch failed reason=log-replaced off=45\n",
                    "{case}"
                );
                if lineage {
                    await_parked(&store, &sid, 5);
                    assert_eq!(fetched(5, "kept-5"), "OK\n");
                    let answer = parked_below.join().expect("the reader finished");
                    assert!(answer.ends_with("\nkept-5"), "{case}: {answer}");
                    let cached = cmd_inbox_get(&ctx, "@3");
                    assert!(
                        cached.ends_with("\nold-3"),
                        "{case}: answered once, kept: {cached}"
                    );
                } else {
                    assert_eq!(
                        parked_below.join().expect("the reader finished"),
                        "ERR fetch failed reason=log-replaced off=5\n",
                        "{case}"
                    );
                }
                let again = read(44);
                await_parked(&store, &sid, 44);
                assert_eq!(fetched(44, "new-44"), "OK\n");
                let answer = again.join().expect("the reader finished");
                assert!(
                    answer.ends_with("\nnew-44"),
                    "{case}: read again on the new generation: {answer}"
                );

                let outbox = cmd_outbox(&store, "");
                assert_eq!(outbox.contains("reply-below"), lineage, "{case}: {outbox}");
                assert!(!outbox.contains("reply-above"), "{case}: {outbox}");
                assert_eq!(
                    wait_landing(&ctx, 1, std::time::Instant::now()).err(),
                    Some(if lineage {
                        "ERR timeout id=1\n".to_string()
                    } else {
                        "ERR log-replaced id=1\n".to_string()
                    }),
                    "{case}"
                );
                assert_eq!(
                    wait_landing(&ctx, 2, std::time::Instant::now()).err(),
                    Some("ERR log-replaced id=2\n".to_string()),
                    "{case}"
                );
                assert_eq!(
                    outbox.contains(&format!("receipt sid={sid} rid=1 off=7 verdict=handled")),
                    lineage,
                    "{case}: {outbox}"
                );
                assert!(!outbox.contains(" rid=2 "), "{case}: {outbox}");
                let earlier = cmd_outbox(&store, "earlier-receipts=1");
                assert!(
                    earlier.contains(
                        " rid=2 off=40 verdict=handled kind=ask from=s-peer@n-lab log=earlier\n"
                    ),
                    "{case}: {earlier}"
                );
                assert_eq!(
                    earlier.contains(
                        " rid=1 off=7 verdict=handled kind=ask from=s-peer@n-lab log=earlier\n"
                    ),
                    !lineage,
                    "{case}: {earlier}"
                );

                assert_eq!(ack(21, 9), "OK 3\n", "{case}");
                assert_eq!(ack(22, 41), "OK 4\n", "{case}");
                assert_eq!(
                    reply_row(9).contains(" re=9 re-id=3 "),
                    lineage,
                    "{case}: {}",
                    reply_row(9)
                );
                assert!(!reply_row(41).contains(" re-id="), "{}", reply_row(41));
                assert_eq!(
                    waiter.join().expect("the waiter finished"),
                    if lineage {
                        "OK 3 off=9 ack=handled msg=3\n"
                    } else {
                        "ERR log-replaced id=3 off=9\n"
                    },
                    "{case}"
                );
                assert_eq!(
                    wait_receipt(&ctx, 4, 41, "", 0),
                    "ERR log-replaced id=4 off=41\n",
                    "{case}"
                );

                assert_eq!(
                    deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "below"),
                    if lineage { "OK 1\n" } else { "OK 5\n" },
                    "{case}"
                );
                assert_eq!(
                    deliver(&store, &sid, 40, "s-peer@n-lab", "ask", "new-40"),
                    if lineage { "OK 5\n" } else { "OK 6\n" },
                    "{case}"
                );
            });
        }
    }

    #[test]
    fn across_two_moves_an_offset_is_kept_only_below_both_forks() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let a = |g: u64| format!("{}.{g}", "aa".repeat(16));
            let tell = |line: String| cmd_link(&store, &format!("log {line}"), Some(generation));
            let land = |id: u64, off: u64| {
                assert_eq!(
                    cmd_post(&ctx, &format!("to=@s-peer kind=note on-{id}"), None),
                    format!("OK {id}\n")
                );
                assert_eq!(
                    cmd_deliver(&store, &format!("{sid} landed={id} off={off}")),
                    "OK\n"
                );
            };
            let receipt_at_35 = || {
                (
                    cmd_outbox(&store, "").contains(" rid=1 off=35 "),
                    cmd_outbox(&store, "earlier-receipts=1").contains(
                        " rid=1 off=35 verdict=handled kind=ask from=s-peer@n-lab log=earlier\n",
                    ),
                )
            };

            assert_eq!(tell(a(0)), "OK\n");
            land(1, 40);
            assert_eq!(
                deliver(&store, &sid, 40, "s-peer@n-lab", "note", "a0-at-40"),
                "OK 1\n"
            );
            assert_eq!(
                deliver(&store, &sid, 35, "s-peer@n-lab", "ask", "a0-at-35"),
                "OK 2\n"
            );
            assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=2\n");
            assert_eq!(receipt_at_35(), (true, false), "owed on A.0");
            let (watch_35, watch_25) = {
                let mut inbox = ctx.fabric.lock();
                (inbox.watch(35, true), inbox.watch(25, true))
            };
            assert_eq!(
                tell(format!("{} continues=30 was={}", a(1), a(0))),
                "OK forgot=1\n"
            );
            assert_eq!(receipt_at_35(), (false, true), "35 is at or above 30");
            land(2, 40);
            assert_eq!(
                deliver(&store, &sid, 40, "s-peer@n-lab", "note", "a1-at-40"),
                "OK 3\n",
                "A.1's record at 40 is not A.0's"
            );
            assert_eq!(
                tell(format!("{} continues=50 was={}", a(2), a(1))),
                "OK forgot=1\n"
            );

            assert_eq!(
                cmd_deliver(
                    &store,
                    &format!(
                        "{sid} off=55 from=s-peer@n-lab kind=answer trust=agent re=40 text=re"
                    )
                ),
                "OK 4\n"
            );
            let listing = cmd_inbox(&ctx, "--peek --meta");
            let reply = listing
                .lines()
                .find(|l| l.contains(" re=40"))
                .unwrap_or_else(|| panic!("no reply row: {listing}"));
            assert!(
                reply.contains(" re=40 re-id=2 "),
                "the reply answers A.1's post, which A.2 still holds: {reply}"
            );
            assert_eq!(
                cmd_inbox_get(&ctx, "@40"),
                "OK 8 off=40 from=s-peer@n-lab kind=note trust=agent\na1-at-40",
                "A.1's row, not A.0's"
            );
            assert_eq!(
                deliver(&store, &sid, 40, "s-peer@n-lab", "note", "a1-at-40"),
                "OK 3\n",
                "A.1's record handed again on A.2 is the row it was"
            );
            assert_eq!(
                receipt_at_35(),
                (false, true),
                "A.2 holds 35 below its own fork, but not A.0's record there"
            );
            assert!(!watch_35.on_log(), "a wait on A.0's 35 ends: taken at 30");
            assert!(watch_25.on_log(), "a wait on A.0's 25 goes on: below both");
            drop((watch_35, watch_25));
            let _armed = ctx.fabric.lock().watch(1, true);
            assert_eq!(ctx.fabric.lock().watches.len(), 1);
        });
    }

    #[test]
    fn a_session_ahead_of_the_bridges_witness_keeps_below_the_chains_fork() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let a = |g: u64| format!("{}.{g}", "aa".repeat(16));
            let tell = |line: String| cmd_link(&store, &format!("log {line}"), Some(generation));
            assert_eq!(tell(a(0)), "OK\n");
            assert_eq!(
                deliver(&store, &sid, 10, "s-peer@n-lab", "note", "a0-at-10"),
                "OK 1\n"
            );
            assert_eq!(
                deliver(&store, &sid, 35, "s-peer@n-lab", "ask", "a0-at-35"),
                "OK 2\n"
            );
            assert_eq!(
                deliver(&store, &sid, 40, "s-peer@n-lab", "note", "a0-at-40"),
                "OK 3\n"
            );
            assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=2\n");
            assert_eq!(
                tell(format!("{} continues=50 was={}", a(1), a(0))),
                "OK forgot=1\n"
            );
            assert_eq!(
                tell(format!("{} continues=30 was={}", a(2), a(0))),
                "OK forgot=1\n"
            );
            assert_eq!(
                deliver(&store, &sid, 10, "s-peer@n-lab", "note", "a0-at-10"),
                "OK 1\n",
                "below the chain's fork: the row it was"
            );
            assert_eq!(
                deliver(&store, &sid, 40, "s-peer@n-lab", "note", "a2-at-40"),
                "OK 4\n",
                "at or above it: A.2's record at 40 is a new row"
            );
            assert!(
                cmd_outbox(&store, "earlier-receipts=1").contains(
                    " rid=1 off=35 verdict=handled kind=ask from=s-peer@n-lab log=earlier\n"
                ),
                "the receipt at 35 is an earlier log's"
            );

            let other = new_store();
            let (other_sid, _other_ctx) = registered(&other);
            let other_link =
                |line: String| cmd_link(&other, &format!("log {line}"), Some(generation));
            let b0 = format!("{}.0", "bb".repeat(16));
            assert!(other_link(b0).starts_with("OK"));
            assert_eq!(
                deliver(&other, &other_sid, 10, "s-peer@n-lab", "note", "b0-at-10"),
                "OK 1\n"
            );
            assert_eq!(
                other_link(format!("{} continues=30 was={}", a(3), a(0))),
                "OK forgot=1\n"
            );
            assert_eq!(
                deliver(&other, &other_sid, 10, "s-peer@n-lab", "note", "a3-at-10"),
                "OK 2\n",
                "a session on another log keeps nothing"
            );
        });
    }

    #[test]
    fn link_log_continues_takes_only_an_earlier_generation_of_the_same_log() {
        with_link(|| {
            let store = new_store();
            let (sid, _ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let a = |g: u64| format!("{}.{g}", "aa".repeat(16));
            let b0 = format!("{}.0", "bb".repeat(16));
            let tell = |line: String| cmd_link(&store, &format!("log {line}"), Some(generation));
            let at_seven = || deliver(&store, &sid, 7, "s-peer@n-lab", "note", "seven");
            assert_eq!(tell(a(0)), "OK\n");
            assert_eq!(at_seven(), "OK 1\n");
            for line in [
                format!("{} continues=20", a(1)),
                format!("{} was={}", a(1), a(0)),
                format!("{} continues=x was={}", a(1), a(0)),
                format!("{} continues=-1 was={}", a(1), a(0)),
                format!("{} continues=20 was={b0}", a(1)),
                format!("{} continues=20 was={}", a(1), a(1)),
                format!("{} continues=20 was={}", a(1), a(2)),
                format!("{} continues=20 was=nope", a(1)),
                format!("{} continues=20 continues=21 was={}", a(1), a(0)),
                format!("{} continues=20 was={} was={}", a(1), a(0), a(0)),
                format!("{} continues=20 was={} extra", a(1), a(0)),
            ] {
                assert_eq!(tell(line.clone()), LINK_USAGE, "{line}");
                assert_eq!(at_seven(), "OK 1\n", "nothing forgotten by {line}");
            }
            let stale = next_bridge_generation();
            assert_eq!(
                cmd_link(
                    &store,
                    &format!("log {} continues=20 was={}", a(1), a(0)),
                    Some(stale)
                ),
                "OK stale=1\n"
            );
            assert_eq!(at_seven(), "OK 1\n", "a stale lane forgot nothing");
            assert_eq!(
                tell(format!("{} was={} continues=20", a(1), a(0))),
                "OK forgot=1\n"
            );
            assert_eq!(at_seven(), "OK 1\n", "7 is below the fork");
        });
    }

    #[test]
    fn a_session_carried_on_the_continued_generation_keeps_what_is_below_the_fork() {
        let a0 = format!("{}.0", "aa".repeat(16));
        let a1 = format!("{}.1", "aa".repeat(16));
        let b0 = format!("{}.0", "bb".repeat(16));
        let carry = |on: &str| {
            with_link(|| {
                let store = new_store();
                let (sid, ctx) = registered(&store);
                let generation = next_bridge_generation();
                attach_up(&store, generation);
                assert_eq!(
                    cmd_link(&store, &format!("log {on}"), Some(generation)),
                    "OK\n"
                );
                for (id, off) in [(1, 7), (2, 40)] {
                    assert_eq!(
                        deliver(&store, &sid, off, "s-peer@n-lab", "ask", "ask"),
                        format!("OK {id}\n")
                    );
                    assert_eq!(
                        cmd_inbox_seen(&ctx, &format!("{id} handled")),
                        format!("OK seen={id}\n")
                    );
                    assert_eq!(
                        cmd_post(
                            &ctx,
                            &format!("to=@s-peer kind=answer re={off} reply-{off}"),
                            None
                        ),
                        format!("OK {id}\n")
                    );
                }
                ctx.fabric.handoff_carry(&mut CARRY_MAIL_BYTES.clone())
            })
        };
        let on_a0 = aterm_toml::to_string(&carry(&a0)).expect("outgoing carry");
        let on_b0 = aterm_toml::to_string(&carry(&b0)).expect("outgoing carry");
        let legacy = on_a0
            .lines()
            .filter(|line| !line.starts_with("bus_log ="))
            .collect::<Vec<_>>()
            .join("\n");
        for (name, source, kept) in [
            ("carried on A.0", on_a0.as_str(), true),
            ("carried on B.0", on_b0.as_str(), false),
            ("carried with no clock", legacy.as_str(), false),
        ] {
            with_link(|| {
                let store = new_store();
                registered(&store);
                let generation = next_bridge_generation();
                attach_up(&store, generation);
                assert_eq!(
                    cmd_link(&store, &format!("log {a0}"), Some(generation)),
                    "OK\n"
                );
                assert_eq!(
                    cmd_link(
                        &store,
                        &format!("log {a1} continues=20 was={a0}"),
                        Some(generation)
                    ),
                    "OK forgot=1\n"
                );
                assert_eq!(
                    cmd_link(&store, &format!("log {a1}"), Some(generation)),
                    "OK\n"
                );
                let late = test_handle(2);
                let ctx = late.ctx.clone();
                let incoming: FabricCarry = aterm_toml::from_str(source).expect("incoming carry");
                ctx.fabric.seed_carry(&incoming, &ctx.timeline);
                register_on_log(&ctx, || {
                    store
                        .write()
                        .unwrap_or_else(|p| p.into_inner())
                        .register(late);
                });
                let outbox = cmd_outbox(&store, "");
                assert_eq!(
                    outbox.contains(" rid=1 off=7 verdict=handled"),
                    kept,
                    "{name}: {outbox}"
                );
                assert!(!outbox.contains(" rid=2 "), "{name}: {outbox}");
                assert_eq!(outbox.contains("reply-7"), kept, "{name}: {outbox}");
                assert!(!outbox.contains("reply-40"), "{name}: {outbox}");
                assert_eq!(
                    wait_landing(&ctx, 2, std::time::Instant::now()).err(),
                    Some("ERR log-replaced id=2\n".to_string()),
                    "{name}"
                );
            });
        }
    }

    #[test]
    fn a_verdict_given_after_the_move_owes_its_receipt_below_the_fork() {
        let a0 = format!("{}.0", "aa".repeat(16));
        let a1 = format!("{}.1", "aa".repeat(16));
        for lineage in [true, false] {
            with_link(|| {
                let store = new_store();
                let (sid, ctx) = registered(&store);
                let generation = next_bridge_generation();
                attach_up(&store, generation);
                let case = format!("lineage={lineage}");
                let tell = |line: &str| cmd_link(&store, &format!("log {line}"), Some(generation));
                assert_eq!(tell(&a0), "OK\n", "{case}");
                assert_eq!(
                    deliver(&store, &sid, 3, "s-peer@n-lab", "note", "read"),
                    "OK 1\n"
                );
                assert_eq!(
                    deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "below"),
                    "OK 2\n"
                );
                assert_eq!(
                    deliver(&store, &sid, 40, "s-peer@n-lab", "ask", "above"),
                    "OK 3\n"
                );
                assert_eq!(cmd_inbox_seen(&ctx, "1"), "OK seen=1\n");
                assert_eq!(last_inbox_seen(&ctx), "1 off=3 floor=7 seen=1", "{case}");

                let moved = if lineage {
                    format!("{a1} continues=20 was={a0}")
                } else {
                    a1.clone()
                };
                assert_eq!(tell(&moved), "OK forgot=1\n", "{case}");

                assert_eq!(cmd_inbox_seen(&ctx, "1"), "OK seen=1\n");
                assert_eq!(
                    last_inbox_seen(&ctx),
                    if lineage {
                        "1 off=3 floor=7 seen=1"
                    } else {
                        "1 off=3 floor=- seen=1"
                    },
                    "{case}: the unread ask below the fork is still the floor"
                );
                assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=2\n");
                assert_eq!(cmd_inbox_seen(&ctx, "3 handled"), "OK seen=3\n");
                let outbox = cmd_outbox(&store, "");
                assert_eq!(
                    outbox.contains(&format!("receipt sid={sid} rid=1 off=7 verdict=handled")),
                    lineage,
                    "{case}: {outbox}"
                );
                assert!(!outbox.contains(" rid=2 "), "{case}: {outbox}");
                let earlier = cmd_outbox(&store, "earlier-receipts=1");
                assert!(
                    earlier.contains(
                        " rid=2 off=40 verdict=handled kind=ask from=s-peer@n-lab log=earlier\n"
                    ),
                    "{case}: {earlier}"
                );
                assert_eq!(
                    earlier.contains(
                        " rid=1 off=7 verdict=handled kind=ask from=s-peer@n-lab log=earlier\n"
                    ),
                    !lineage,
                    "{case}: {earlier}"
                );
            });
        }
    }

    fn armed_watches(ctx: &SessionCtx) -> usize {
        ctx.fabric
            .lock()
            .watches
            .iter()
            .filter(|w| w.strong_count() > 0)
            .count()
    }

    #[test]
    fn await_inbox_re_follows_what_the_move_kept() {
        let a0 = format!("{}.0", "aa".repeat(16));
        let a1 = format!("{}.1", "aa".repeat(16));
        let hang_ms = u64::try_from(aterm_spec::wait::MUST_HAPPEN.as_millis())
            .expect("a minute in milliseconds");
        for lineage in [true, false] {
            with_link(|| {
                let store = new_store();
                let (sid, ctx) = registered(&store);
                let generation = next_bridge_generation();
                attach_up(&store, generation);
                let case = format!("lineage={lineage}");
                let tell = |line: &str| cmd_link(&store, &format!("log {line}"), Some(generation));
                let park = |args: &'static [&'static str]| {
                    let ctx = ctx.clone();
                    std::thread::spawn(move || cmd_await_inbox(&ctx, args, hang_ms))
                };
                assert_eq!(tell(&a0), "OK\n", "{case}");
                for (id, off) in [(1, 9), (2, 41)] {
                    assert_eq!(
                        cmd_post(&ctx, &format!("to=@s-peer kind=note at-{off}"), None),
                        format!("OK {id}\n")
                    );
                    assert_eq!(
                        cmd_deliver(&store, &format!("{sid} landed={id} off={off}")),
                        "OK\n"
                    );
                }
                assert_eq!(
                    cmd_deliver(
                        &store,
                        &format!(
                            "{sid} off=15 from=s-peer@n-lab kind=answer trust=agent re=9 text=r"
                        )
                    ),
                    "OK 1\n"
                );
                let on_nine = park(&["re=9", "since=1"]);
                let on_forty_one = park(&["re=41"]);
                let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
                while armed_watches(&ctx) < 2 {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "{case}: the two waits did not arm within a minute"
                    );
                    std::thread::yield_now();
                }

                let moved = if lineage {
                    format!("{a1} continues=20 was={a0}")
                } else {
                    a1.clone()
                };
                assert_eq!(tell(&moved), "OK forgot=1\n", "{case}");
                assert_eq!(
                    on_forty_one.join().expect("the wait finished"),
                    "ERR log-replaced re=41\n",
                    "{case}"
                );
                assert_eq!(
                    cmd_await_inbox(&ctx, &["re=9", "since=0"], 0),
                    if lineage {
                        "OK inbox 1\n"
                    } else {
                        "OK timeout\n"
                    },
                    "{case}: the reply delivered on A.0"
                );
                if lineage {
                    assert_eq!(
                        cmd_deliver(
                            &store,
                            &format!(
                                "{sid} off=21 from=s-peer@n-lab kind=ack trust=agent re=9 \
                                 verdict=handled text=ack"
                            )
                        ),
                        "OK 2\n"
                    );
                }
                assert_eq!(
                    on_nine.join().expect("the wait finished"),
                    if lineage {
                        "OK inbox 2\n"
                    } else {
                        "ERR log-replaced re=9\n"
                    },
                    "{case}"
                );
            });
        }
    }

    #[test]
    fn a_chain_of_moves_keeps_what_is_below_every_fork_however_long() {
        let a = |g: u64| format!("{}.{g}", "aa".repeat(16));
        for lineage in [true, false] {
            with_link(|| {
                let store = new_store();
                let (sid, ctx) = registered(&store);
                let generation = next_bridge_generation();
                attach_up(&store, generation);
                let case = format!("lineage={lineage}");
                let tell =
                    |line: String| cmd_link(&store, &format!("log {line}"), Some(generation));
                assert_eq!(tell(a(0)), "OK\n", "{case}");
                assert_eq!(
                    deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "seven"),
                    "OK 1\n"
                );
                for g in 1..=9 {
                    let line = if lineage {
                        format!("{} continues={} was={}", a(g), 20 + g, a(g - 1))
                    } else {
                        a(g)
                    };
                    assert_eq!(tell(line), "OK forgot=1\n", "{case}: move {g}");
                }
                assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
                assert_eq!(
                    cmd_outbox(&store, "")
                        .contains(&format!("receipt sid={sid} rid=1 off=7 verdict=handled")),
                    lineage,
                    "{case}"
                );
                assert_eq!(
                    cmd_outbox(&store, "earlier-receipts=1").contains(
                        " rid=1 off=7 verdict=handled kind=ask from=s-peer@n-lab log=earlier\n"
                    ),
                    !lineage,
                    "{case}"
                );
                assert_eq!(
                    deliver(&store, &sid, 7, "s-peer@n-lab", "ask", "seven"),
                    if lineage { "OK 1\n" } else { "OK 2\n" },
                    "{case}"
                );
                if lineage {
                    assert_eq!(
                        cmd_inbox_get(&ctx, "@7"),
                        "OK 5 off=7 from=s-peer@n-lab kind=ask trust=agent\nseven",
                        "{case}"
                    );
                }
            });
        }
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);
            let tell = |line: String| cmd_link(&store, &format!("log {line}"), Some(generation));
            assert_eq!(tell(format!("{}.0", "bb".repeat(16))), "OK\n");
            assert_eq!(
                deliver(&store, &sid, 5, "s-peer@n-lab", "note", "b0"),
                "OK 1\n"
            );
            assert_eq!(tell(a(0)), "OK forgot=1\n", "another log");
            for g in 1..=9 {
                assert_eq!(
                    tell(format!("{} continues=100 was={}", a(g), a(g - 1))),
                    "OK forgot=1\n",
                    "move {g}"
                );
            }
            assert_eq!(
                deliver(&store, &sid, 5, "s-peer@n-lab", "note", "a9"),
                "OK 2\n",
                "A.9's record at 5 is not B.0's"
            );
            assert_eq!(
                cmd_inbox_get(&ctx, "@5"),
                "OK 2 off=5 from=s-peer@n-lab kind=note trust=agent\na9"
            );
        });
    }

    fn parse_outbox(reply: &str) -> (usize, Vec<(String, String)>) {
        let (declared, posts, _) = parse_outbox_frame(reply);
        (declared, posts)
    }

    fn says_more(reply: &str) -> bool {
        parse_outbox_frame(reply).2
    }

    fn parse_outbox_frame(reply: &str) -> (usize, Vec<(String, String)>, bool) {
        let (head, payload) = reply.split_once('\n').expect("a header line");
        let declared: usize = head
            .strip_prefix("OK ")
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("bad outbox header {head:?}"));
        assert_eq!(
            declared,
            payload.len(),
            "the length prefix must equal the bytes that follow"
        );
        let mut out = Vec::new();
        let mut more = false;
        let mut rest = payload;
        while !rest.is_empty() {
            let (line, tail) = rest.split_once('\n').expect("a post line");
            if line.starts_with("receipt ") || line.starts_with("fetch ") {
                rest = tail;
                continue;
            }
            if line == "more" {
                assert!(tail.is_empty(), "`more` is the frame's last line");
                more = true;
                rest = tail;
                continue;
            }
            let len: usize = line
                .rsplit_once(" len=")
                .and_then(|(_, n)| n.parse().ok())
                .unwrap_or_else(|| panic!("no len= on {line:?}"));
            assert!(tail.len() >= len, "the frame promised {len} body bytes");
            out.push((line.to_string(), tail[..len].to_string()));
            rest = &tail[len..];
        }
        (declared, out, more)
    }

    #[test]
    fn outbox_hands_the_bridge_the_body_and_outbox_sent_retires_it() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert_eq!(
            cmd_post(&ctx, "to=@s-abc kind=note first body", None),
            "OK 1\n"
        );
        assert_eq!(
            cmd_post(
                &ctx,
                "to=h-andrew kind=answer re=90312 dl=240000 via=s-inner second",
                None
            ),
            "OK 2\n"
        );
        let (_, posts) = parse_outbox(&cmd_outbox(&store, ""));
        assert_eq!(posts.len(), 2, "both queued posts drain");
        assert!(
            posts[0]
                .0
                .starts_with(&format!("post sid={sid} id=1 to=@s-abc kind=note len=")),
            "{:?}",
            posts[0].0
        );
        assert_eq!(posts[0].1, "first body");
        assert_eq!(posts[1].1, "second");
        for field in ["kind=answer", "re=90312", "dl=240000", "via=s-inner"] {
            assert!(posts[1].0.contains(field), "{} lacks {field}", posts[1].0);
        }

        let (_, again) = parse_outbox(&cmd_outbox(&store, ""));
        assert_eq!(again, posts, "outbox moves nothing");

        assert_eq!(
            cmd_outbox_sent(&store, &format!("{sid} 1 off=90355")),
            "OK\n"
        );
        let (_, after) = parse_outbox(&cmd_outbox(&store, ""));
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].1, "second");
        {
            let inbox = ctx.fabric.lock();
            let row = inbox.posts.iter().find(|p| p.id == 1).expect("post 1 kept");
            assert_eq!(row.off, Some(90355));
            assert!(
                row.body.is_empty(),
                "the retained body is dropped at retirement"
            );
        }
        let listing = cmd_inbox(&ctx, "--peek");
        assert!(listing.contains("post 2 to=h-andrew"), "{listing}");
        assert!(
            !listing.contains("post 1 "),
            "a landed post is not in flight: {listing}"
        );

        assert_eq!(
            cmd_outbox_sent(&store, &format!("{sid} 1 off=90355")),
            "OK\n"
        );
        assert_eq!(cmd_outbox_sent(&store, &format!("{sid} 2 off=-")), "OK\n");
        let (n, none_left) = parse_outbox(&cmd_outbox(&store, ""));
        assert_eq!(
            (n, none_left.len()),
            (0, 0),
            "an undeliverable post leaves the queue"
        );
        assert_eq!(
            cmd_outbox_sent(&store, &format!("{sid} 9 off=1")),
            "ERR no such post\n"
        );
        assert_eq!(
            cmd_outbox_sent(&store, "s-nope 1 off=1"),
            "ERR no such session\n"
        );
    }

    #[test]
    fn an_outbound_body_may_contain_newlines() {
        let store = new_store();
        let (_sid, ctx) = registered(&store);
        let body = "line one\nline two\n\nlen=7 post sid=fake id=99 to=x kind=note len=3\n";
        assert_eq!(
            cmd_post(
                &ctx,
                &format!("to=@s-abc kind=note len={}", body.len()),
                Some(body.as_bytes().to_vec())
            ),
            "OK 1\n"
        );
        let (_, posts) = parse_outbox(&cmd_outbox(&store, ""));
        assert_eq!(
            posts.len(),
            1,
            "the embedded post-shaped line is BODY, not a row"
        );
        assert_eq!(posts[0].1, body);
    }

    #[test]
    fn a_full_outbox_refuses_the_post_instead_of_dropping_one() {
        let store = new_store();
        let (_sid, ctx) = registered(&store);
        for i in 1..=OUTBOX_CAP {
            assert_eq!(
                cmd_post(&ctx, &format!("to=@s-abc kind=note m{i}"), None),
                format!("OK {i}\n")
            );
        }
        let refused = cmd_post(&ctx, "to=@s-abc kind=note one-too-many", None);
        assert!(
            refused.starts_with(&format!("ERR outbox full queued={OUTBOX_CAP} ")),
            "{refused}"
        );
        let (_, posts) = parse_outbox(&cmd_outbox(&store, ""));
        assert_eq!(
            posts.len(),
            OUTBOX_CAP,
            "nothing already queued was evicted"
        );
        assert_eq!(posts[0].1, "m1", "the OLDEST message is still there");
    }

    #[test]
    fn a_parked_post_wakes_on_an_undeliverable_verdict() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            attach_up(&store, next_bridge_generation());
            let waiter = {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    cmd_post(&ctx, "to=@s-abc kind=ask --wait=60000 hello", None)
                })
            };
            let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            while !waiter.is_finished() {
                let reply = cmd_outbox_sent(&store, &format!("{sid} 1 off=-"));
                assert!(reply == "OK\n" || reply == "ERR no such post\n", "{reply}");
                assert!(
                    std::time::Instant::now() < deadline,
                    "the waiter never woke"
                );
                std::thread::yield_now();
            }
            assert_eq!(
                waiter.join().expect("the waiter thread"),
                "ERR undeliverable id=1\n"
            );
        });
    }

    #[test]
    fn outbox_drains_every_session_and_max_bounds_the_batch() {
        let store = new_store();
        let (sid_a, ctx_a) = registered(&store);
        let (sid_b, ctx_b) = registered_as(&store, 2);
        assert_eq!(cmd_post(&ctx_a, "to=h-x kind=note from-a", None), "OK 1\n");
        assert_eq!(cmd_post(&ctx_b, "to=h-x kind=note from-b", None), "OK 1\n");
        let whole = cmd_outbox(&store, "");
        assert!(!says_more(&whole), "every post listed: {whole:?}");
        let (_, posts) = parse_outbox(&whole);
        assert_eq!(posts.len(), 2);
        assert!(
            posts[0].0.contains(&format!("sid={sid_a}")),
            "{:?}",
            posts[0].0
        );
        assert!(
            posts[1].0.contains(&format!("sid={sid_b}")),
            "{:?}",
            posts[1].0
        );
        let cut = cmd_outbox(&store, "1");
        assert!(says_more(&cut), "B's post is unlisted: {cut:?}");
        let (_, one) = parse_outbox(&cut);
        assert_eq!(one.len(), 1, "`max` bounds the batch");
        assert_eq!(one[0].1, "from-a");
        assert_eq!(cmd_outbox(&store, "nope"), OUTBOX_USAGE);
    }

    #[test]
    fn a_parked_post_learns_the_bridge_died_rather_than_timing_out() {
        with_link(|| {
            let store = new_store();
            let (_sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            attach_up(&store, generation);

            let poster = {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    cmd_post(
                        &ctx,
                        "to=@s-peer kind=ask --wait=600000 which branch?",
                        None,
                    )
                })
            };
            for spin in 0.. {
                assert!(spin < 5_000_000, "the post never reached the outbox");
                if cmd_inbox(&ctx, "--peek").contains("post 1 ") {
                    break;
                }
                std::thread::yield_now();
            }
            bridge_lost(&store, generation);

            assert_eq!(
                poster.join().expect("the poster thread"),
                "ERR fabric disconnected id=1 queued=1\n",
                "a parked waiter must get the verdict, not its whole timeout"
            );
        });
    }

    #[test]
    fn post_may_not_claim_a_verdict_a_bridge_records() {
        with_link(post_may_not_claim_a_verdict_a_bridge_records_body);
    }

    fn post_may_not_claim_a_verdict_a_bridge_records_body() {
        let store = new_store();
        let (sid, ctx) = registered(&store);

        for kind in ["expired", "undeliverable"] {
            assert_eq!(
                cmd_post(
                    &ctx,
                    &format!("to=@s-victim kind={kind} re=90312 nope"),
                    None
                ),
                POST_USAGE,
                "`post kind={kind}` forges a bridge-recorded verdict"
            );
        }
        for (i, kind) in POSTABLE.iter().enumerate() {
            let id = i + 1;
            let reply = cmd_post(&ctx, &format!("to=@s-peer kind={kind} hi"), None);
            let expected = if matches!(*kind, "ask" | "task") {
                format!("ERR fabric absent id={id} no-bridge=1\n")
            } else {
                format!("OK {id}\n")
            };
            assert_eq!(reply, expected, "`post kind={kind}` is one of the seven");
        }

        for kind in ["expired", "undeliverable"] {
            assert_eq!(
                cmd_deliver(
                    &store,
                    &format!("{sid} off=1 from=h-a kind={kind} trust=agent text=x")
                ),
                "OK 1\n",
                "`deliver kind={kind}` is the bridge recording a verdict, and stays legal"
            );
        }
    }

    #[test]
    fn eviction_takes_an_agent_row_before_a_listed_human_one_and_counts_it() {
        let store = new_store();
        let (sid, ctx) = registered(&store);

        assert_eq!(
            deliver(&store, &sid, 1, "h-andrew", "task", "stop"),
            "OK 1\n"
        );
        assert!(cmd_inbox(&ctx, "").contains("from=h-andrew"));
        assert!(
            ctx.fabric.lock().rows.iter().all(|r| r.listed),
            "the human's row is listed, which is what used to make it the victim"
        );

        for off in 2..=u64::try_from(RING_CAP).expect("cap fits") {
            assert!(
                deliver(&store, &sid, off, &format!("a-svc{off}"), "note", "x").starts_with("OK ")
            );
        }
        assert_eq!(ctx.fabric.lock().rows.len(), RING_CAP);

        assert!(deliver(&store, &sid, 9999, "a-late", "note", "x").starts_with("OK "));
        let reply = cmd_inbox(&ctx, "--peek");
        assert!(
            reply.contains("from=h-andrew kind=task"),
            "a LISTED human row was evicted ahead of 511 unlisted agent rows"
        );
        assert!(!reply.contains("from=a-svc2 "), "the oldest agent row left");

        assert!(
            reply.contains(" dropped=1 "),
            "an evicted unhandled row must be reported: {}",
            reply.lines().next().unwrap_or("")
        );
    }

    #[test]
    fn an_evicted_handled_row_is_not_reported_as_dropped() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        for off in 1..=u64::try_from(RING_CAP).expect("cap fits") {
            deliver(&store, &sid, off, &format!("a-svc{off}"), "note", "x");
        }
        cmd_inbox(&ctx, "");
        assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
        deliver(&store, &sid, 9999, "a-late", "note", "x");
        assert!(
            cmd_inbox(&ctx, "--peek").contains(" dropped=0 "),
            "row 1 was handled; losing it is not a drop the agent needs told about"
        );
    }

    #[test]
    fn pending_counts_the_rows_a_bounded_or_filtered_reply_left_behind() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        for off in 1..=10 {
            deliver(&store, &sid, off, "a-svc", "note", "x");
        }
        let header = |reply: &str| reply.lines().next().unwrap_or("").to_string();

        let filtered = header(&cmd_inbox(&ctx, "since=5"));
        assert!(filtered.starts_with("OK 5 "), "{filtered}");
        assert!(
            filtered.contains(" pending=5"),
            "five rows below the newest listed one are still unread: {filtered}"
        );

        let bounded = header(&cmd_inbox(&ctx, "1"));
        assert!(bounded.starts_with("OK 1 "), "{bounded}");
        assert!(
            bounded.contains(" pending=5"),
            "the reply carried one row and left five: {bounded}"
        );

        assert!(header(&cmd_inbox(&ctx, "")).contains(" pending=0"));
    }

    #[test]
    fn a_late_attach_from_a_dead_launch_cannot_regress_the_live_generation() {
        with_link(|| {
            let store = new_store();
            let (sid, _ctx) = registered(&store);

            let first = next_bridge_generation();
            bridge_attached(first);
            assert_eq!(deliver(&store, &sid, 1, "h-a", "task", "go"), "OK 1\n");

            let second = next_bridge_generation();
            attach_up(&store, second);
            assert_eq!(fabric_state(), "connected");

            bridge_attached(first);
            assert_eq!(
                fabric_state(),
                "connected",
                "a ghost lane's attach must not regress the owning generation"
            );

            assert_eq!(
                bridge_lost(&store, first),
                1,
                "the hold sweep is unconditional"
            );
            assert_eq!(
                fabric_state(),
                "connected",
                "the ghost lane pinned the LIVE bridge at disconnected"
            );

            assert_eq!(bridge_lost(&store, second), 1);
            assert_eq!(fabric_state(), "disconnected");
        });
    }

    #[test]
    fn one_registrys_prune_does_not_evict_another_registrys_governed_sids() {
        with_link(|| {
            let bystander = new_store();
            let (b_sid, _b_ctx) = registered_as(&bystander, 1);
            assert_eq!(deliver(&bystander, &b_sid, 1, "h-a", "note", "x"), "OK 1\n");
            assert!(touched_contains(&b_sid), "the bystander is governed");

            let driver = new_store();
            let n = TOUCHED_PRUNE_AT + 8;
            let mut d_sids = Vec::with_capacity(n);
            for local in 1..=n as u64 {
                let (sid, _ctx) = registered_as(&driver, local);
                assert_eq!(deliver(&driver, &sid, local, "h-a", "note", "x"), "OK 1\n");
                d_sids.push(sid);
            }
            assert!(
                touched_contains(&b_sid),
                "a prune driven by one registry must not forget another's live session"
            );

            for local in 1..n as u64 {
                driver
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .deregister_local(local);
            }
            let last = d_sids.last().expect("a session").clone();
            assert_eq!(
                deliver(&driver, &last, 9_999, "h-a", "note", "again"),
                "OK 2\n"
            );
            for sid in &d_sids[..n - 1] {
                assert!(
                    !touched_contains(sid),
                    "the driver's own dead sids are still pruned"
                );
            }
            assert!(
                touched_contains(&last),
                "the driver's live session survives"
            );
            assert!(
                touched_contains(&b_sid),
                "and the bystander is still governed after the driver's prune"
            );
        });
    }

    #[test]
    fn one_peer_cannot_mint_extra_quota_by_rotating_the_sid_it_claims() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert_eq!(
            deliver(&store, &sid, 1, "h-andrew", "task", "stop"),
            "OK 1\n"
        );

        let mut accepted = 0usize;
        let mut refused = 0usize;
        for off in 2..=201u64 {
            let from = format!("s-rogue{off:04}@n-rogue");
            match deliver(&store, &sid, off, &from, "note", "spam").as_str() {
                "ERR quota\n" => refused += 1,
                ok if ok.starts_with("OK ") => accepted += 1,
                other => panic!("unexpected deliver reply {other:?}"),
            }
        }
        assert_eq!(
            accepted, SENDER_QUOTA,
            "one PEER gets one allowance however many sids it claims"
        );
        assert_eq!(refused, 200 - SENDER_QUOTA);

        assert!(
            deliver(&store, &sid, 900, "s-a@n-other", "note", "hello").starts_with("OK "),
            "an honest peer must not be charged for the rogue's flood"
        );
        assert!(deliver(&store, &sid, 901, "h-andrew", "note", "hi").starts_with("OK "));
        let reply = cmd_inbox(&ctx, "--peek");
        assert!(reply.contains("from=h-andrew kind=task"), "{reply}");
        assert!(reply.contains(" dropped=0 "), "{reply}");
        cmd_inbox(&ctx, "");
        assert!(
            deliver(&store, &sid, 902, "s-rogue9999@n-rogue", "note", "again").starts_with("OK ")
        );
    }

    #[test]
    fn an_ack_releases_the_senders_quota() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        for off in 1..=SENDER_QUOTA as u64 {
            assert!(deliver(&store, &sid, off, "s-p@n-peer", "note", "x").starts_with("OK "));
        }
        assert_eq!(
            deliver(&store, &sid, 100, "s-p@n-peer", "note", "x"),
            "ERR quota\n",
            "the quota is full"
        );
        cmd_inbox(&ctx, "--peek");
        assert_eq!(
            deliver(&store, &sid, 101, "s-p@n-peer", "note", "x"),
            "ERR quota\n",
            "`--peek` must not relieve the quota"
        );
        assert_eq!(
            cmd_inbox_seen(&ctx, &format!("{SENDER_QUOTA} handled")),
            format!("OK seen={SENDER_QUOTA}\n")
        );
        assert!(
            deliver(&store, &sid, 102, "s-p@n-peer", "note", "x").starts_with("OK "),
            "an acknowledged row must stop counting against its sender"
        );
        let _ = sid;
    }

    #[test]
    fn a_bridge_truncated_body_says_so_in_the_row_and_in_inbox_get() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert_eq!(
            cmd_deliver(
                &store,
                &format!("{sid} off=1 from=s-a@n-b kind=report trust=agent len=200000 text=cut")
            ),
            "OK 1\n"
        );
        let row = rows(&cmd_inbox(&ctx, "--peek"))[0].to_string();
        assert!(row.contains(" len=200000"), "{row}");
        assert!(
            row.contains(" truncated=1"),
            "a cut row must say the endpoint never received the rest: {row}"
        );
        assert!(
            row.contains(" more=1"),
            "a cut row is never the whole of the message, however short: {row}"
        );
        assert_eq!(
            cmd_inbox_get(&ctx, "1"),
            "OK 3 truncated=1 len=200000\ncut",
            "`inbox get` must not answer a cut body under a complete body's header"
        );

        assert_eq!(
            cmd_deliver(
                &store,
                &format!("{sid} off=2 from=s-a@n-b kind=report trust=agent text=whole")
            ),
            "OK 2\n"
        );
        assert_eq!(cmd_inbox_get(&ctx, "2"), "OK 5\nwhole");
        let row2 = rows(&cmd_inbox(&ctx, "--peek"))[1].to_string();
        assert!(!row2.contains("truncated"), "{row2}");
    }

    #[test]
    fn a_post_refused_for_want_of_a_link_says_it_is_still_queued() {
        with_link(|| {
            let store = new_store();
            let (_sid, ctx) = registered(&store);

            assert_eq!(
                cmd_post(&ctx, "to=@s-peer kind=ask --wait=0 no fabric?", None),
                "ERR fabric absent id=1 no-bridge=1\n"
            );

            note_bridge_supervised();
            assert_eq!(
                cmd_post(&ctx, "to=@s-peer kind=ask --wait=0 which branch?", None),
                "ERR fabric absent id=2 queued=1\n"
            );

            let drained = cmd_outbox(&store, "");
            assert!(drained.contains("no fabric?"), "{drained}");
            assert!(drained.contains("which branch?"), "{drained}");
        });
    }

    #[test]
    fn one_outbox_drain_is_bounded_in_bytes_across_every_session() {
        let store = new_store();
        let body = vec![b'x'; BODY_MAX];
        for local in 1..=3u64 {
            let (_sid, ctx) = registered_as(&store, local);
            for _ in 0..(OUTBOX_BYTES_MAX / BODY_MAX) {
                assert!(
                    cmd_post(&ctx, "to=@s-peer kind=note", Some(body.clone())).starts_with("OK "),
                    "the per-session queue takes its own {OUTBOX_BYTES_MAX} bytes"
                );
            }
        }
        let drained = cmd_outbox(&store, "");
        assert!(
            drained.len() <= OUTBOX_DRAIN_BYTES_MAX + BODY_MAX + 256,
            "one drain answered {} bytes: the budget is the aggregate plus at most \
             the one post that crossed it",
            drained.len()
        );
        assert!(
            drained.len() > OUTBOX_DRAIN_BYTES_MAX / 2,
            "the budget must still hand the bridge a useful batch"
        );
        assert!(
            says_more(&drained),
            "a cut walk says so, last, so the bridge does not take it for the whole queue"
        );
        let again = cmd_outbox(&store, "");
        assert_eq!(
            again.len(),
            drained.len(),
            "a bounded drain retires nothing"
        );
    }

    #[test]
    fn a_walk_stopped_at_the_budget_with_nothing_left_says_no_more() {
        let store = new_store();
        let body = vec![b'x'; BODY_MAX];
        let budget = OUTBOX_DRAIN_BYTES_MAX / BODY_MAX;
        let (_a, ctx_a) = registered_as(&store, 1);
        let (_b, ctx_b) = registered_as(&store, 2);
        for _ in 1..budget {
            assert!(
                cmd_post(&ctx_a, "to=@s-peer kind=note", Some(body.clone())).starts_with("OK ")
            );
        }
        assert!(cmd_post(&ctx_b, "to=@s-peer kind=note", Some(body.clone())).starts_with("OK "));
        let whole = cmd_outbox(&store, "");
        let (_, posts) = parse_outbox(&whole);
        assert_eq!(posts.len(), budget);
        assert!(!says_more(&whole), "every post listed");
        assert!(cmd_post(&ctx_a, "to=h-x kind=note after", None).starts_with("OK "));
        let cut = cmd_outbox(&store, "");
        let (_, posts) = parse_outbox(&cut);
        assert_eq!(posts.len(), budget, "the budget binds before `after`");
        assert!(
            says_more(&cut),
            "the control: a post past the budget is unlisted"
        );
    }

    #[test]
    fn a_full_queue_ahead_in_drain_order_does_not_hide_a_later_sessions_post() {
        let store = new_store();
        let body = vec![b'x'; BODY_MAX];
        let (sid_a, ctx_a) = registered_as(&store, 1);
        for _ in 0..(OUTBOX_BYTES_MAX / BODY_MAX) {
            assert!(
                cmd_post(&ctx_a, "to=@s-peer kind=note", Some(body.clone())).starts_with("OK "),
                "A's queue takes its own {OUTBOX_BYTES_MAX} bytes"
            );
        }
        let (sid_b, ctx_b) = registered_as(&store, 2);
        assert_eq!(cmd_post(&ctx_b, "to=h-x kind=note from-b", None), "OK 1\n");
        let (_, posts) = parse_outbox(&cmd_outbox(&store, ""));
        assert!(
            posts
                .iter()
                .any(|(line, body)| line.contains(&format!("sid={sid_b} ")) && body == "from-b"),
            "B's post is listed although A's queue alone fills the budget"
        );
        let ids: Vec<u64> = posts
            .iter()
            .filter(|(line, _)| line.contains(&format!("sid={sid_a} ")))
            .map(|(line, _)| {
                line.split_whitespace()
                    .find_map(|t| t.strip_prefix("id="))
                    .and_then(|n| n.parse().ok())
                    .expect("id=")
            })
            .collect();
        assert!(!ids.is_empty(), "A's head post is listed too");
        assert!(
            ids.windows(2).all(|w| w[1] == w[0] + 1) && ids[0] == 1,
            "A's posts in its own queue order, from its head: {ids:?}"
        );
    }

    #[test]
    fn repeated_outbox_peeks_do_not_starve_a_session_past_the_byte_budget() {
        let store = new_store();
        let body = vec![b'x'; BODY_MAX];
        let mut queued = Vec::new();
        for local in 1..=16 {
            let (sid, ctx) = registered_as(&store, local);
            assert_eq!(
                cmd_post(&ctx, "to=h-x kind=note", Some(body.clone())),
                "OK 1\n"
            );
            queued.push((sid, ctx));
        }
        let (last, ctx) = registered_as(&store, 17);
        assert_eq!(cmd_post(&ctx, "to=h-x kind=note allowed", None), "OK 1\n");
        queued.push((last.clone(), ctx));

        let mut exposed = BTreeSet::new();
        for _ in 0..2 {
            let drain = cmd_outbox(&store, "");
            assert!(drain.len() <= OUTBOX_DRAIN_BYTES_MAX + BODY_MAX + 4096);
            let (_, posts) = parse_outbox(&drain);
            for (line, _) in posts {
                let sid = line
                    .split_whitespace()
                    .find_map(|word| word.strip_prefix("sid="))
                    .expect("post sid=");
                exposed.insert(sid.to_string());
            }
        }
        assert!(
            exposed.contains(&last.to_string()) && exposed.len() == queued.len(),
            "every sender is exposed although the earlier heads stay queued: {exposed:?}"
        );
        for (_, ctx) in queued {
            let inbox = ctx.fabric.lock();
            assert_eq!(inbox.posts.len(), 1, "a peek retires no post");
            assert_eq!(inbox.posts[0].id, 1);
            assert!(inbox.posts[0].off.is_none() && !inbox.posts[0].dead);
        }
    }
}
