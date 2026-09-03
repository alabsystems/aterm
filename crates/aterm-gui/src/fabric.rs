// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FABRIC ENDPOINT — the per-session inbox ring, the outbound post queue, and
//! the hold gate. STATE, not I/O: nothing in this module opens a socket, speaks to
//! a broker, or knows a subject exists. The bus lives outside the process, in the
//! `aterm-link serve` child the instance launches; this is the endpoint it delivers
//! INTO and drains posts OUT of, over the ordinary control protocol.
//!
//! Three things live here and nothing else does:
//!
//! * [`SessionFabric`] — one per [`crate::SessionCtx`]: a bounded inbox ring, this
//!   session's un-landed posts, the two watermarks, and the hold flag. A LEAF lock
//!   like `meta`/`timeline`, with exactly one sanctioned nesting (fabric →
//!   timeline, never reversed) so an events watcher can never see two concurrent
//!   `deliver`s in the opposite order from the ring.
//! * The verb handlers `inbox` / `inbox get` / `inbox seen` / `post` / `deliver` /
//!   `hold`, and the `await inbox` predicate. They are plain functions over a
//!   `SessionCtx` (and, for the two bridge verbs, the registry), so every one of
//!   them is directly testable without an event loop.
//! * `LINK` — the INSTANCE's view of its bridge: connected, disconnected, or
//!   never launched, plus the set of sessions that bridge has ever touched.
//!
//! ## Why the two bridge verbs are not Owner verbs
//!
//! `deliver` stamps `from=` and `trust=` on a row an agent reads as an attested
//! human order; `hold` lifts a fleet halt. Owner scope is what every in-session
//! client already holds (`aterm-ctl @self` is Owner), so an Owner classification
//! would put both inside the blast radius of one prompt injection. They are
//! [`aterm_types::control_verbs::Access::BridgeOnly`] instead: the only caller is
//! the connection the instance handed its own child, and there is no token that
//! opens it. See `Scope::Bridge`.
//!
//! ## Fail closed on a dead bridge
//!
//! The halt must not depend on a killable process staying alive. When the bridge
//! connection closes — the child exited, was killed, or the fd was closed —
//! [`bridge_lost`] applies `hold on reason=fabric-lost origin=fleet` to every
//! session that bridge ever delivered to or held. Killing the bridge therefore
//! HALTS the sessions it was governing; it does not free them.
//!
//! THE ONLY LIFT IS A RECONNECTING BRIDGE, and that is a narrower promise than
//! this header used to make. [`apply_hold`] has exactly two production callers —
//! [`cmd_hold`], which is `Access::BridgeOnly`, and [`bridge_lost`] — and there
//! is no menu item, key binding, palette action, Owner verb or config path that
//! clears a hold. DESIGN §11.2 and an earlier version of this header both said
//! "a human lifts it at the GUI"; no such path exists, so the sentence is
//! WITHDRAWN rather than left standing as a promise the code does not keep. An
//! operator whose bridge cannot come back (a deleted cap file, a `[fabric]
//! command` that exits at startup) has exactly one recovery, and it is
//! restarting `aterm-gui`. That is a real gap, recorded as one: building the
//! lift is a change to the GUI modules, not to this one.

use std::collections::{BTreeSet, HashSet, VecDeque};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};

use aterm_control::wire::pct_encode;
use aterm_session::SessionId;

use crate::SessionCtx;
use crate::session_store::Store;
use crate::turn_ledger::now_ms;

/// How many message rows one session's inbox retains. Drop-oldest past this, and
/// an eviction of a row the agent has not HANDLED (`inbox seen`) is REPORTED
/// (`dropped=`), never silent — a message that vanished without a trace is worse
/// than one refused.
///
/// UNHANDLED, not merely unlisted. A row listed once at turn start and never
/// acted on is exactly the message whose silent loss matters most (a human's
/// `task` waiting through a long turn), and counting only unlisted rows made
/// that the one case the header never mentioned.
pub(crate) const RING_CAP: usize = 512;

/// Unlisted rows one ATTESTED PEER may hold in a session's ring. The 65th is
/// `ERR quota` at [`cmd_deliver`], so one peer cannot evict a human's unread
/// `task` under a burst of `note`s.
///
/// COUNTED ON THE CAP-FORCED HALF OF `from=`, which is what makes the sentence
/// above true. The whole `from=` string is NOT one identity: for the third form
/// (`s-<sid>@n-<node>`) the `s-<sid>@` prefix is the NODE's word — `render_from`
/// (`aterm-link/src/bridge.rs`) reads it from the record BODY, checking only that
/// it looks like a session principal — while only the part after `@` comes from
/// the delivered SUBJECT, which the broker's grant forces. `is_principal` admits
/// ~36^32 sids, so a quota keyed on the whole string gave ONE rogue node an
/// unbounded number of 64-row allowances: rotate the pseudo-sid every 64 notes
/// and `ERR quota` never fires while the ring turns over continuously. See
/// [`quota_key`]; [`InboxRow::from`] records the same split for the same reason.
///
/// THE PRICE, SAID OUT LOUD: an honest node hosting many sessions now shares one
/// allowance across all of them. 64 UNLISTED rows is generous for that — a bare
/// `inbox` relieves the whole set — and the promise the design makes is about a
/// PEER, so the peer is what has to be counted.
pub(crate) const SENDER_QUOTA: usize = 64;

/// How much of a body an `inbox` row carries inline before `more=1` sends the
/// reader to `inbox get`. Bytes of the DECODED body, measured before encoding.
const TEXT_PREVIEW_MAX: usize = 512;

/// The largest body `deliver`/`post` accept, and the largest `inbox get` returns.
pub(crate) const BODY_MAX: usize = 256 * 1024;

/// The largest INLINE `post` text (the argument form). A longer body arrives as
/// the length-prefixed frame instead.
pub(crate) const POST_INLINE_MAX: usize = 4 * 1024;

/// How many outbound posts one session may hold WAITING for the bridge, and how
/// many bytes of retained body they may hold together.
///
/// TWO BOUNDS BECAUSE THERE ARE TWO WAYS TO OVERFLOW: 128 messages of 4 B, and 16
/// messages of 256 KiB, are the same hazard from opposite directions, and a count
/// alone would let one session pin 32 MiB behind an unreachable broker.
///
/// AND THE OVERFLOW IS REFUSED AT THE DOOR, not evicted. The inbox ring may
/// drop-oldest because every row it drops is still on the log and its loss is
/// reported (`dropped=`); a dropped OUTBOUND message has no record anywhere —
/// the sender was told `OK`, the bus never saw it, and nothing on either side
/// can notice. So `post` answers `ERR outbox full` and the caller still holds
/// its text.
pub(crate) const OUTBOX_CAP: usize = 128;
pub(crate) const OUTBOX_BYTES_MAX: usize = 4 * 1024 * 1024;

/// How many bytes ONE `outbox` drain may answer with, across ALL sessions.
///
/// The two bounds above are per session, so `sessions × OUTBOX_BYTES_MAX` is what
/// a single reply used to cost — three times over, counting the reply copy and
/// the bridge's own buffer for the announced length. This is the aggregate the
/// caller and the endpoint MEET on, and it is enforced where the reply is built
/// so no caller can disagree with it. Sized at one session's full queue: large
/// enough that the common case (a handful of sessions, small bodies) is still one
/// round trip, and the drain is resumed by the bridge's next idle tick because
/// `outbox` retires nothing.
const OUTBOX_DRAIN_BYTES_MAX: usize = OUTBOX_BYTES_MAX;

/// How many RETIRED posts (landed or undeliverable) a session keeps after the
/// bridge is done with them. They carry no body — only (id, to, kind, off) — and
/// they are kept at all because an incoming answer's `re=<offset>` is resolved to
/// `re-id=<post id>` through exactly this table.
const RETIRED_KEEP: usize = 512;

/// How many delivered offsets the ring remembers for idempotency BEYOND the rows
/// it still holds. Twice the ring, so a redelivery of a row the ring has already
/// evicted is still recognized as a duplicate rather than re-appended. A
/// redelivery older than this reappears as a fresh row, which is the honest
/// behaviour: the row was dropped unread and the bus still holds it.
const DEDUP_CAP: usize = 2 * RING_CAP;

/// Bytes of a hold reason kept (pct-encoded on the wire). A halt reason is a line
/// for a human, not a document.
const REASON_MAX: usize = 128;

/// The five session-timeline record kinds the fabric writes, which are ALSO their
/// wire names on the `events` digest (`EVENT <local> inbox <id> …`). One list, read
/// by `subscribe`'s `timeline_wire_kind`, so the recorder and the digest cannot
/// disagree about which rows leave the process. NONE of them carries a body — that
/// is the discipline the digest exists to keep.
pub(crate) const FABRIC_EVENT_KINDS: &[&str] =
    &["inbox", "inbox-seen", "post", "post-landed", "hold"];

/// The `events` wire name for a timeline record kind the fabric wrote, or `None`.
pub(crate) fn fabric_event_wire_kind(kind: &str) -> Option<&'static str> {
    FABRIC_EVENT_KINDS.iter().copied().find(|k| *k == kind)
}

/// The kinds `deliver` accepts, and the `await inbox` vocabulary. Closed: a kind
/// outside this set is refused at `deliver`, so nothing an agent reads carries a
/// kind token the endpoint never classified.
///
/// NOT the set a SENDER may claim — that is [`POSTABLE`], and the difference
/// between the two lists is the whole reason there are two.
const KINDS: &[&str] = &[
    "ask",
    "answer",
    "task",
    "report",
    "note",
    "control",
    "ack",
    "expired",
    "undeliverable",
];

/// The kinds a SENDER may claim on `post`: [`KINDS`] minus the two a bridge
/// RECORDS on a sender's behalf.
///
/// TWO LISTS BECAUSE THERE ARE TWO QUESTIONS. `deliver` must accept everything
/// that can arrive off the bus, `expired` and `undeliverable` included, because
/// the asker's OWN bridge writes them (§6.4: when the asker's bridge sees the
/// deadline pass with no answer it records the verdict as `kind=expired re=R`).
/// `post` must accept only what a principal is entitled to ASSERT. A forged
/// `post to=@s-peer kind=expired re=<R>` is byte-identical in shape to the
/// verdict a bridge records, and nothing downstream can tell them apart: the
/// broker's `in` write grant ends in `*`, `drain_outbox` appends the kind
/// straight onto the subject, `parse_in` accepts it, and `classify_kind` demotes
/// only `task` and `control` — so it lands in a peer's inbox undemoted, as
/// itself, and the peer abandons a request that is still in flight.
///
/// The file plane already had this right, and said why (`POSTABLE` in
/// `aterm-link/src/mirror.rs`: "`expired` and `undeliverable` are verdicts a
/// bridge records, never something a sender may claim"); this is the socket
/// plane's half of the same rule, and it is what [`POST_USAGE`] and the `post`
/// verb row have said all along. `post_may_not_claim_a_verdict_a_bridge_records`
/// pins the difference, so a kind added to `deliver` cannot silently become
/// postable.
const POSTABLE: &[&str] = &["ask", "answer", "task", "report", "note", "ack", "control"];

/// The trust labels the RECEIVING bridge computes (§4.3) — the endpoint stores
/// what it is told on a `BridgeOnly` line and refuses anything else, so an
/// unknown label can never reach an agent's screen as if it meant something.
const TRUSTS: &[&str] = &["human", "agent", "relayed", "screen"];

/// The kinds `await inbox` latches on when the caller lists none: everything but
/// `note`. A note is the class that must never wake anybody by itself.
const AWAIT_DEFAULT_SKIP: &str = "note";

/// One delivered message. Every field is a wire token the `inbox` row prints
/// verbatim; free text is pct-encoded ONCE, at delivery, so the listing never
/// re-encodes and the body `inbox get` returns is the decoded original.
#[derive(Clone, Debug)]
pub(crate) struct InboxRow {
    /// Per-session monotone row id (1-based) — the `since=`/`inbox seen` key.
    pub id: u64,
    /// The broker offset this record landed at: dense, unique, durable, and
    /// assigned by the log, which is what makes it a correlation id nobody can
    /// forge, and the idempotency key `deliver` dedups on.
    pub off: u64,
    /// Milliseconds on the process clock ([`now_ms`]) at delivery.
    pub t_ms: u64,
    /// The sender, as the delivering bridge renders it: `h-andrew`,
    /// `s-<sid>@n-<node>`, `a-<service>`.
    ///
    /// CAP-FORCED, WITH ONE ATTESTED EXCEPTION — and the exception is in the form
    /// an agent reads most. The `<src>` segment comes from the delivered SUBJECT,
    /// which the broker's grant forces, so no sender chooses it. The `s-<sid>@`
    /// PREFIX of the third form does not: `Bridge::render_from`
    /// (`aterm-link/src/bridge.rs`) reads it from the record BODY's `from=<sid>`
    /// token, which `aterm-link/src/body.rs` names "the one attested exception".
    /// It is allowed because the node could type as any session it hosts anyway,
    /// so claiming to is no escalation (§8.3) — but it means `s-worker-3@n-lab`
    /// is NODE `n-lab`'s word for which of its sessions spoke, trustworthy
    /// exactly as far as that node's uid is (§9.3 T1), and not the broker's word.
    /// Do not read the whole of this field as if all of it were forced.
    pub from: String,
    /// One of [`KINDS`].
    pub kind: String,
    /// One of [`TRUSTS`] — the receiver's verdict on what the content IS.
    pub trust: String,
    /// The offset this message answers, when it answers one.
    pub re: Option<u64>,
    /// The local post id `re` resolves to through the post table, when the answer
    /// is to something this session sent.
    pub re_id: Option<u64>,
    /// Advisory deadline in ms.
    pub dl: Option<u64>,
    /// The answer arrived after the deadline had already been recorded expired.
    pub late: bool,
    /// The kind this message CLAIMED before the receiver demoted it (a relayed
    /// row is always `note`, whatever it says it is).
    pub demoted: Option<String>,
    /// The relay chain, comma-joined, when the message came through one.
    pub via: Option<String>,
    /// Body length in bytes, before truncation.
    pub len: usize,
    /// The body, as delivered (pct-DECODED, so already lossy-valid UTF-8).
    pub body: String,
    /// Whether a non-peek `inbox` has listed this row. Unlisted rows are what the
    /// quota counts and what an eviction reports.
    pub listed: bool,
}

impl InboxRow {
    /// Whether this row's sender is a HUMAN principal. Eviction never drops one of
    /// these ahead of an agent's row: a burst from a peer must not be able to push
    /// out the one message a person sent.
    fn is_human(&self) -> bool {
        self.from.starts_with("h-")
    }
}

/// One outbound message this session sent. It stays listed until the bridge
/// reports the offset it landed at, so an agent sees at turn start what is still
/// in flight rather than assuming.
#[derive(Clone, Debug)]
pub(crate) struct PostRow {
    /// Per-session monotone post id (1-based), the `--wait` and `landed=` key.
    pub id: u64,
    /// The address as typed, pct-encoded.
    pub to: String,
    /// One of [`KINDS`].
    pub kind: String,
    /// The offset the record landed at, once the bridge says so.
    pub off: Option<u64>,
    /// The offset this post ANSWERS, carried through to the bridge so the
    /// published record's body can say `re=`.
    pub re: Option<u64>,
    /// Advisory deadline in ms, carried through the same way.
    pub dl: Option<u64>,
    /// The relay chain, when the sender declared one.
    pub via: Option<String>,
    /// Retired as permanently undeliverable (`outbox sent … off=-`). A dead post
    /// is neither queued nor landed: it releases its `--wait` with an error and
    /// stops appearing in `inbox`'s in-flight rows.
    pub dead: bool,
    /// WHY it is dead, from the bridge's `reason=` (default `undeliverable`).
    ///
    /// §6.1 gives the two-claimant case its own verdict — "a second node
    /// advertising a pinned sid makes `post` answer `ERR ambiguous`" — and the
    /// endpoint cannot compute it: routing is the bridge's knowledge, and the
    /// sender is parked in `post --wait` here. So the bridge names the verdict
    /// on the retirement and this is where it is kept until the wait wakes. The
    /// remedies differ, which is why one word is worth carrying: a bad address
    /// is the sender's mistake, a contested sid is a fleet problem.
    pub dead_reason: String,
    /// The message body, kept ONLY until the bridge retires the post.
    ///
    /// The bridge reads it with `outbox` — a length-prefixed, BridgeOnly peek —
    /// and drops it here with `outbox sent`. That pair is the outbound mirror of
    /// `deliver`/`inbox seen`, and the retention bound is the reason the pair
    /// exists at all: a body held forever behind an unreachable broker is the
    /// "unbounded wait fixed into an unbounded buffer" trade this codebase keeps
    /// refusing. `len()` is reported on the `post` row so the bytes are visible.
    pub body: String,
}

/// The standing halt on one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hold {
    /// Pct-encoded reason, or `-`.
    pub reason: String,
    /// `fleet` (a human's halt, or a lost bridge) or `local`.
    pub origin: String,
}

/// The mutable half of [`SessionFabric`], behind one leaf mutex.
#[derive(Default)]
struct Inbox {
    rows: VecDeque<InboxRow>,
    posts: VecDeque<PostRow>,
    next_msg_id: u64,
    next_post_id: u64,
    /// The HANDLED watermark: moved only by `inbox seen`.
    ///
    /// The LISTED state is NOT a watermark — it is [`InboxRow::listed`], per row.
    /// There used to be a `listed: u64` here as well, read by nothing but
    /// `pending=`, and the two disagreed the moment a reply was bounded or
    /// `since=`-filtered: the field advanced to the highest row the reply KEPT
    /// while the older rows it skipped stayed unlisted, so `pending=` reported 0
    /// over unread mail. One value, one copy.
    seen: u64,
    /// UNHANDLED rows the ring has evicted, ever — every evicted row above the
    /// `seen` watermark, not merely one nobody had listed. A row listed once at
    /// turn start and never acted on is exactly the loss that matters most (a
    /// human's `task` waiting through a long turn), and while this counter said
    /// "unlisted" that was the one case it stayed silent for. [`RING_CAP`] and
    /// [`evict_for`] state the same rule; this field used to state the older one.
    ///
    /// Monotone: `inbox` reports it, so a drop is always visible to the next
    /// drain even if the agent missed the one it happened on.
    dropped: u64,
    /// The highest offset ever delivered here.
    bus_head: u64,
    /// Offsets already delivered, newest last, capped at [`DEDUP_CAP`].
    dedup: VecDeque<u64>,
    dedup_set: HashSet<u64>,
    hold: Option<Hold>,
}

impl Inbox {
    /// The outbound load `post` is refused against: how many posts still wait for
    /// the bridge, and how many bytes of body they hold between them.
    fn queued_load(&self) -> (usize, usize) {
        self.posts
            .iter()
            .filter(|p| p.off.is_none() && !p.dead)
            .fold((0, 0), |(n, b), p| (n + 1, b + p.body.len()))
    }

    /// Drop the oldest RETIRED posts past [`RETIRED_KEEP`]. A post still waiting
    /// for the bridge is never dropped here — that is what the refusal at the door
    /// is for — so this can only ever discard a bodiless (id, to, kind, off) row
    /// whose only remaining use is resolving an old answer's `re-id=`.
    fn trim_retired_posts(&mut self) {
        let retired = self
            .posts
            .iter()
            .filter(|p| p.off.is_some() || p.dead)
            .count();
        let Some(mut over) = retired.checked_sub(RETIRED_KEEP).filter(|n| *n > 0) else {
            return;
        };
        // Oldest first, and ONLY retired rows: a post still in flight keeps its
        // place however old it is, so a stuck bridge can never make the endpoint
        // forget a message it has not yet handed over.
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

/// The per-session fabric state, hung on [`crate::SessionCtx`].
///
/// LOCK DISCIPLINE. `inbox` is a LEAF, with exactly one sanctioned nesting:
/// fabric → `ctx.timeline`, never reversed, so two concurrent `deliver`s cannot
/// append to the ring in one order and record their `EVENT`s in the other. The
/// condvar is signalled under the same guard, which is what makes `await inbox`
/// and `post --wait` event-driven instead of polled — no sleep anywhere in this
/// module.
#[derive(Default)]
pub(crate) struct SessionFabric {
    inbox: Mutex<Inbox>,
    /// Signalled on every state change a parked waiter could care about: a new
    /// row, a post landing, a hold transition.
    changed: Condvar,
    /// A6: the per-producer high-water marks that make the PTY seam
    /// exactly-once. It lives here for the same reason the ring does — one copy
    /// per session, reachable from the control thread without a store lock — and
    /// it is a SEPARATE lock from `inbox`, never nested with it: the mailbox and
    /// the keyboard seam share a session and nothing else.
    idem: crate::pty_idem::PtyIdem,
}

impl SessionFabric {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inbox> {
        self.inbox.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// This session's PTY idempotency marks (A6).
    pub(crate) fn idem(&self) -> &crate::pty_idem::PtyIdem {
        &self.idem
    }

    /// The session's standing halt, if any — the one read `status` and the
    /// dispatch gate both take.
    pub(crate) fn hold(&self) -> Option<Hold> {
        self.lock().hold.clone()
    }
}

// ---------------------------------------------------------------------------
// The instance's bridge link
// ---------------------------------------------------------------------------

/// `fabric=` states, in the order [`FabricLink::state`] encodes them.
const FABRIC_ABSENT: u8 = 0;
const FABRIC_CONNECTED: u8 = 1;
const FABRIC_DISCONNECTED: u8 = 2;

/// The instance's view of its bridge. Process-global because the bridge is
/// per-INSTANCE, not per-session: one child, one pair of inherited fds, whatever
/// number of sessions it hosts.
struct FabricLink {
    /// The published `fabric=` token, kept as an atomic so [`fabric_state`] is a
    /// cheap read from any thread. WRITTEN ONLY under `generation` — see
    /// [`bridge_lost`] for why the compare and the store have to be one step.
    state: AtomicU8,
    /// The bridge incarnation `state` currently speaks for, or `0` before any
    /// bridge has attached. A `Mutex` and not an atomic because the only two
    /// writers must each read-compare-and-store WITHOUT another attach landing
    /// in between; a pair of atomics would leave exactly the one-instruction
    /// window this field exists to close.
    generation: Mutex<u64>,
    /// Sids the bridge has delivered to or held. This is the set [`bridge_lost`]
    /// halts, and it grows only through the `BridgeOnly` verbs, so a session the
    /// bridge never governed is never halted by its death.
    ///
    /// A `BTreeSet` AND PRUNED, because neither half of the old justification
    /// held. It was a `Vec` scanned linearly, argued as "a handful of sids on a
    /// path that runs at most once per bridge verb" — but `deliver` is a bridge
    /// verb, so the scan ran once per INBOUND MESSAGE, over every sid the bridge
    /// had ever governed since process start, and nothing ever removed one: a
    /// session that exited stayed in the vector for the life of the process, and
    /// `bridge_lost` then did a registry lookup per dead sid on every relaunch.
    /// `BTreeSet::new` is const (which is what the `Vec` was chosen for), the
    /// membership test is O(log n), and [`note_bridge_touched`] prunes the set
    /// against the live registry once it passes [`TOUCHED_PRUNE_AT`] — so its
    /// size is bounded by the live sessions plus that slack, not by uptime.
    touched: Mutex<BTreeSet<String>>,
}

static LINK: FabricLink = FabricLink {
    state: AtomicU8::new(FABRIC_ABSENT),
    generation: Mutex::new(0),
    touched: Mutex::new(BTreeSet::new()),
};

/// How many sids [`FabricLink::touched`] may hold before the next insert prunes
/// the ones whose sessions have left the registry. A slack bound, not a cap: the
/// prune is amortized O(1) per insert (one registry snapshot per `n` inserts),
/// and a sid is only ever dropped when its session is gone — a departed session
/// cannot come back, so a pruned entry can never need halting again.
const TOUCHED_PRUNE_AT: usize = 1024;

/// ONE BRIDGE INCARNATION — a LAUNCH, not a lane.
///
/// THE PROBLEM IT SOLVES. `bridge_attached` is the only writer of
/// `fabric=connected` and [`bridge_lost`] the only writer of
/// `fabric=disconnected`, and the two are UNORDERED across a relaunch: two
/// [`crate::control::BridgeLostGuard`]s exist per bridge (the verb lane and the
/// push lane), the verb lane sees EOF on its next read while the push lane sees
/// it only on the subscribe loop's 250 ms peer probe, and the supervisor
/// relaunches after 200 ms. So the replacement can be attached and CONNECTED
/// before the previous incarnation's second guard has finished unwinding, and
/// that guard then stored `disconnected` over a live bridge — permanently, since
/// nothing re-asserts the state. `status fabric=` lied for the life of the
/// process and, worse, `post --wait` (ON by default for `ask` and `task`)
/// short-circuited with `ERR fabric disconnected` for every message the live
/// bridge went on to publish.
///
/// This is the link-state analogue of the hold's `converge_hold`/`reconcile_halt`
/// (`aterm-link/src/bridge.rs`), which fixed the same interleaving for the HOLD
/// half only. A generation is the cheaper answer here because, unlike the hold,
/// `fabric=` has no external source of truth to reconcile against.
///
/// A LAUNCH AND NOT A LANE, deliberately. `launch_once` mints one and hands the
/// SAME value to both near ends, so the two guards of one incarnation stay
/// indistinguishable — either fd closing still reports the link lost, which is
/// §11.2's rule — while a guard from a PREVIOUS incarnation cannot report a live
/// replacement disconnected.
///
/// THE HOLD SWEEP STAYS UNCONDITIONAL. Only the STATE is generation-checked: a
/// halt that a `kill -9` and a lucky interleaving together lift is not a halt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct BridgeGeneration(u64);

/// Mint the identity of one bridge incarnation. Called ONCE per launch, before
/// either near end is served. Never `0`, which is reserved for "no bridge has
/// ever attached" so a zero-valued guard cannot match a fresh link.
pub(crate) fn next_bridge_generation() -> BridgeGeneration {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    BridgeGeneration(NEXT.fetch_add(1, Ordering::Relaxed).wrapping_add(1))
}

/// The `status` reply's `fabric=` token.
pub(crate) fn fabric_state() -> &'static str {
    match LINK.state.load(Ordering::Relaxed) {
        FABRIC_CONNECTED => "connected",
        FABRIC_DISCONNECTED => "disconnected",
        _ => "absent",
    }
}

/// A bridge connection has been served. Called from the control server as it
/// starts serving an inherited socketpair end with `Scope::Bridge`, once per
/// lane, with the [`BridgeGeneration`] its launch minted.
///
/// Takes `generation` UNDER THE LOCK together with the state store, so a stale
/// guard's compare in [`bridge_lost`] cannot straddle this and clobber it.
///
/// MONOTONE, because the ATTACHES are as unordered as the guards were.
/// `attach_fabric_bridge` hands each near end to a freshly spawned thread and
/// each thread calls this itself, so launch N's second lane can run AFTER launch
/// N+1's first. A last-writer-wins store let that regress `owner` to N; the ghost
/// lane's own `BridgeLostGuard` then matched, stored `disconnected` over the live
/// bridge N+1, and nothing ever re-asserted it — `status fabric=` lied for the
/// life of the process and every `post --wait` (ON by default for `ask`/`task`)
/// short-circuited, which is the exact outcome [`BridgeGeneration`] exists to
/// prevent. So an OLDER OR EQUAL generation is ignored outright.
///
/// STRICTLY GREATER, and equal is ignored ON PURPOSE. Both lanes of one launch
/// carry the same generation; the first stores `connected`, and the second must
/// not be able to store it AGAIN after its sibling's death already reported the
/// link lost. Either fd closing means the link is gone (§11.2), and a halt a
/// racing attach lifts is not a halt.
pub(crate) fn bridge_attached(generation: BridgeGeneration) {
    let mut owner = LINK.generation.lock().unwrap_or_else(|p| p.into_inner());
    if generation.0 <= *owner {
        return;
    }
    *owner = generation.0;
    LINK.state.store(FABRIC_CONNECTED, Ordering::Relaxed);
}

/// Record that the bridge has governed this session — the membership test
/// [`bridge_lost`] halts on.
///
/// THREE CALL SITES, ACROSS FOUR `BridgeOnly` VERBS: `deliver` (both forms),
/// `hold` and `outbox sent`. `outbox` is deliberately NOT one — a PEEK governs
/// nothing, and a bridge that only ever read a session's queue has not taken
/// responsibility for halting it.
///
/// PRUNES ITSELF. Past [`TOUCHED_PRUNE_AT`] entries the set is intersected with
/// the live registry, so sids whose sessions have exited stop being carried for
/// the life of the process. The registry snapshot is taken with the `touched`
/// lock RELEASED and re-acquired afterwards: `bridge_lost` reads `touched` and
/// then the store, so taking them in the other order here is the one shape that
/// could deadlock, and it is structurally avoided rather than argued about.
fn note_bridge_touched(store: &Store, sid: &str) {
    let over = {
        let mut touched = LINK.touched.lock().unwrap_or_else(|p| p.into_inner());
        touched.insert(sid.to_string());
        touched.len() > TOUCHED_PRUNE_AT
    };
    if !over {
        return;
    }
    let live: BTreeSet<String> = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.live_handles()
            .map(|h| h.sid.as_str().to_string())
            .collect()
    };
    let mut touched = LINK.touched.lock().unwrap_or_else(|p| p.into_inner());
    touched.retain(|s| live.contains(s));
}

/// THE SAFETY PROPERTY. Either bridge fd closed, so the halt this instance was
/// enforcing on the fleet's behalf can no longer be refreshed, lifted, or even
/// observed. Hold every session the bridge ever touched with `reason=fabric-lost
/// origin=fleet` and report `fabric=disconnected`.
///
/// Fail CLOSED. THE HOLD SWEEP IS UNCONDITIONAL: a halt that a `kill -9` lifts
/// is not a halt, so it does not ask whether a hold was standing, whether the
/// bridge exited cleanly, or whether anything is coming back. A reconnecting
/// bridge lifts what it wants lifted with `hold off`. NOTHING ELSE LIFTS IT —
/// see the module header; the "a human lifts it at the GUI" this doc used to end
/// with names a path that does not exist.
///
/// THE STATE IS GENERATION-CHECKED, and only the state. `generation` is the
/// incarnation this guard was created under; if a LATER incarnation has attached
/// since, this guard is a ghost of a bridge that is already gone and must not
/// report the live one disconnected. See [`BridgeGeneration`].
pub(crate) fn bridge_lost(store: &Store, generation: BridgeGeneration) -> usize {
    {
        // Compare and store under the same lock `bridge_attached` writes under:
        // a read-then-store pair would leave a window in which a newer attach
        // lands between the two and is clobbered anyway.
        let owner = LINK.generation.lock().unwrap_or_else(|p| p.into_inner());
        if *owner == generation.0 {
            LINK.state.store(FABRIC_DISCONNECTED, Ordering::Relaxed);
        }
    }
    let sids: Vec<String> = LINK
        .touched
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .cloned()
        .collect();
    let mut held = 0usize;
    for sid in sids {
        // Clone the ctx Arc out under the store guard; never record a timeline
        // event while holding the registry.
        let ctx = {
            let g = store.read().unwrap_or_else(|p| p.into_inner());
            g.by_sid(&SessionId::new(sid.clone()))
                .map(|h| h.ctx.clone())
        };
        if let Some(ctx) = ctx {
            apply_hold(
                &ctx,
                Some(Hold {
                    reason: "fabric-lost".to_string(),
                    origin: "fleet".to_string(),
                }),
            );
            held += 1;
        }
    }
    // WAKE EVERY REGISTERED SESSION, not only the governed ones. A session that
    // has only ever POSTED is not in `touched`, so the sweep above never touches
    // it — and its `post --wait` is parked on THIS session's condvar with no
    // other reason to wake. Without this it sits out its whole wait (up to
    // `WAIT_MAX_MS`) and answers `ERR timeout` for a landing nothing could ever
    // have reported, which is the exact outcome `cmd_post`'s entry check
    // (§11.2 deviation 9) exists to avoid. The waiter re-reads `fabric_state()`
    // on each wake and answers `ERR fabric disconnected id=<n> queued=1` instead
    // — see [`fabric_wait_refusal`] for why the queued token is not decoration.
    //
    // Ctxs are cloned out from under the registry guard first: the fabric lock is
    // a leaf and is never taken while the store is held.
    let parked: Vec<_> = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.live_handles().map(|h| h.ctx.clone()).collect()
    };
    for ctx in &parked {
        // Signalled UNDER the session's own fabric lock, and after the state
        // store above. A `post --wait` reads `fabric_state()` while holding that
        // lock and releases it only inside `wait_timeout`, so mutual exclusion
        // gives the whole property: either this lock waits until the waiter is
        // parked (and the signal reaches it), or it wins the lock first — in
        // which case the waiter's next read of `fabric_state()` already sees
        // `disconnected`. A `notify_all` outside the lock is the classic lost
        // wakeup, and losing this one costs the caller its entire `--wait`.
        let guard = ctx.fabric.lock();
        ctx.fabric.changed.notify_all();
        drop(guard);
    }
    held
}

// ---------------------------------------------------------------------------
// The hold gate
// ---------------------------------------------------------------------------

/// The verbs a fleet halt refuses (§5.3): every socket verb that can reach the
/// PTY, signal the child, or retire a LIVE session.
///
/// A LITERAL, BUT NOT A HAND-MAINTAINED ONE. It has to be a literal, because the
/// answer must be available before a session is resolved — but it is CHECKED
/// against the verb table it claims to summarise, by a test that carries the
/// name it is cited under: [`tests::the_halt_set_is_derived_from_the_verb_table`]
/// walks EVERY [`aterm_types::control_verbs::VERBS`] row, whatever its
/// [`aterm_types::control_verbs::Target`], and fails unless each row that is not
/// `OpClass::Read` is either in this set or in that test's `HALT_EXEMPT` list of
/// NAMED, argued exemptions. Its `match` on `OpClass` is exhaustive, so a new op
/// class fails to COMPILE rather than slipping past a filter.
///
/// TWO EARLIER VERSIONS OF THIS PARAGRAPH WERE WRONG, in the two ways that
/// matter. The first said "EXACTLY this list" and was checked only against
/// another hand-written list in the same file, so the two agreed with each other
/// and both disagreed with the engine: `focus` writes `\x1b[I` / `\x1b[O` to the
/// PTY whenever DEC 1004 focus reporting is on (`input.rs`, the "SOLE
/// focus-report egress") and was absent, so a fleet halt did not stop it. The
/// second cited a test name that no test carried, and the derivation that did
/// exist skipped every row whose target was not a SESSION — so `tab close`, the
/// App-lane twin of `close`, retired a halted session while `close` on the same
/// connection answered `ERR halted`. Both holes were a SET trusted because a
/// check was said to derive it. The check now exists, under that name, and walks
/// the whole table.
///
/// EXEMPT ON PURPOSE: `post`, `inbox seen`, `meta set`, `lease` and every read
/// verb, because a halted agent must still be able to ask why it is halted, mark
/// the notice seen, and escalate; and the physical keyboard is not on this seam
/// at all, so a human at the glass keeps typing. A halt stops DRIVERS.
///
/// THREE MEMBERS ARE NOT `Target::Session` ROWS, and each has its own seam.
/// `operator-propose-bin` is Owner-only `Meta` and gates at its proposal frame.
/// `hwkey` is `Target::App` too, and it is the most direct PTY-reaching verb in the
/// table: it posts a real NSEvent onto the OS event queue, so it takes the SAME winit
/// path a physical keypress takes. A halt that stopped `send` and `key` but let `hwkey`
/// through would stop the polite routes and leave the one indistinguishable from
/// fingers on the keyboard.
/// `invoke` and `tab` are `Target::App`: they are answered in
/// `dispatch_before_session`, before any session exists, so the session gate is
/// structurally unreachable for their bare forms and [`app_halt_refusal`] is
/// where they are refused instead.
///
/// * `invoke` belongs here because `invoke Paste` writes the OS clipboard into
///   the front tab's PTY through `App::paste_clipboard` ->
///   `deliver_paste(.., Source::Human)`, and because `invoke SelectAll` + `copy`
///   lets the caller CHOOSE those bytes off the session's own screen first — a
///   screen-content-to-PTY path, which is the one thing this design says must
///   not exist.
/// * `tab` belongs here for the THIRD clause of the sentence above: `tab close
///   [N]` reaches `control_input::cmd_tab` -> `Wake::TabCmd` ->
///   `App::close_tab_via_verb`, which retires the tab's session with
///   `ExitReason::CtlClose` — the same act `close` is refused for. The WHOLE
///   verb is refused rather than the `close` sub-form, because a set keyed on
///   the verb NAME and a gate keyed on the ARGUMENTS is exactly the pair of
///   literals that disagreed before. The cost is that a driver cannot navigate
///   tabs over the socket while a halt stands — `app_halt_refusal`'s own "fail
///   closed, and cheap in the case it exists for" argument — and the human at
///   the glass switches tabs from the keyboard as always.
///
/// TWO RESIDUALS, NAMED RATHER THAN PROMISED AWAY. Neither is in this set and
/// neither is refused, so the sentence at the top is scoped to a LIVE session
/// rather than claiming more than the code does:
///
/// * `spawn` MINTS a session. The halt binds sessions, not the instance, and a
///   session created after the halt carries no hold of its own — under
///   `reason=fabric-lost` there is no bridge left to hold it either — so a
///   driver refused on every held session can still open a fresh one and type
///   into that. Halting `spawn` would refuse a human's `aterm new-tab`, which
///   arrives on the same seam and is indistinguishable from a driver's, so this
///   is a design question (§5.3) and not a gate this function may decide alone.
/// * `update apply` re-execs the instance, which ends every session and with
///   them every in-memory hold. It is Owner-only, is answered as
///   `Access::AnyScopeMeta` before any session resolves, and needs a build
///   already staged on disk; it is recorded here so a reader does not conclude
///   from the first sentence that no verb can end a halted session.
pub(crate) fn is_pty_reaching(verb: &str) -> bool {
    matches!(
        verb,
        "send"
            | "key"
            | "ctrl"
            | "feed"
            | "feed-bin"
            | "paste"
            | "paste-bin"
            | "mouse"
            | "resize"
            | "focus"
            | "signal"
            | "turn"
            | "close"
            | "invoke"
            | "hwkey"
            | "pane"
            | "tab"
            | "operator-propose-bin"
    )
}

/// The refusal a halted session answers `verb` with, or `None` when the verb may
/// proceed. Transient class beside `ERR busy`, so a driver's existing back-off
/// already does the right thing.
///
/// SCOPE-BLIND on purpose: the halt binds Owner, Edge and Bridge alike. A halt
/// only the unprivileged obey is decoration.
pub(crate) fn halt_refusal(ctx: &SessionCtx, verb: &str) -> Option<String> {
    if !is_pty_reaching(verb) {
        return None;
    }
    let hold = ctx.fabric.hold()?;
    Some(format!(
        "ERR halted reason={} origin={}\n",
        hold.reason, hold.origin
    ))
}

/// [`halt_refusal`] for an APP-TARGET verb, which has no session to check.
///
/// `invoke <action>` and `tab <sub-form>` are answered in
/// `dispatch_before_session`, BEFORE any session is resolved, and what they touch
/// is whatever window is frontmost at the time — which this thread cannot learn
/// without a main-thread hop, and which can change between the check and the
/// action anyway. So the question asked here is the only one that is both
/// answerable and honest: is ANY session on this instance held?
///
/// FAIL CLOSED, and cheap in the case it exists for. A fleet halt is a fleet-wide
/// gesture — the node's bridge writes `hold <sid> on origin=fleet` to every
/// session it hosts — so when this matters the answer is the same either way.
/// Over-refusing costs an unheld front tab one clipboard paste and one tab
/// switch it can still perform from the physical keyboard, which the halt
/// deliberately never touches. Under-refusing costs the whole property: a
/// standing halt that let `invoke SelectAll` + `copy` + `invoke Paste` put the
/// session's own screen text on its own PTY, or let `tab close` retire the very
/// session `close` had just been refused on.
pub(crate) fn app_halt_refusal(store: &Store, verb: &str) -> Option<String> {
    if !is_pty_reaching(verb) {
        return None;
    }
    // Clone the ctxs out from under the registry guard: the fabric lock is a leaf
    // and is never taken while the store is held (`cmd_deliver` and
    // [`bridge_lost`] keep the same discipline).
    let ctxs: Vec<_> = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.live_handles().map(|h| h.ctx.clone()).collect()
    };
    ctxs.iter().find_map(|ctx| halt_refusal(ctx, verb))
}

// ---------------------------------------------------------------------------
// Small wire helpers
// ---------------------------------------------------------------------------

/// A principal segment: class prefix + a bounded lowercase name, optionally
/// `@<node>` for a session behind a node. Nothing a sender types reaches this —
/// the bridge renders it from the delivered subject — but the endpoint validates
/// it anyway, so a compromised bridge cannot smuggle a sentence into a `from=`
/// field an agent reads as identity.
fn valid_principal(p: &str) -> bool {
    let ok_one = |s: &str| {
        let mut it = s.splitn(2, '-');
        let (Some(class), Some(name)) = (it.next(), it.next()) else {
            return false;
        };
        matches!(class, "s" | "n" | "h" | "a")
            && (1..=40).contains(&name.len())
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    match p.split_once('@') {
        Some((owner, node)) => ok_one(owner) && ok_one(node),
        None => ok_one(p),
    }
}

/// Split `k=v` and keep the value, for a token that must be exactly one `k=v`.
fn kv<'a>(tok: &'a str, key: &str) -> Option<&'a str> {
    tok.strip_prefix(key)
        .and_then(|rest| rest.strip_prefix('='))
}

/// A halt reason, sanitized for the wire. `-` for none.
///
/// This string is printed VERBATIM into `ERR halted …`, onto the events digest,
/// and into `status`, so it is rebuilt byte by byte rather than trusted: the wire
/// form is pct-encoded and therefore ASCII by construction, and anything else is
/// dropped rather than passed through — a raw control byte or a bidi override in a
/// halt reason would become every reader's problem. Clamping then cannot split a
/// character, and the trailing-escape trim stops it splitting a `%XX` either.
fn reason_token(raw: &str) -> String {
    let mut out = String::with_capacity(REASON_MAX);
    for byte in raw.trim().bytes() {
        if out.len() == REASON_MAX {
            break;
        }
        if byte.is_ascii_graphic() {
            out.push(byte as char);
        }
    }
    while out.ends_with('%') || (out.len() >= 2 && out.as_bytes()[out.len() - 2] == b'%') {
        out.pop();
    }
    if out.is_empty() { "-".to_string() } else { out }
}

// ---------------------------------------------------------------------------
// hold
// ---------------------------------------------------------------------------

const HOLD_USAGE: &str = "ERR usage: hold <sid> on|off [reason=<pct>] [origin=fleet|local]\n";

/// Apply (or lift) a hold and record the transition. Records the timeline event
/// WHILE holding the fabric guard — the sanctioned fabric → timeline nesting —
/// so a watcher's `EVENT hold` order can never invert against the stored flag.
/// Signals the condvar so a parked `await inbox … kinds=hold` wakes at once.
fn apply_hold(ctx: &SessionCtx, hold: Option<Hold>) -> bool {
    let mut inbox = ctx.fabric.lock();
    let changed = inbox.hold != hold;
    if changed {
        let (flag, reason, origin) = match &hold {
            Some(h) => (1, h.reason.clone(), h.origin.clone()),
            None => (0, "-".to_string(), "local".to_string()),
        };
        inbox.hold = hold;
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record("hold", format!("{flag} reason={reason} origin={origin}"));
    }
    drop(inbox);
    if changed {
        ctx.fabric.changed.notify_all();
    }
    changed
}

/// Run `f` with the process-wide bridge link RESET to "never launched", and reset
/// it again on the way out — on an unwind too, through the guard's `Drop`.
///
/// The link is per-INSTANCE state in a `static`, which is right for production
/// (one bridge, one instance) and a hazard for a parallel test binary: a test
/// that attaches or loses a bridge is visible to every other test that reads
/// `fabric_state()`, `status`'s own record included. Every such test takes THIS,
/// so they serialize and each starts from `absent`.
#[cfg(test)]
pub(crate) fn with_link_reset<T>(f: impl FnOnce() -> T) -> T {
    static LINK_TESTS: Mutex<()> = Mutex::new(());

    fn reset_now() {
        LINK.state.store(FABRIC_ABSENT, Ordering::Relaxed);
        *LINK.generation.lock().unwrap_or_else(|p| p.into_inner()) = 0;
        LINK.touched
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    /// ONE GUARD, so the order cannot be written wrongly.
    ///
    /// Rust runs a struct's own `Drop::drop` BEFORE dropping its fields, so the
    /// reset below happens while `_lock` — the field that serializes the section
    /// — is still held, and the mutex is released only afterwards. The two-binding
    /// form this replaced (`let guard = …; let _reset_on_exit = Reset; let out =
    /// f(); drop(guard); out`) released the lock FIRST on the success path,
    /// because the explicit `drop(guard)` ran before the reset guard's scope-end
    /// drop — so the reset landed outside the lock and could clear the link under
    /// the next section, which then saw `fabric=absent` where it had just
    /// attached.
    struct Section<'a> {
        _lock: std::sync::MutexGuard<'a, ()>,
    }
    impl Drop for Section<'_> {
        fn drop(&mut self) {
            // PROVEN, not asserted in prose: `Mutex` is not reentrant, so a
            // `try_lock` that SUCCEEDS here means the section's lock is already
            // gone and the next section could be running against the state this
            // is about to clear. (The check is conservative in the safe
            // direction — a lock another thread grabbed also fails it — so it
            // can under-report, never falsely fire.)
            assert!(
                LINK_TESTS.try_lock().is_err(),
                "the link reset ran outside the lock that serializes it"
            );
            reset_now();
        }
    }

    let _section = Section {
        _lock: LINK_TESTS.lock().unwrap_or_else(|p| p.into_inner()),
    };
    reset_now();
    f()
}

/// Whether [`FabricLink::touched`] currently holds `sid`. Test-only. Unlike an
/// exact global count, MEMBERSHIP of a sid this test itself minted is meaningful
/// even while unlocked sibling tests insert THEIR sids concurrently — which is
/// exactly what they do: 17 tests in `inbox_hold` call `deliver` outside
/// [`with_link_reset`]'s mutex, so any exact global count raced and the
/// governed-set test flaked whenever the schedule overlapped it with one of
/// them (2 of 3 full-suite samples, 2026-09-01).
#[cfg(test)]
pub(crate) fn touched_contains(sid: &str) -> bool {
    LINK.touched
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .any(|s| s == sid)
}

/// [`apply_hold`] for a caller that already holds the ctx — the `status` test's
/// door into the same state the bridge writes, without a registry round trip.
#[cfg(test)]
pub(crate) fn apply_hold_for_test(ctx: &SessionCtx, hold: Option<Hold>) -> bool {
    apply_hold(ctx, hold)
}

/// `hold <sid> on|off [reason=<pct>] [origin=fleet|local]` — the fleet drive
/// halt, BRIDGE-ONLY (the dispatch gates the scope; this is the handler).
pub(crate) fn cmd_hold(store: &Store, rest: &str) -> String {
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
    let mut origin = "fleet".to_string();
    for tok in toks {
        if let Some(v) = kv(tok, "reason") {
            reason = v.to_string();
        } else if let Some(v) = kv(tok, "origin") {
            if !matches!(v, "fleet" | "local") {
                return HOLD_USAGE.to_string();
            }
            origin = v.to_string();
        } else {
            return HOLD_USAGE.to_string();
        }
    }
    let ctx = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        match g.by_sid(&SessionId::new(sid)) {
            Some(h) => h.ctx.clone(),
            None => return "ERR no such session\n".to_string(),
        }
    };
    // The session is now governed by the bridge whichever way this went: a `hold
    // off` is as much an act of governance as a `hold on`, and losing the bridge
    // right after one must fail closed too.
    note_bridge_touched(store, sid);
    let hold = on.then(|| Hold {
        reason: reason_token(&reason),
        origin,
    });
    apply_hold(&ctx, hold);
    format!("OK hold={}\n", u8::from(on))
}

// ---------------------------------------------------------------------------
// deliver
// ---------------------------------------------------------------------------

const DELIVER_USAGE: &str = "ERR usage: deliver <sid> off=<n> from=<p> kind=<k> trust=<t> \
                             [re=<n>] [dl=<ms>] [late=1] [demoted=<k>] [via=<p,...>] [len=<n>] \
                             [text=<pct>] | deliver <sid> landed=<post-id> off=<n>\n";

/// `deliver` — put one bus record in a session's inbox, or close an outbound post.
/// BRIDGE-ONLY (the dispatch gates the scope).
///
/// IDEMPOTENT ON `off=`, OVER A BOUNDED WINDOW. The bus gives at-least-once
/// delivery (the group cursor commits AFTER this returns `OK`), so a bridge that
/// crashes in that window redelivers. Answering the id the offset first got — and
/// appending nothing — is the sink half that makes the path exactly-once into the
/// endpoint FOR THE LAST [`DEDUP_CAP`] DELIVERED OFFSETS.
///
/// AND NOT ONE OFFSET FURTHER, which is stated here because the two agent-facing
/// claims used to promise it unqualified. `remember_offset` pops the oldest entry
/// once the window is full, so a redelivery older than that reappears as a FRESH
/// row with a new id — the honest behaviour (the row was dropped unread and the
/// bus still holds it), but reachable in ordinary operation: `Bridge::refill`
/// re-delivers everything from the persisted `seen_off + 1` on every
/// `session-created` and on the roster tick, so a session that lists its mail
/// without ever running `inbox seen` accumulates offsets past the window and sees
/// the oldest of them again as new mail. The `deliver` verb row says so too.
pub(crate) fn cmd_deliver(store: &Store, rest: &str) -> String {
    let mut toks = rest.split_whitespace();
    let Some(sid) = toks.next() else {
        return DELIVER_USAGE.to_string();
    };
    let ctx = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        match g.by_sid(&SessionId::new(sid)) {
            Some(h) => h.ctx.clone(),
            None => return "ERR no such session\n".to_string(),
        }
    };
    let rest_toks: Vec<&str> = toks.collect();
    note_bridge_touched(store, sid);
    if rest_toks.iter().any(|t| t.starts_with("landed=")) {
        return deliver_landed(&ctx, &rest_toks);
    }
    deliver_row(&ctx, &rest_toks)
}

/// `deliver <sid> landed=<post-id> off=<n>` — the §11.2 spelling of a post's
/// retirement. ONE implementation with `outbox sent`, which is the same act with
/// one more case (`off=-`, permanently undeliverable): two names for one
/// transition would be two chances to disagree about the watermark.
fn deliver_landed(ctx: &SessionCtx, toks: &[&str]) -> String {
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
    match retire_post(ctx, post_id, Some(off), DEAD_DEFAULT) {
        Ok(()) => "OK\n".to_string(),
        Err(e) => e,
    }
}

/// Retire one outbound post: `Some(off)` is the offset it landed at, `None` is
/// "permanently undeliverable", with `reason` naming which refusal it was.
/// Either way the retained body is DROPPED — that retention bound is what makes
/// the outbound queue safe to hold at all — and any parked `post --wait` is
/// released.
///
/// IDEMPOTENT. A retirement repeated after a lost reply is an `OK` that appends
/// no second timeline event, so a bridge that retries cannot double-push
/// `post-landed` onto the events digest.
fn retire_post(
    ctx: &SessionCtx,
    post_id: u64,
    off: Option<u64>,
    reason: &str,
) -> Result<(), String> {
    let mut inbox = ctx.fabric.lock();
    let Some(row) = inbox.posts.iter_mut().find(|p| p.id == post_id) else {
        return Err("ERR no such post\n".to_string());
    };
    let already = match off {
        Some(n) => row.off == Some(n),
        None => row.dead,
    };
    match off {
        Some(n) => row.off = Some(n),
        None => {
            row.dead = true;
            row.dead_reason = reason.to_string();
        }
    }
    row.body.clear();
    row.body.shrink_to_fit();
    if !already {
        let payload = match off {
            Some(n) => format!("{post_id} off={n}"),
            None => format!("{post_id} off=-"),
        };
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record("post-landed", payload);
    }
    inbox.trim_retired_posts();
    drop(inbox);
    ctx.fabric.changed.notify_all();
    Ok(())
}

// ---------------------------------------------------------------------------
// outbox / outbox sent
// ---------------------------------------------------------------------------

const OUTBOX_USAGE: &str =
    "ERR usage: outbox [<max>] | outbox sent <sid> <post-id> off=<n|-> [reason=<word>]\n";

/// The verdict a bare `off=-` carries, and the only one A2 could express.
const DEAD_DEFAULT: &str = "undeliverable";

/// A `reason=` token: one to twenty-four lowercase ASCII letters or dashes. It
/// is echoed into a `post --wait`'s `ERR <reason> id=<n>` reply, which is one
/// line of the control protocol, so the shape is checked HERE rather than
/// trusted — a bridge is a separate process and its output is still input.
fn valid_dead_reason(v: &str) -> bool {
    !v.is_empty() && v.len() <= 24 && v.chars().all(|c| c.is_ascii_lowercase() || c == '-')
}

/// `outbox` — the whole keyword. Splits its one sub-form here, exactly as
/// `inbox` does, so the router keeps one arm per keyword.
pub(crate) fn cmd_outbox_verb(store: &Store, rest: &str) -> String {
    match rest.split_once(' ') {
        Some(("sent", tail)) => cmd_outbox_sent(store, tail),
        _ if rest.trim() == "sent" => OUTBOX_USAGE.to_string(),
        _ => cmd_outbox(store, rest),
    }
}

/// `outbox [<max>]` — drain every session's queued outbound posts WITH their
/// bodies, as one length-prefixed frame. BRIDGE-ONLY (the dispatch gates it).
///
/// A PEEK, on purpose. It moves no watermark and removes nothing: a bridge that
/// dies between reading a post and getting its `PublishAck` re-reads the same
/// post on restart and republishes it under the same producer sequence, which the
/// broker's `(producer_id, producer_seq)` dedup then collapses. Retirement is a
/// separate, explicit act (`outbox sent`), which is what makes the pair the
/// outbound mirror of `deliver` / `inbox seen`.
///
/// FRAMING. `OK <nbytes>` then that many bytes, holding a `post …  len=<n>` line
/// and then that post's `n` body bytes, repeated. A length prefix rather than
/// rows because a body may contain newlines — the reason `deliver`'s inline
/// `text=` is pct-encoded and this is not.
///
/// BOUNDED IN BYTES AS WELL AS ROWS, and the byte bound is the one that binds.
/// [`OUTBOX_CAP`]/[`OUTBOX_BYTES_MAX`] bound one SESSION's queue (4 MiB each);
/// this reply concatenated EVERY registered session's queued bodies into one
/// `String`, and the only caller (`Bridge::drain_outbox`) issues a bare `outbox`
/// and never passes the `[<max>]` the grammar advertises — which counts posts,
/// not bytes, and defaults to `usize::MAX`. Thirty sessions queued full behind an
/// unreachable broker therefore cost ~120 MiB in the payload, another ~120 MiB in
/// the `format!` copy, and another in the bridge's `vec![0u8; n]` for the
/// announced length: "an unbounded wait becomes an unbounded buffer", which is
/// the trade [`PostRow::body`] says this verb pair exists to refuse.
/// [`OUTBOX_DRAIN_BYTES_MAX`] now stops the walk, and A PARTIAL DRAIN IS SAFE
/// PRECISELY BECAUSE THIS IS A PEEK: nothing was retired, the remaining posts are
/// still queued in the same order, and the bridge's idle tick calls `outbox`
/// again. The bound is enforced HERE, where the reply is built, rather than by
/// asking every caller to remember a number — the two sides of a bound must meet
/// in one place.
pub(crate) fn cmd_outbox(store: &Store, rest: &str) -> String {
    let mut max = usize::MAX;
    for tok in rest.split_whitespace() {
        match tok.parse::<usize>() {
            Ok(n) => max = n,
            Err(_) => return OUTBOX_USAGE.to_string(),
        }
    }
    // Sessions in `local_id` order, posts in queue order: the drain a bridge sees
    // is the order the endpoint accepted them in, which is the order the bus must
    // see them in for a per-sender stream to mean anything.
    let handles = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.snapshot()
    };
    let mut payload = String::new();
    let mut sent = 0usize;
    for h in handles {
        if sent >= max || payload.len() >= OUTBOX_DRAIN_BYTES_MAX {
            break;
        }
        let sid = h.sid.as_str().to_string();
        let inbox = h.ctx.fabric.lock();
        for post in inbox.posts.iter().filter(|p| p.off.is_none() && !p.dead) {
            // AFTER the first post, never before it: a single body may legally be
            // as large as `BODY_MAX`, and a budget that could refuse the post at
            // the head of the queue would strand it forever behind a bound it can
            // never fit under.
            if sent >= max || (sent > 0 && payload.len() >= OUTBOX_DRAIN_BYTES_MAX) {
                break;
            }
            payload.push_str(&format!(
                "post sid={sid} id={} to={} kind={}",
                post.id, post.to, post.kind
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
            payload.push_str(&format!(" len={}\n", post.body.len()));
            payload.push_str(&post.body);
            sent += 1;
        }
    }
    format!("OK {}\n{payload}", payload.len())
}

/// `outbox sent <sid> <post-id> off=<n|->` — retire one queued post.
/// BRIDGE-ONLY (the dispatch gates it).
pub(crate) fn cmd_outbox_sent(store: &Store, rest: &str) -> String {
    let mut toks = rest.split_whitespace();
    let (Some(sid), Some(id), Some(off_tok)) = (toks.next(), toks.next(), toks.next()) else {
        return OUTBOX_USAGE.to_string();
    };
    let mut reason = DEAD_DEFAULT;
    for tok in toks {
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
    let ctx = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        match g.by_sid(&SessionId::new(sid)) {
            Some(h) => h.ctx.clone(),
            None => return "ERR no such session\n".to_string(),
        }
    };
    note_bridge_touched(store, sid);
    match retire_post(&ctx, post_id, off, reason) {
        Ok(()) => "OK\n".to_string(),
        Err(e) => e,
    }
}

/// The ordinary `deliver`: one message row.
#[allow(clippy::too_many_lines)]
fn deliver_row(ctx: &SessionCtx, toks: &[&str]) -> String {
    let mut off: Option<u64> = None;
    let mut from = String::new();
    let mut kind = String::new();
    let mut trust = String::new();
    let mut re: Option<u64> = None;
    let mut dl: Option<u64> = None;
    let mut late = false;
    let mut demoted: Option<String> = None;
    let mut via: Option<String> = None;
    let mut declared_len: Option<usize> = None;
    let mut text = String::new();
    for tok in toks {
        if let Some(v) = kv(tok, "off") {
            match v.parse() {
                Ok(n) => off = Some(n),
                Err(_) => return DELIVER_USAGE.to_string(),
            }
        } else if let Some(v) = kv(tok, "from") {
            from = v.to_string();
        } else if let Some(v) = kv(tok, "kind") {
            kind = v.to_string();
        } else if let Some(v) = kv(tok, "trust") {
            trust = v.to_string();
        } else if let Some(v) = kv(tok, "re") {
            match v.parse() {
                Ok(n) => re = Some(n),
                Err(_) => return DELIVER_USAGE.to_string(),
            }
        } else if let Some(v) = kv(tok, "dl") {
            match v.parse() {
                Ok(n) => dl = Some(n),
                Err(_) => return DELIVER_USAGE.to_string(),
            }
        } else if let Some(v) = kv(tok, "late") {
            late = v == "1";
        } else if let Some(v) = kv(tok, "demoted") {
            if !KINDS.contains(&v) {
                return DELIVER_USAGE.to_string();
            }
            demoted = Some(v.to_string());
        } else if let Some(v) = kv(tok, "via") {
            if !v.split(',').all(valid_principal) {
                return DELIVER_USAGE.to_string();
            }
            via = Some(v.to_string());
        } else if let Some(v) = kv(tok, "len") {
            match v.parse::<usize>() {
                Ok(n) if n <= BODY_MAX => declared_len = Some(n),
                _ => return "ERR too large\n".to_string(),
            }
        } else if let Some(v) = kv(tok, "text") {
            text = v.to_string();
        } else {
            return DELIVER_USAGE.to_string();
        }
    }
    let Some(off) = off else {
        return DELIVER_USAGE.to_string();
    };
    if !valid_principal(&from) || !KINDS.contains(&kind.as_str()) {
        return DELIVER_USAGE.to_string();
    }
    // `trust=` is the RECEIVER's verdict, computed by the bridge from (face,
    // sender class, relay), and the endpoint refuses a label it does not know
    // rather than passing an uninterpreted token to an agent as if it meant
    // something. The default is the conservative one, not the trusting one.
    if trust.is_empty() {
        trust = "agent".to_string();
    }
    if !TRUSTS.contains(&trust.as_str()) {
        return DELIVER_USAGE.to_string();
    }
    let body = aterm_control::wire::pct_decode(&text);
    if body.len() > BODY_MAX {
        return "ERR too large\n".to_string();
    }
    let len = declared_len.unwrap_or(body.len());

    let mut inbox = ctx.fabric.lock();
    // IDEMPOTENCY, before anything else: the offset decides, and a duplicate
    // answers the id it first got without touching the ring, the quota or the
    // events digest.
    if inbox.dedup_set.contains(&off) {
        let existing = inbox.rows.iter().find(|r| r.off == off).map(|r| r.id);
        return match existing {
            Some(id) => format!("OK {id}\n"),
            // Delivered before, then evicted or pruned: the row is gone but the
            // offset is not deliverable twice. `OK 0` names no row on purpose —
            // a bridge that re-reads it learns the endpoint kept nothing.
            None => "OK 0\n".to_string(),
        };
    }
    // PER-PEER QUOTA. Counted over UNLISTED rows only — a peer that has already
    // had its say and been read is not holding the ring hostage — and keyed on
    // the CAP-FORCED half of `from=` (see [`quota_key`] and [`SENDER_QUOTA`]),
    // because the rest of that string is the sending node's own word and a peer
    // that could choose its own quota key would have no quota at all.
    let key = quota_key(&from);
    let unread_from_peer = inbox
        .rows
        .iter()
        .filter(|r| !r.listed && quota_key(&r.from) == key)
        .count();
    if unread_from_peer >= SENDER_QUOTA {
        return "ERR quota\n".to_string();
    }

    inbox.next_msg_id += 1;
    let id = inbox.next_msg_id;
    let re_id = re.and_then(|r| inbox.posts.iter().find(|p| p.off == Some(r)).map(|p| p.id));
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
        demoted,
        via,
        len,
        body,
        listed: false,
    };
    evict_for(&mut inbox);
    inbox.rows.push_back(row);
    inbox.bus_head = inbox.bus_head.max(off);
    remember_offset(&mut inbox, off);
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .record("inbox", format!("{id} from={from} kind={kind} off={off}"));
    drop(inbox);
    ctx.fabric.changed.notify_all();
    format!("OK {id}\n")
}

/// The identity [`SENDER_QUOTA`] is counted against: the part of `from=` the
/// broker's grant FORCED, which is the `<src>` segment of the delivered subject.
///
/// `h-andrew`, `a-svc` and `n-lab` are forced whole. `s-<sid>@n-lab` is forced
/// only after the `@`: everything before it is the node's own claim about which
/// of its sessions spoke (§8.3's attested exception), so it identifies a SENDER
/// only as far as that node is honest, and it identifies a PEER not at all.
/// Returning the suffix collapses every sid one node can mint into the single
/// allowance the design promises per peer.
fn quota_key(from: &str) -> &str {
    from.split_once('@').map_or(from, |(_, node)| node)
}

/// Remember a delivered offset for idempotency, dropping the oldest past the cap.
fn remember_offset(inbox: &mut Inbox, off: u64) {
    inbox.dedup.push_back(off);
    inbox.dedup_set.insert(off);
    while inbox.dedup.len() > DEDUP_CAP {
        if let Some(old) = inbox.dedup.pop_front() {
            inbox.dedup_set.remove(&old);
        }
    }
}

/// Make room for one more row, if the ring is full.
///
/// THE ORDER IS THE POLICY, and CLASS IS THE PRIMARY KEY: (class, listed, age).
/// An AGENT's row goes before a HUMAN's, whatever either one's listed state, and
/// only within one class does an already-listed row go before an unlisted one
/// (the agent has had its chance to read it). So a burst of agent `note`s evicts
/// itself long before it can reach the one `task` a person sent.
///
/// THE ORDER WAS (listed, class) AND THAT WAS THE BUG. Predicate 2 was a bare
/// `listed`, which matched a HUMAN's listed row before predicate 3 ever looked at
/// an unlisted agent row — so a ring holding one listed human `task` and 511
/// unlisted agent `note`s evicted the human's on the 513th delivery. Three
/// separate places promised the opposite and were all right about the intent:
/// `deliver`'s help ("eviction never drops an `h-*` row ahead of an agent's"),
/// DESIGN §5's "never evicts an `h-*` or allowlisted row ahead of anyone else's",
/// and this comment. The code is what moved.
///
/// AND WHAT LEAVES UNHANDLED IS COUNTED. `dropped=` bumps for any evicted row
/// past the HANDLED watermark (`id > seen`), not merely an unlisted one: a row
/// listed once at turn start and never acted on is precisely the loss the counter
/// exists to make visible, and it was the one case that stayed silent.
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

// ---------------------------------------------------------------------------
// inbox / inbox get / inbox seen
// ---------------------------------------------------------------------------

const INBOX_USAGE: &str = "ERR usage: inbox [<n>] [since=<id>] [--peek] [--meta] | inbox get <id> \
                           | inbox seen <id> [handled|refused|deferred]\n";

/// `inbox` — the whole keyword, sub-forms included. The three forms split HERE
/// rather than in the router's `match verb`, so the router keeps one arm per
/// keyword and the sub-keywords never look like verbs of their own.
pub(crate) fn cmd_inbox_verb(ctx: &SessionCtx, rest: &str) -> String {
    match rest.split_once(' ') {
        Some(("get", tail)) => cmd_inbox_get(ctx, tail),
        Some(("seen", tail)) => cmd_inbox_seen(ctx, tail),
        // A bare sub-keyword with no argument: usage, never a silent listing.
        _ if matches!(rest.trim(), "get" | "seen") => INBOX_USAGE.to_string(),
        _ => cmd_inbox(ctx, rest),
    }
}

/// The `holder=` token: who holds this session's keyboard, as `who`/`lease`
/// report it. `-` when nobody does.
fn holder_token(ctx: &SessionCtx) -> String {
    let now = crate::metrics::now_us();
    ctx.turn_lease
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .and_then(|l| l.driving_token(now))
        .unwrap_or_else(|| "-".to_string())
}

/// `inbox [<n>] [since=<id>] [--peek] [--meta]` — the drain a turn-based agent
/// runs at turn start. One header, one row per message, one row per un-landed
/// post; `Lines`-framed, so `OK <n>` counts EVERY row that follows.
pub(crate) fn cmd_inbox(ctx: &SessionCtx, rest: &str) -> String {
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
    let mut inbox = ctx.fabric.lock();
    let hold_flag = u8::from(inbox.hold.is_some());
    let selected: Vec<u64> = inbox
        .rows
        .iter()
        .filter(|r| r.id > since)
        .map(|r| r.id)
        .collect();
    // `<n>` keeps the NEWEST n, the `history`/`timeline` grammar.
    let keep: Vec<u64> = match limit {
        Some(n) => selected.iter().rev().take(n).rev().copied().collect(),
        None => selected,
    };
    let kept: HashSet<u64> = keep.iter().copied().collect();

    let mut msg_lines = Vec::with_capacity(kept.len());
    for row in inbox.rows.iter().filter(|r| kept.contains(&r.id)) {
        msg_lines.push(render_row(row, meta_only));
    }
    let post_lines: Vec<String> = inbox
        .posts
        .iter()
        .filter(|p| p.off.is_none() && !p.dead)
        .map(|p| {
            format!(
                "post {} to={} kind={} off=- len={}",
                p.id,
                p.to,
                p.kind,
                p.body.len()
            )
        })
        .collect();

    // `pending=` is what the ring still holds that this reply did NOT carry:
    // rows waiting for an explicit drain. Zero after a bare `inbox` that listed
    // everything.
    //
    // COUNTED OVER THE PER-ROW FLAG, not over a watermark. A non-peek `inbox`
    // marks only the rows it KEPT, so a bounded (`inbox <n>`) or `since=`-filtered
    // reply leaves older rows unlisted BELOW the newest one it carried. A
    // watermark test called every one of those "not pending" and reported
    // `pending=0` with mail still sitting in the ring — the verb table promises
    // "the delivered rows this reply did not carry", with no watermark qualifier,
    // and this is that number. (The watermark field is gone: one value with two
    // copies is how the two came to disagree.)
    let pending = inbox
        .rows
        .iter()
        .filter(|r| !r.listed && !kept.contains(&r.id))
        .count();

    if !peek {
        for row in &mut inbox.rows {
            if kept.contains(&row.id) {
                row.listed = true;
            }
        }
    }

    let header = format!(
        "OK {} hold={hold_flag} holder={holder} seen={} bus_head={} dropped={} pending={pending}\n",
        msg_lines.len() + post_lines.len(),
        inbox.seen,
        inbox.bus_head,
        inbox.dropped,
    );
    drop(inbox);
    let mut out = header;
    for line in msg_lines.into_iter().chain(post_lines) {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// One `msg` row. `trust=` is printed before the text ON PURPOSE (§4.3): what a
/// message IS must reach the reader ahead of what it says.
fn render_row(row: &InboxRow, meta_only: bool) -> String {
    let mut s = format!(
        "msg {} off={} t={} from={} kind={} trust={}",
        row.id, row.off, row.t_ms, row.from, row.kind, row.trust
    );
    if let Some(re) = row.re {
        s.push_str(&format!(" re={re}"));
        if let Some(re_id) = row.re_id {
            s.push_str(&format!(" re-id={re_id}"));
        }
    }
    if let Some(dl) = row.dl {
        s.push_str(&format!(" dl={dl}"));
    }
    if row.late {
        s.push_str(" late=1");
    }
    if let Some(d) = &row.demoted {
        s.push_str(&format!(" demoted={d}"));
    }
    if let Some(v) = &row.via {
        s.push_str(&format!(" via={v}"));
    }
    s.push_str(&format!(" len={}", row.len));
    // `truncated=1` means the ENDPOINT NEVER RECEIVED the rest: the delivering
    // bridge cut the body to fit one control request line and named the true size
    // in `len=`. `more=1` means only "this row does not carry the whole of what
    // the endpoint holds" — so a cut body sets BOTH, including the case where
    // what survived is under `TEXT_PREVIEW_MAX` and the row would otherwise have
    // read as a complete short message beside a much larger `len=`.
    let truncated = row.len > row.body.len();
    if row.body.len() > TEXT_PREVIEW_MAX || truncated {
        s.push_str(" more=1");
    }
    if truncated {
        s.push_str(" truncated=1");
    }
    if !meta_only {
        // Cut on a CHARACTER boundary at or below the byte bound: a preview that
        // halved a multi-byte character would render as a replacement glyph in
        // the one field an agent actually reads.
        let mut end = TEXT_PREVIEW_MAX.min(row.body.len());
        while end > 0 && !row.body.is_char_boundary(end) {
            end -= 1;
        }
        s.push_str(&format!(" text={}", pct_encode(&row.body[..end])));
    }
    s
}

/// `inbox get <id>` — the whole of the body THIS ENDPOINT HOLDS, as a
/// length-prefixed byte frame.
///
/// NOT ALWAYS THE WHOLE MESSAGE, AND IT SAYS SO. A body that did not fit one
/// control request line was cut by the delivering bridge, which is a decision
/// taken before the endpoint ever saw the record: the rest is on the bus, and the
/// recipient has no bus access — that is the point of the endpoint — so there is
/// no route by which this verb could fetch it. It used to answer a cut body under
/// a header identical to a complete one, and its help promised "the FULL body",
/// so the one documented way to read a message could not distinguish the two.
/// Now a cut answer is `OK <nbytes> truncated=1 len=<true>`: only the first token
/// after `OK` is the frame length (`aterm-ctl`'s `byte_count` and `Ctl`'s
/// `count_after_ok` both read exactly that), so the marker rides in the tail
/// without changing the framing. `glance.rs` established `"truncated": true` as
/// this crate's way of saying an answer is not a complete one.
pub(crate) fn cmd_inbox_get(ctx: &SessionCtx, rest: &str) -> String {
    let mut toks = rest.split_whitespace();
    let (Some(id), None) = (toks.next().and_then(|t| t.parse::<u64>().ok()), toks.next()) else {
        return INBOX_USAGE.to_string();
    };
    let inbox = ctx.fabric.lock();
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

/// `inbox seen <id> [handled|refused|deferred]` — advance the HANDLED watermark
/// and record the decision. Exempt from `hold`: a halted agent must still be able
/// to say it read the notice.
///
/// AND IT LISTS EVERY ROW AT OR BELOW `<id>`, which is a second effect and not a
/// side effect. `listed` is what the per-peer quota counts and what makes a row
/// the first eviction candidate, so acknowledging a row also RELEASES its
/// sender's quota and lets the ring drop it. Without this, an agent that reads
/// through a `--peek` listing — the file-plane mirror does exactly that, and
/// never runs a bare `inbox` — could acknowledge all of its mail and still be
/// refused the next message with `ERR quota` forever. The `inbox seen` verb row
/// says so; a change here that stopped touching `listed` would silently reinstate
/// that deadlock, so `an_ack_releases_the_senders_quota` pins it.
pub(crate) fn cmd_inbox_seen(ctx: &SessionCtx, rest: &str) -> String {
    let mut toks = rest.split_whitespace();
    let Some(id) = toks.next().and_then(|t| t.parse::<u64>().ok()) else {
        return INBOX_USAGE.to_string();
    };
    match toks.next() {
        None | Some("handled" | "refused" | "deferred") => {}
        Some(_) => return INBOX_USAGE.to_string(),
    }
    if toks.next().is_some() {
        return INBOX_USAGE.to_string();
    }
    let mut inbox = ctx.fabric.lock();
    let Some(off) = inbox.rows.iter().find(|r| r.id == id).map(|r| r.off) else {
        return "ERR no such message\n".to_string();
    };
    // MONOTONE: the watermark never goes backwards, so acknowledging an older row
    // out of order cannot re-open rows already handled.
    inbox.seen = inbox.seen.max(id);
    for row in &mut inbox.rows {
        if row.id <= id {
            row.listed = true;
        }
    }
    let seen = inbox.seen;
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .record("inbox-seen", format!("{id} off={off}"));
    drop(inbox);
    ctx.fabric.changed.notify_all();
    format!("OK seen={seen}\n")
}

// ---------------------------------------------------------------------------
// post
// ---------------------------------------------------------------------------

const POST_USAGE: &str = "ERR usage: post to=<@<sid>[@<node>]|<principal>|say> \
                          kind=<ask|answer|task|report|note|ack|control> [re=<n>] [dl=<ms>] \
                          [via=<p>] [--wait[=<ms>]] (<text> | len=<n> + <n> raw bytes)\n";

/// Whether `tok` is one of `post`'s leading OPTION tokens. Shared with the serve
/// loop's length-prefixed-frame detector so the two parsers cannot disagree about
/// where the options end and the body begins — a detector that saw a frame the
/// handler did not would leave the announced bytes to be read as control verbs.
pub(crate) fn post_option_token(tok: &str) -> bool {
    tok == "--wait"
        || tok.starts_with("--wait=")
        || ["to=", "kind=", "re=", "dl=", "via=", "len="]
            .iter()
            .any(|k| tok.starts_with(k))
}

/// The byte offset in `rest` where the inline body begins — the first token that
/// is not an option — or `None` when the line is options all the way down.
///
/// Scans FORWARD from the end of the previous token rather than searching the
/// whole string for each one, so a body whose first word also appears inside an
/// earlier option value cannot make the body start in the wrong place.
fn post_body_offset(rest: &str) -> Option<usize> {
    let mut idx = 0usize;
    for tok in rest.split_whitespace() {
        let at = idx + rest[idx..].find(tok)?;
        if !post_option_token(tok) {
            return Some(at);
        }
        idx = at + tok.len();
    }
    None
}

/// How long `post --wait` parks by default, and the ceiling on an explicit wait.
const WAIT_DEFAULT_MS: u64 = 30_000;
const WAIT_MAX_MS: u64 = 600_000;

/// `post` — queue one outbound message and, for the kinds that need a
/// correlation id, wait for the offset it landed at.
///
/// `body` is `Some` only for the length-prefixed frame form, whose bytes the
/// serve loop has already read off the stream.
pub(crate) fn cmd_post(ctx: &SessionCtx, rest: &str, body: Option<Vec<u8>>) -> String {
    let mut to: Option<String> = None;
    let mut kind: Option<String> = None;
    let mut re: Option<u64> = None;
    let mut dl: Option<u64> = None;
    let mut via: Option<String> = None;
    let mut wait: Option<Option<u64>> = None;
    let mut declared_len: Option<usize> = None;
    let body_at = post_body_offset(rest);
    // Options lead; the first token that is not one begins the inline text, so a
    // body may contain `=` and spaces without quoting.
    for tok in rest.split_whitespace() {
        if !post_option_token(tok) {
            break;
        }
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
        } else if let Some(v) = kv(tok, "len") {
            match v.parse::<usize>() {
                Ok(n) if n <= BODY_MAX => declared_len = Some(n),
                _ => return "ERR too large\n".to_string(),
            }
        } else if tok == "--wait" {
            wait = Some(None);
        } else if let Some(v) = kv(tok, "--wait") {
            match v.parse::<u64>() {
                Ok(n) => wait = Some(Some(n.min(WAIT_MAX_MS))),
                Err(_) => return POST_USAGE.to_string(),
            }
        }
    }
    let (Some(to), Some(kind)) = (to, kind) else {
        return POST_USAGE.to_string();
    };
    // [`POSTABLE`], NOT [`KINDS`]: `expired` and `undeliverable` are verdicts a
    // bridge records on a sender's behalf, never something a sender may claim.
    if !POSTABLE.contains(&kind.as_str()) {
        return POST_USAGE.to_string();
    }
    // No `to=fleet`: only a human's `/f/<F>/fleet/<h>/>` grant can write the halt
    // subject, and a node holds none. An agent PROPOSES a halt as an `ask`.
    if to == "fleet" {
        return "ERR denied: no to=fleet — an agent may only ask a human to halt\n".to_string();
    }
    if to != "say" && !valid_principal(to.trim_start_matches('@')) {
        return POST_USAGE.to_string();
    }
    if let Some(v) = &via
        && !v.split(',').all(valid_principal)
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
                return "ERR too large\n".to_string();
            }
            text.into_bytes()
        }
    };
    if body.len() > BODY_MAX {
        return "ERR too large\n".to_string();
    }

    // `--wait` is ON by default for the two kinds whose whole point is a reply:
    // an `ask`/`task` sender that does not learn its offset cannot recognize the
    // answer when it comes back as `re=`.
    let wait = wait.or_else(|| matches!(kind.as_str(), "ask" | "task").then_some(None));

    let mut inbox = ctx.fabric.lock();
    // REFUSED AT THE DOOR. A full outbox is answered, not absorbed: see
    // [`OUTBOX_CAP`]. Counted over the posts still WAITING for the bridge —
    // retired ones hold no body and cost nothing.
    let (queued, queued_bytes) = inbox.queued_load();
    if queued >= OUTBOX_CAP || queued_bytes.saturating_add(body.len()) > OUTBOX_BYTES_MAX {
        return format!("ERR outbox full queued={queued} bytes={queued_bytes}\n");
    }
    inbox.next_post_id += 1;
    let id = inbox.next_post_id;
    let to_tok = pct_encode(&to);
    inbox.posts.push_back(PostRow {
        id,
        to: to_tok.clone(),
        kind: kind.clone(),
        off: None,
        re,
        dl,
        via: via.clone(),
        dead: false,
        dead_reason: String::new(),
        // LOSSY FOR NON-UTF-8, and said out loud: the control reply plane is
        // `String`-typed end to end, so a body that is not UTF-8 is replaced
        // here exactly as `inbox get`'s is on the way in. `outbox`'s length
        // prefix buys framing (a body may hold newlines), not byte-exactness;
        // a byte-exact outbound path needs a bytes-carrying `ControlReply`,
        // which is a wider change than one verb pair.
        body: String::from_utf8_lossy(&body).into_owned(),
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
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .record("post", payload);
    drop(inbox);
    ctx.fabric.changed.notify_all();

    let Some(wait) = wait else {
        return format!("OK {id}\n");
    };
    // Waiting for a landing that nothing can report is a guaranteed timeout, so
    // say so at once and name the id: the post IS queued, and `inbox` lists it.
    if fabric_state() != "connected" {
        return fabric_wait_refusal(id);
    }
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_millis(wait.unwrap_or(WAIT_DEFAULT_MS));
    let mut guard = ctx.fabric.lock();
    loop {
        let row = guard.posts.iter().find(|p| p.id == id);
        if let Some(off) = row.and_then(|p| p.off) {
            return format!("OK {id} off={off}\n");
        }
        // A post the bridge retired as undeliverable releases its wait with a
        // verdict, not a timeout: the caller learns the difference between "the
        // bus never took it" and "nobody has answered yet".
        if let Some(row) = row.filter(|p| p.dead) {
            // THE BRIDGE'S OWN WORD FOR IT. §6.1 makes the two-claimant case
            // `ERR ambiguous`, which the endpoint could never compute: it does
            // not route. The token was validated at `outbox sent`.
            let reason = if row.dead_reason.is_empty() {
                DEAD_DEFAULT
            } else {
                row.dead_reason.as_str()
            };
            return format!("ERR {reason} id={id}\n");
        }
        // THE ENTRY CHECK, ON EVERY WAKE — checked AFTER the landing and the
        // verdict, so a post that landed just before the bridge died still
        // answers `OK … off=`. Waiting for a landing that nothing can report is a
        // guaranteed timeout (§11.2 deviation 9), and a bridge can die while a
        // waiter is parked exactly as easily as before it parked; the entry check
        // alone left that caller sitting out its whole `--wait` (up to
        // `WAIT_MAX_MS` = 600 s) for the `ERR timeout` the check exists to
        // replace. `bridge_lost` wakes every registered session so this runs.
        if fabric_state() != "connected" {
            return fabric_wait_refusal(id);
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return format!("ERR timeout id={id}\n");
        }
        let (next, _) = ctx
            .fabric
            .changed
            .wait_timeout(guard, deadline - now)
            .unwrap_or_else(|p| p.into_inner());
        guard = next;
    }
}

/// The answer a `post --wait` gives when the link cannot report a landing.
///
/// `queued=1`, AND IT IS THE LOAD-BEARING TOKEN. A bridge EXIT is the ordinary
/// relaunch path, not only the terminal one: `fabric_launch::supervise` brings a
/// replacement up after `RELAUNCH_MIN`, and the post survives it untouched —
/// `outbox` is a PEEK that removes nothing, so the new bridge drains the same
/// queue and `outbox sent` retires it seconds later. Answering a bare `ERR fabric
/// disconnected` for a message that is still queued reads as "not sent", and the
/// documented remedy for "not sent" is to send again: `post` carries no
/// idempotency key, so the peer's inbox ends up holding the same `ask` twice.
/// This refusal says the only two things the endpoint actually knows — nothing
/// can report the landing, and the message is still in the outbox — and the
/// `post` verb row says so as well.
fn fabric_wait_refusal(id: u64) -> String {
    format!("ERR fabric {} id={id} queued=1\n", fabric_state())
}

// ---------------------------------------------------------------------------
// await inbox
// ---------------------------------------------------------------------------

/// `await inbox since=<id> [kinds=<k,...>]` — the fabric wake predicate.
///
/// MONOTONE: it latches only on a row whose id is strictly greater than `since`,
/// so a row the agent read and chose to ignore cannot latch the same wait twice.
/// A `hold` transition latches only when `hold` is one of the listed kinds — a
/// halt reaches an agent through the events digest, never by pretending to be
/// mail. `OK timeout` on expiry, which the client exits 124 on.
pub(crate) fn cmd_await_inbox(ctx: &SessionCtx, args: &[&str], timeout_ms: u64) -> String {
    const USAGE: &str = "ERR usage: await inbox since=<id> [kinds=<k,...>] [timeout=<ms>]\n";
    let mut since: Option<u64> = None;
    let mut kinds: Option<Vec<String>> = None;
    for tok in args {
        if let Some(v) = kv(tok, "since") {
            match v.parse() {
                Ok(n) => since = Some(n),
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
        } else {
            return USAGE.to_string();
        }
    }
    let Some(since) = since else {
        return USAGE.to_string();
    };
    let watch_hold = kinds
        .as_ref()
        .is_some_and(|k| k.iter().any(|k| k == "hold"));
    let accepts = |kind: &str| match &kinds {
        Some(list) => list.iter().any(|k| k == kind),
        None => kind != AWAIT_DEFAULT_SKIP,
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let mut guard = ctx.fabric.lock();
    let hold_at_arm = guard.hold.is_some();
    loop {
        if let Some(id) = guard
            .rows
            .iter()
            .filter(|r| r.id > since && accepts(&r.kind))
            .map(|r| r.id)
            .max()
        {
            return format!("OK inbox {id}\n");
        }
        if watch_hold && guard.hold.is_some() != hold_at_arm {
            return format!("OK inbox hold={}\n", u8::from(guard.hold.is_some()));
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return "OK timeout\n".to_string();
        }
        let (next, _) = ctx
            .fabric
            .changed
            .wait_timeout(guard, deadline - now)
            .unwrap_or_else(|p| p.into_inner());
        guard = next;
    }
}

#[cfg(test)]
mod inbox_hold {

    /// `hold`'s help ENUMERATES the PTY-reaching verbs, and `is_pty_reaching` is
    /// the set that actually decides. A hand-typed roster beside a derived set
    /// drifts, and this one had: it omitted `hwkey` and `pane`, so two verbs
    /// that really are refused while a hold is on were documented as answerable.
    ///
    /// Both directions. A verb missing from the prose UNDERSTATES the halt — the
    /// dangerous side, since a driver reads the list to plan what it can still
    /// do. A verb in the prose but not in the set overstates it, stranding a
    /// caller that waits for a refusal which never comes.
    #[test]
    fn the_hold_help_enumerates_exactly_the_pty_reaching_set() {
        let detail = aterm_types::control_verbs::spec("hold")
            .expect("`hold` is a catalog verb")
            .detail;
        let listed: std::collections::BTreeSet<&str> = detail
            .split_once("from ANY scope — `")
            .and_then(|(_, tail)| tail.split_once('`'))
            .expect("the help quotes the roster in one backticked run")
            .0
            .split_whitespace()
            .collect();
        assert!(!listed.is_empty(), "the roster parse found nothing");
        for verb in &listed {
            assert!(
                is_pty_reaching(verb),
                "`hold`'s help lists `{verb}` as PTY-reaching, but `is_pty_reaching` \
                 does not — a caller waits for a refusal that never comes"
            );
        }
        // The other direction needs the candidate set: every verb the catalog
        // knows, so a NEW pty-reaching verb fails here on arrival.
        for spec in aterm_types::control_verbs::VERBS {
            if is_pty_reaching(spec.name) {
                assert!(
                    listed.contains(spec.name),
                    "`{}` is refused while a hold is on and `hold`'s help does not \
                     say so — the roster understates the halt, which is the side a \
                     driver plans against",
                    spec.name
                );
            }
        }
    }
    use super::*;
    use crate::session_store::{Store, new_store, test_handle};

    /// The instance bridge link is process-global (one bridge per instance), so
    /// every test that reads or moves it goes through [`with_link_reset`] and
    /// starts from `absent`. Everything else here is per-session and parallel.
    use super::with_link_reset as with_link;

    /// A registered session: the real `SessionHandle` the store builds, so these
    /// tests drive the same `ctx.fabric` the dispatch reaches.
    fn registered(store: &Store) -> (String, std::sync::Arc<SessionCtx>) {
        registered_as(store, 1)
    }

    /// [`registered`] with an explicit `local_id`, for the tests that need two
    /// sessions in one store — the store lists by `local_id`, so this is also
    /// what fixes the order `outbox` drains them in.
    fn registered_as(store: &Store, local_id: u64) -> (String, std::sync::Arc<SessionCtx>) {
        let h = test_handle(local_id);
        let sid = h.sid.as_str().to_string();
        let ctx = h.ctx.clone();
        store.write().unwrap_or_else(|p| p.into_inner()).register(h);
        (sid, ctx)
    }

    fn deliver(store: &Store, sid: &str, off: u64, from: &str, kind: &str, text: &str) -> String {
        cmd_deliver(
            store,
            &format!("{sid} off={off} from={from} kind={kind} trust=agent text={text}"),
        )
    }

    /// The header's `<n>` and the rows that follow it.
    fn rows(reply: &str) -> Vec<&str> {
        reply.lines().skip(1).collect()
    }

    /// `Lines` framing is a hard client contract: `OK <n>` must count EVERY row
    /// that follows, message rows and un-landed post rows alike. A header that
    /// counted only messages would make the client stop reading mid-reply and
    /// leave the post rows in the stream, to be parsed as the NEXT reply.
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
        // The header carries every field §11.2 names, in order.
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

    /// EXACTLY-ONCE AT THE SINK. The bus commits its group cursor only after
    /// `deliver` answers `OK`, so a bridge that dies in that window redelivers the
    /// same offset. Answering the id it first got — and appending nothing — is
    /// what turns at-least-once delivery into exactly-once at the endpoint.
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
        // A DIFFERENT body at the same offset is still the same record: the
        // offset is the log's, not the sender's, so there is nothing to reconcile.
        assert_eq!(
            deliver(&store, &sid, 90312, "h-a", "ask", "two"),
            "OK 1\n",
            "the offset decides, not the payload"
        );
        assert_eq!(rows(&cmd_inbox(&ctx, "--peek")).len(), 1);
        // A different offset is a different message.
        assert_eq!(deliver(&store, &sid, 90313, "h-a", "ask", "next"), "OK 2\n");
        assert_eq!(rows(&cmd_inbox(&ctx, "--peek")).len(), 2);
    }

    /// TWO WATERMARKS, and the difference is the whole ergonomics of the drain: a
    /// bare `inbox` moves the LISTED mark (the agent has now had its chance to
    /// read the row, so it stops holding the ring hostage), `inbox seen` moves the
    /// HANDLED mark (`seen=`, the durable one), and `--peek` moves neither — which
    /// is what lets a hook print the header into a prompt without consuming mail.
    #[test]
    fn the_watermarks_move_only_where_they_should() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        for off in 1..=3 {
            deliver(&store, &sid, off, "h-a", "task", "x");
        }
        // --peek: nothing moves.
        assert!(cmd_inbox(&ctx, "--peek").starts_with("OK 3 hold=0 holder=- seen=0 "));
        assert!(cmd_inbox(&ctx, "--peek").starts_with("OK 3 hold=0 holder=- seen=0 "));
        {
            let inbox = ctx.fabric.lock();
            assert!(inbox.rows.iter().all(|r| !r.listed), "--peek listed a row");
        }
        // A bare listing marks every row LISTED but moves no SEEN.
        assert!(cmd_inbox(&ctx, "").contains(" seen=0 "));
        {
            let inbox = ctx.fabric.lock();
            assert!(inbox.rows.iter().all(|r| r.listed));
        }
        // `inbox seen` moves SEEN, is monotone, and rejects an id it never held.
        assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=2\n");
        assert_eq!(
            cmd_inbox_seen(&ctx, "1 deferred"),
            "OK seen=2\n",
            "the handled watermark never goes backwards"
        );
        assert_eq!(cmd_inbox_seen(&ctx, "99"), "ERR no such message\n");
        assert_eq!(cmd_inbox_seen(&ctx, "2 shrugged"), INBOX_USAGE);
        assert!(cmd_inbox(&ctx, "--peek").contains(" seen=2 "));
        // `since=` selects, `<n>` keeps the newest, and what a bounded reply did
        // not carry is `pending=`.
        assert_eq!(rows(&cmd_inbox(&ctx, "since=2 --peek")).len(), 1);
        let one = cmd_inbox(&ctx, "1 --peek");
        assert!(one.starts_with("OK 1 "), "{one}");
        assert!(rows(&one)[0].starts_with("msg 3 "), "newest kept: {one}");
    }

    /// The ring is bounded and drop-oldest, and an eviction of a row the agent
    /// never saw is REPORTED. A message that vanished without a trace is worse
    /// than one refused, so `dropped=` is monotone and every later drain carries
    /// it — the agent does not have to be looking on the wake it happened.
    #[test]
    fn the_ring_is_bounded_and_says_what_it_dropped() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        // One sender per row keeps the per-sender quota out of this test.
        for off in 1..=u64::try_from(RING_CAP).expect("cap fits") {
            let from = format!("a-svc{off}");
            assert!(deliver(&store, &sid, off, &from, "note", "x").starts_with("OK "));
        }
        assert!(cmd_inbox(&ctx, "--peek").contains(" dropped=0 "));
        let over = u64::try_from(RING_CAP).expect("cap fits") + 1;
        deliver(&store, &sid, over, "a-late", "note", "x");
        let reply = cmd_inbox(&ctx, "--peek");
        assert!(
            reply.contains(" dropped=1 "),
            "{}",
            &reply[..80.min(reply.len())]
        );
        assert_eq!(rows(&reply).len(), RING_CAP, "the ring stays at its cap");
        assert!(
            !reply.contains("from=a-svc1 "),
            "the OLDEST row is the one that left"
        );
    }

    /// THE PER-SENDER QUOTA and THE EVICTION CLASS, together — they exist for one
    /// scenario, so they are tested in it: a peer floods with `note`s while a
    /// human's `task` waits unread. The 65th note is refused at the door (the
    /// flood never reaches the ring), and the human's row is still readable
    /// afterwards, because eviction takes an agent's row before a human's.
    #[test]
    fn a_flood_of_notes_can_never_evict_a_humans_unread_task() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert_eq!(
            deliver(&store, &sid, 1, "h-andrew", "task", "stop"),
            "OK 1\n"
        );
        let mut refused = 0usize;
        let mut accepted = 0usize;
        for off in 2..=601 {
            match deliver(&store, &sid, off, "s-peer@n-b", "note", "spam").as_str() {
                "ERR quota\n" => refused += 1,
                ok if ok.starts_with("OK ") => accepted += 1,
                other => panic!("unexpected deliver reply {other:?}"),
            }
        }
        assert_eq!(
            accepted, SENDER_QUOTA,
            "exactly the quota lands; the 65th note from that sender is refused"
        );
        assert_eq!(refused, 600 - SENDER_QUOTA);
        let reply = cmd_inbox(&ctx, "--peek");
        assert!(
            reply.contains("from=h-andrew kind=task"),
            "the human's task survived 600 notes"
        );
        assert!(reply.contains(" dropped=0 "), "nothing was evicted at all");
        // The quota counts UNLISTED rows: once the agent has drained, the same
        // sender may speak again.
        cmd_inbox(&ctx, "");
        assert!(deliver(&store, &sid, 700, "s-peer@n-b", "note", "again").starts_with("OK "));
    }

    /// Eviction order, stated directly: with the ring full of agent rows and one
    /// human row at the OLDEST position, the row that leaves is an agent's.
    #[test]
    fn eviction_never_takes_a_human_row_ahead_of_an_agents() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert_eq!(
            deliver(&store, &sid, 1, "h-andrew", "task", "first"),
            "OK 1\n"
        );
        for off in 2..=u64::try_from(RING_CAP).expect("cap fits") {
            deliver(&store, &sid, off, &format!("a-svc{off}"), "note", "x");
        }
        deliver(&store, &sid, 9999, "a-new", "note", "x");
        let reply = cmd_inbox(&ctx, "--peek");
        assert!(
            reply.contains("from=h-andrew"),
            "the human row is the OLDEST and still must not be the one evicted"
        );
        assert!(!reply.contains("from=a-svc2 "), "the oldest AGENT row left");
        assert!(reply.contains(" dropped=1 "));
    }

    /// A body larger than the inline preview is cut with `more=1` and read whole
    /// through `inbox get`; `--meta` drops `text=` entirely, which is what makes a
    /// hook able to print the header into an agent's context with no body in it.
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
        // Reading a body moves nothing.
        assert!(cmd_inbox(&ctx, "--peek").contains(" seen=0 "));
    }

    /// `trust=` is the RECEIVER's verdict and the endpoint refuses a label it does
    /// not know, rather than passing an uninterpreted token to an agent as if it
    /// meant something. Same for `kind=` and for a `from=` that is not a
    /// class-prefixed principal — a `from=` field is read as IDENTITY, so it must
    /// never be able to carry a sentence.
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
        // The conservative default: an unstated trust is `agent`, never `human`.
        assert_eq!(bad("off=2 from=h-a kind=task text=x"), "OK 1\n");
        let reply = cmd_inbox(&_ctx, "--peek");
        let row = rows(&reply)[0];
        assert!(row.contains(" trust=agent"), "{row}");
    }

    /// The hold gate, as the dispatch reads it: the §5.3 verb set is refused and
    /// everything else is not. SCOPE-BLIND by construction — `halt_refusal` has no
    /// scope parameter to check — because a halt only the unprivileged obey is
    /// decoration, and the point of putting `hold` behind the bridge is that the
    /// halted party is not the one who lifts it.
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
        assert_eq!(
            cmd_hold(
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
            "resize",
            // `focus` writes the DEC 1004 focus reports to the PTY (`input.rs`,
            // the SOLE focus-report egress) and sat outside this set for two
            // rungs while the `hold` help claimed every PTY-reaching verb was
            // refused.
            "focus",
            "signal",
            "turn",
            "close",
            // `invoke` and `tab` are `Target::App` and never resolve a session,
            // so in production they are refused by `app_halt_refusal` rather than
            // here — but it is the SAME set, and this is the function that decides
            // membership. `tab` is in it for the THIRD clause of §5.3's sentence:
            // `tab close [N]` retires a session, exactly as `close` does.
            "invoke",
            "hwkey",
            "tab",
            "operator-propose-bin",
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
        // THE EXEMPTIONS, and each one is load-bearing: a halted agent must still
        // be able to ask why it is halted, mark the notice read, escalate, take
        // the coordination lease, and look at its own screen.
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
            // `spawn` MINTS a session rather than driving or retiring one; the
            // residual it leaves is named in `is_pty_reaching`'s doc.
            "spawn",
        ] {
            assert!(
                halt_refusal(&ctx, verb).is_none(),
                "{verb} must stay answerable under a halt"
            );
        }
        // And the exempt verbs really answer. The `ask` is the one that matters:
        // it defaults to `--wait`, and with no bridge attached it reports the
        // MISSING BRIDGE — never `ERR halted`, which is the whole exemption.
        assert_eq!(
            cmd_post(&ctx, "to=h-andrew kind=ask why am I halted?", None),
            "ERR fabric absent id=1 queued=1\n"
        );
        assert_eq!(
            cmd_post(&ctx, "to=h-andrew kind=note halted", None),
            "OK 2\n"
        );
        assert_eq!(
            deliver(&store, &sid, 4, "h-andrew", "note", "wait"),
            "OK 1\n"
        );
        assert_eq!(cmd_inbox_seen(&ctx, "1 deferred"), "OK seen=1\n");
        assert!(cmd_inbox(&ctx, "--peek").contains(" hold=1 "));

        assert_eq!(cmd_hold(&store, &format!("{sid} off")), "OK hold=0\n");
        assert!(halt_refusal(&ctx, "turn").is_none(), "the halt lifted");
        assert_eq!(cmd_hold(&store, &format!("{sid} sideways")), HOLD_USAGE);
        assert_eq!(
            cmd_hold(&store, &format!("{sid} on origin=nowhere")),
            HOLD_USAGE
        );
        assert_eq!(cmd_hold(&store, "s-nope on"), "ERR no such session\n");
    }

    /// A halt reason reaches three wire surfaces verbatim, so it is rebuilt from
    /// ASCII-graphic bytes rather than trusted: no control bytes, no bidi
    /// overrides, no newline that could forge a second reply line, bounded, and
    /// never cut through a `%XX` escape.
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
        // Cut exactly where an escape would be halved: both halves go.
        let straddle = format!("{}%41", "y".repeat(REASON_MAX - 2));
        let out = reason_token(&straddle);
        assert!(!out.ends_with('%'), "{out}");
        assert_eq!(out.len(), REASON_MAX - 2, "{out}");
    }

    /// THE SAFETY PROPERTY. Killing the bridge must be strictly worse for a
    /// prompt-injected agent than leaving it alone, which is true exactly when the
    /// halt does not depend on that process staying alive. So: deliver through the
    /// bridge, lose the bridge, and every session it governed is held with
    /// `reason=fabric-lost` while the instance reports `fabric=disconnected`.
    #[test]
    fn losing_the_bridge_halts_every_session_it_governed() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            // A session the bridge NEVER touched is not swept up by its death.
            let untouched = test_handle(2);
            let untouched_ctx = untouched.ctx.clone();
            store
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .register(untouched);

            assert_eq!(fabric_state(), "absent");
            let generation = next_bridge_generation();
            bridge_attached(generation);
            assert_eq!(fabric_state(), "connected");
            assert_eq!(deliver(&store, &sid, 7, "h-a", "task", "go"), "OK 1\n");
            assert!(halt_refusal(&ctx, "turn").is_none());

            assert_eq!(bridge_lost(&store, generation), 1, "one governed session");
            assert_eq!(fabric_state(), "disconnected");
            assert_eq!(
                halt_refusal(&ctx, "turn").as_deref(),
                Some("ERR halted reason=fabric-lost origin=fleet\n")
            );
            assert!(
                halt_refusal(&untouched_ctx, "turn").is_none(),
                "a session the bridge never governed is left alone"
            );
            // Unconditional: it does not matter whether a hold was standing, and
            // losing the bridge twice is idempotent rather than an escalation.
            assert_eq!(bridge_lost(&store, generation), 1);
            assert!(cmd_inbox(&ctx, "--peek").contains(" hold=1 "));
        });
    }

    /// `hold` is governance whichever way it goes, so a session the bridge only
    /// ever LIFTED a halt on is still one it governed — and is halted again when
    /// the bridge dies. The alternative would let a bridge free a session and then
    /// be killed, leaving it running with nobody watching.
    #[test]
    fn a_session_the_bridge_only_unheld_is_still_governed() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            bridge_attached(generation);
            assert_eq!(cmd_hold(&store, &format!("{sid} off")), "OK hold=0\n");
            assert_eq!(bridge_lost(&store, generation), 1);
            assert!(halt_refusal(&ctx, "turn").is_some());
        });
    }

    /// `await inbox` is MONOTONE and kind-filtered: a row the agent already knows
    /// about cannot latch the same wait twice, a `note` does not wake anybody by
    /// itself, and a timeout answers `OK timeout` — the reply every `await` form
    /// gives, which the client exits 124 on.
    #[test]
    fn await_inbox_latches_forward_only_and_ignores_notes_by_default() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert_eq!(deliver(&store, &sid, 1, "h-a", "task", "old"), "OK 1\n");
        // An OLDER row does not latch a wait that starts past it.
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
        // A hold TRANSITION latches, and only when the caller listed `hold`. The
        // transition is measured from the moment the wait armed, so a halt that
        // was already standing does not latch — a wait is for news, and the caller
        // could have read `status hold=` before parking.
        assert_eq!(
            cmd_hold(&store, &format!("{sid} on reason=x")),
            "OK hold=1\n"
        );
        assert_eq!(
            cmd_await_inbox(&ctx, &["since=3", "kinds=hold"], 5),
            "OK timeout\n",
            "a halt already in force is not news"
        );
        let waiter = {
            let ctx = ctx.clone();
            std::thread::spawn(move || cmd_await_inbox(&ctx, &["since=3", "kinds=hold"], 30_000))
        };
        // FLIP UNTIL IT LATCHES, rather than flipping once and hoping the waiter
        // armed first: whenever it armed, the next flip is a change from the value
        // it captured, so this terminates without a sleep and without assuming an
        // interleaving. (`apply_hold` signals only on a REAL change, which is what
        // makes a re-flip necessary and a spin harmless.)
        let mut on = true;
        while !waiter.is_finished() {
            let arg = if on { "on reason=x" } else { "off" };
            cmd_hold(&store, &format!("{sid} {arg}"));
            on = !on;
            std::thread::yield_now();
        }
        let latched = waiter.join().expect("the waiter finished");
        assert!(
            latched == "OK inbox hold=1\n" || latched == "OK inbox hold=0\n",
            "a hold TRANSITION latches the wait, not the timeout: {latched}"
        );
        cmd_hold(&store, &format!("{sid} off"));
        assert_eq!(
            cmd_await_inbox(&ctx, &["since=3", "kinds=task"], 5),
            "OK timeout\n"
        );
        // Usage.
        assert!(cmd_await_inbox(&ctx, &[], 5).starts_with("ERR usage"));
        assert!(cmd_await_inbox(&ctx, &["since=1", "kinds=nope"], 5).starts_with("ERR usage"));
    }

    /// `await inbox` is EVENT-DRIVEN, not polled: a delivery on another thread
    /// releases the parked wait through the condvar. No sleep, no interval —
    /// the test synchronizes on the wait's own answer.
    #[test]
    fn await_inbox_wakes_on_a_delivery_from_another_thread() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        let waiter = {
            let ctx = ctx.clone();
            std::thread::spawn(move || cmd_await_inbox(&ctx, &["since=0"], 30_000))
        };
        // Deliver until the waiter has parked; the FIRST delivery it sees ends it,
        // and the retry loop needs no sleep because `deliver` is cheap and the
        // condvar signal is not edge-triggered — the row stays in the ring.
        assert_eq!(deliver(&store, &sid, 1, "h-a", "task", "go"), "OK 1\n");
        assert_eq!(waiter.join().expect("the waiter finished"), "OK inbox 1\n");
    }

    /// `post` queues an outbound row, lists it as un-landed, and closes it when
    /// the bridge reports the offset it landed at — which is the correlation id an
    /// answer comes back on. `--wait` parks until that report; with no bridge
    /// attached it says so AT ONCE and names the id, because waiting for a landing
    /// nothing can report is a guaranteed timeout, and the post IS queued.
    #[test]
    fn a_post_is_listed_until_the_bridge_reports_where_it_landed() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            assert_eq!(
                cmd_post(&ctx, "to=@s-b kind=note hello there", None),
                "OK 1\n"
            );
            assert!(
                cmd_inbox(&ctx, "--peek").contains("post 1 to=@s-b kind=note off=-"),
                "an un-landed post is listed so the agent sees what is in flight"
            );
            // `--wait` is ON by default for ask/task — the kinds whose whole point
            // is a reply — and with no bridge it refuses instead of parking.
            let reply = cmd_post(&ctx, "to=@s-b kind=ask where?", None);
            assert_eq!(reply, "ERR fabric absent id=2 queued=1\n");
            assert!(
                cmd_inbox(&ctx, "--peek").contains("post 2 to=@s-b kind=ask off=-"),
                "the refused wait still queued the post"
            );
            // The bridge reports the landing; the row leaves the un-landed list and
            // a later answer resolves `re=` to the local post id.
            assert_eq!(
                cmd_deliver(&store, &format!("{sid} landed=2 off=90355")),
                "OK\n"
            );
            assert!(!cmd_inbox(&ctx, "--peek").contains("post 2 "));
            assert_eq!(
                cmd_deliver(
                    &store,
                    &format!(
                        "{sid} off=90400 from=s-b@n-x kind=answer re=90355 trust=agent text=ok"
                    )
                ),
                "OK 1\n"
            );
            let reply = cmd_inbox(&ctx, "--peek");
            let row = rows(&reply)[0];
            assert!(row.contains(" re=90355 re-id=2 "), "{row}");
        });
    }

    /// A parked `post --wait` is released by the bridge's landing report, on
    /// another thread, through the same condvar the inbox uses.
    #[test]
    fn post_wait_is_released_by_the_landing_report() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            bridge_attached(next_bridge_generation());
            let poster = {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    cmd_post(&ctx, "to=h-andrew kind=task --wait=30000 ship it", None)
                })
            };
            // The post id is minted before the wait parks, so the landing can be
            // reported as soon as the row exists — retry until it does, with no
            // sleep: the loop is bounded by the poster's own 30 s wait.
            loop {
                let reply = cmd_deliver(&store, &format!("{sid} landed=1 off=42"));
                if reply == "OK\n" {
                    break;
                }
                assert_eq!(reply, "ERR no such post\n");
                std::thread::yield_now();
            }
            assert_eq!(poster.join().expect("the poster finished"), "OK 1 off=42\n");
        });
    }

    /// `post`'s grammar: the kinds are closed, the address must be a principal,
    /// there is no `to=fleet` (a node holds no fleet write grant — an agent may
    /// only ASK a human to halt), and the body may contain anything, `=` and
    /// spaces included, because options LEAD and the first non-option token begins
    /// the text.
    #[test]
    fn post_refuses_what_it_cannot_address() {
        let store = new_store();
        let (_sid, ctx) = registered(&store);
        assert!(cmd_post(&ctx, "to=@s-b kind=shout hi", None).starts_with("ERR usage"));
        assert!(cmd_post(&ctx, "kind=note hi", None).starts_with("ERR usage"));
        assert!(cmd_post(&ctx, "to=@s-b kind=note", None).starts_with("ERR usage"));
        assert!(cmd_post(&ctx, "to=nonsense kind=note hi", None).starts_with("ERR usage"));
        assert_eq!(
            cmd_post(&ctx, "to=fleet kind=control halt", None),
            "ERR denied: no to=fleet — an agent may only ask a human to halt\n"
        );
        // OPTIONS LEAD, and the first token that is not one begins the body — so a
        // body may carry `=` and spaces with no quoting, and a line that is
        // options all the way down has no body at all (refused, not silently
        // posted empty).
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
        // Over the inline cap, and the frame form.
        let long = "x".repeat(POST_INLINE_MAX + 1);
        assert_eq!(
            cmd_post(&ctx, &format!("to=h-a kind=note {long}"), None),
            "ERR too large\n"
        );
        assert_eq!(
            cmd_post(&ctx, "to=h-a kind=note len=4", None),
            "ERR usage: post len=<n> must be followed by <n> raw bytes\n"
        );
        assert_eq!(
            cmd_post(&ctx, "to=h-a kind=note len=4", Some(b"body".to_vec())),
            "OK 2\n"
        );
    }

    /// The events digest carries the fabric transitions and NO BODY — a count, an
    /// offset, an address. That is the whole discipline: the digest says mail
    /// exists, and reading it is a separate, deliberate `inbox` call, so a watcher
    /// can never be handed message text it did not ask for.
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
            cmd_hold(&store, &format!("{sid} on reason=main%20broken"));

            let events: Vec<(String, String)> = ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .since(None)
                .map(|e| (e.kind.to_string(), e.payload.clone()))
                .collect();
            // The lifecycle kinds a registered session already records (`spawned`)
            // are not this test's subject; the FABRIC five are, in order.
            let kinds: Vec<&str> = events
                .iter()
                .map(|(k, _)| k.as_str())
                .filter(|k| fabric_event_wire_kind(k).is_some())
                .collect();
            assert_eq!(
                kinds,
                ["inbox", "inbox-seen", "post", "post-landed", "hold"],
                "the five fabric kinds, in the order they happened"
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
            assert_eq!(by("inbox-seen"), "1 off=11");
            assert_eq!(by("post"), "1 to=h-a kind=note");
            assert_eq!(by("post-landed"), "1 off=90");
            assert_eq!(by("hold"), "1 reason=main%20broken origin=fleet");
        });
    }

    /// The ring's bound, bound to the DERIVED model `ty` proves (Tier 1). The
    /// model's invariant is that the live window never exceeds `Cap`; this drives
    /// the real inbox past its cap and checks the projection of its state onto the
    /// model's variables. The negative control is the `Cap` value itself: read
    /// from the model, so a model whose bound changed fails here rather than
    /// passing vacuously against a hard-coded number.
    #[test]
    fn the_real_ring_conforms_to_the_derived_bound_model_inbox_hold() {
        let m = aterm_spec::derive::ring_model();
        let cap = m
            .consts
            .iter()
            .find(|(name, _)| *name == "Cap")
            .map(|(_, v)| *v)
            .expect("the derived ring model declares Cap");
        assert!(cap >= 1, "non-vacuity: a zero cap would prove nothing");
        assert_eq!(
            m.invariants[0].name, "LenBounded",
            "the property bound here is the derived model's invariant"
        );

        let store = new_store();
        let (sid, ctx) = registered(&store);
        let pushes = u64::try_from(RING_CAP).expect("cap fits") + 37;
        for off in 1..=pushes {
            deliver(&store, &sid, off, &format!("a-s{off}"), "note", "x");
        }
        let inbox = ctx.fabric.lock();
        // `seq` = rows ever pushed, `lo` = the oldest still live: the model's two
        // variables, projected onto the real ring.
        let seq = inbox.next_msg_id;
        let lo = inbox.rows.front().map(|r| r.id).expect("non-empty");
        assert_eq!(seq, pushes, "every delivery minted exactly one row");
        let live_window = seq - lo + 1;
        assert!(
            live_window <= u64::try_from(RING_CAP).expect("cap fits"),
            "LenBounded violated: live window {live_window} exceeds the ring cap {RING_CAP}"
        );
        assert_eq!(
            inbox.rows.len(),
            RING_CAP,
            "the live set is exactly the window it is bounded to"
        );
        assert!(lo > 1, "non-vacuity: eviction actually ran");
    }

    // -----------------------------------------------------------------------
    // outbox / outbox sent — the outbound mirror of deliver / inbox seen
    // -----------------------------------------------------------------------

    /// Parse one `outbox` reply into `(header, [(fields, body)])`. Written as a
    /// real parser rather than a substring check because the framing IS the
    /// claim: a length prefix, then a `len=`-terminated line and exactly that
    /// many body bytes, repeated. A reader that got the arithmetic wrong here
    /// would silently read the next post's header as body.
    fn parse_outbox(reply: &str) -> (usize, Vec<(String, String)>) {
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
        let mut rest = payload;
        while !rest.is_empty() {
            let (line, tail) = rest.split_once('\n').expect("a post line");
            let len: usize = line
                .rsplit_once(" len=")
                .and_then(|(_, n)| n.parse().ok())
                .unwrap_or_else(|| panic!("no len= on {line:?}"));
            assert!(tail.len() >= len, "the frame promised {len} body bytes");
            out.push((line.to_string(), tail[..len].to_string()));
            rest = &tail[len..];
        }
        (declared, out)
    }

    /// THE OUTBOUND HALF, end to end at the endpoint: a `post` is queued with its
    /// body, `outbox` hands the bridge the body, and `outbox sent` retires it —
    /// releasing the parked `--wait` and dropping the retained bytes.
    #[test]
    fn outbox_hands_the_bridge_the_body_and_outbox_sent_retires_it() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert_eq!(
            cmd_post(&ctx, "to=@s-abc kind=note first body", None),
            "OK 1\n"
        );
        assert_eq!(
            cmd_post(&ctx, "to=h-andrew kind=report second", None),
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

        // A PEEK: reading it again yields the same two posts, which is what lets
        // a bridge that died mid-publish resume without a watermark of its own.
        let (_, again) = parse_outbox(&cmd_outbox(&store, ""));
        assert_eq!(again, posts, "outbox moves nothing");

        // Retire the first. It leaves the drain and the `inbox` in-flight rows,
        // and the body is gone from the endpoint.
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

        // Idempotent, and `off=-` is the other verdict.
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

    /// A BODY WITH NEWLINES IS THE REASON THE FRAME IS LENGTH-PREFIXED. The
    /// `inbox` listing's `post` row is `Lines`-framed and could never carry one;
    /// `outbox` can, and this is the case that proves the difference is real
    /// rather than stylistic.
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

    /// REFUSED AT THE DOOR, not evicted. A dropped outbound message has no record
    /// on either side — the sender was told `OK` and the bus never saw it — so a
    /// full outbox is an error the caller can act on, and every message already
    /// queued survives the refusal.
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
        // The byte budget is the OTHER bound, and it binds independently: the
        // largest body `post` accepts, repeated until the budget is exactly
        // spent, is far short of the row count.
        let big = vec![b'x'; BODY_MAX];
        let store2 = new_store();
        let (_s2, ctx2) = registered(&store2);
        let fills = OUTBOX_BYTES_MAX / BODY_MAX;
        assert!(fills < OUTBOX_CAP, "the byte budget must bind first");
        for _ in 0..fills {
            assert!(
                cmd_post(
                    &ctx2,
                    &format!("to=@s-abc kind=note len={}", big.len()),
                    Some(big.clone())
                )
                .starts_with("OK "),
            );
        }
        let refused = cmd_post(&ctx2, "to=@s-abc kind=note tiny", None);
        assert!(
            refused.starts_with("ERR outbox full "),
            "one more byte over the budget is refused: {refused}"
        );
    }

    /// `post --wait` PARKS ON A VERDICT, either one. A landing answers the offset;
    /// an `off=-` retirement answers `ERR undeliverable` rather than letting the
    /// caller sit out the full timeout and learn nothing.
    #[test]
    fn a_parked_post_wakes_on_a_landing_and_on_an_undeliverable_verdict() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            bridge_attached(next_bridge_generation());
            for (post_id, off_tok, expect) in [
                (1u64, "off=91".to_string(), "OK 1 off=91\n".to_string()),
                (
                    2,
                    "off=-".to_string(),
                    "ERR undeliverable id=2\n".to_string(),
                ),
            ] {
                let waiter = {
                    let ctx = ctx.clone();
                    std::thread::spawn(move || {
                        cmd_post(&ctx, "to=@s-abc kind=ask --wait=60000 hello", None)
                    })
                };
                // SYNCHRONIZED ON STATE, not on a sleep: wait for the row to exist
                // and the waiter to be parked on the condvar. The bounded loop is
                // a hang detector, not a performance assertion.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                loop {
                    if ctx.fabric.lock().posts.iter().any(|p| p.id == post_id) {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the post never queued"
                    );
                    std::thread::yield_now();
                }
                // Retire it until the waiter has actually observed the change: the
                // retirement is idempotent, so re-notifying is free and closes the
                // window between "row exists" and "waiter is on the condvar".
                let reply = loop {
                    assert_eq!(
                        cmd_outbox_sent(&store, &format!("{sid} {post_id} {off_tok}")),
                        "OK\n"
                    );
                    if waiter.is_finished() {
                        break waiter.join().expect("the waiter thread");
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the waiter never woke"
                    );
                    std::thread::yield_now();
                };
                assert_eq!(reply, expect);
            }
        });
    }

    /// EVERY SESSION'S QUEUE, in one drain, each row naming its own `sid` — the
    /// bridge holds one connection for the whole instance, so a per-session verb
    /// would cost it a round trip per session on every scheduling round.
    #[test]
    fn outbox_drains_every_session_and_max_bounds_the_batch() {
        let store = new_store();
        let (sid_a, ctx_a) = registered(&store);
        let (sid_b, ctx_b) = registered_as(&store, 2);
        assert_eq!(cmd_post(&ctx_a, "to=h-x kind=note from-a", None), "OK 1\n");
        assert_eq!(cmd_post(&ctx_b, "to=h-x kind=note from-b", None), "OK 1\n");
        let (_, posts) = parse_outbox(&cmd_outbox(&store, ""));
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
        let (_, one) = parse_outbox(&cmd_outbox(&store, "1"));
        assert_eq!(one.len(), 1, "`max` bounds the batch");
        assert_eq!(one[0].1, "from-a");
        assert_eq!(cmd_outbox(&store, "nope"), OUTBOX_USAGE);
    }

    /// `re=`/`dl=`/`via=` reach the bridge on the `outbox` row. They are what the
    /// published record's body says, and the endpoint is the only thing that
    /// knows them — a bridge that could not read them would publish an `answer`
    /// with no `re=`, which is an answer to nothing.
    #[test]
    fn the_outbox_row_carries_the_fields_the_published_body_needs() {
        let store = new_store();
        let (_sid, ctx) = registered(&store);
        assert_eq!(
            cmd_post(
                &ctx,
                "to=@s-abc kind=answer re=90312 dl=240000 via=s-inner ok",
                None
            ),
            "OK 1\n"
        );
        let (_, posts) = parse_outbox(&cmd_outbox(&store, ""));
        let line = &posts[0].0;
        for field in ["kind=answer", "re=90312", "dl=240000", "via=s-inner"] {
            assert!(line.contains(field), "{line} lacks {field}");
        }
    }
    // -----------------------------------------------------------------------
    // R1 audit regressions
    // -----------------------------------------------------------------------

    /// A LATE GUARD MUST NOT REPORT A LIVE BRIDGE DEAD.
    ///
    /// Two `BridgeLostGuard`s exist per bridge and they observe the close at very
    /// different times: the verb lane sees EOF on its next read, the push lane
    /// only on the subscribe loop's 250 ms peer probe, and the supervisor
    /// relaunches after 200 ms. So the replacement can be attached and CONNECTED
    /// before the previous incarnation's second guard finishes unwinding. That
    /// guard used to store `disconnected` unconditionally — over a live bridge,
    /// permanently, because nothing re-asserts the state.
    ///
    /// The consequence is the second half of this test: `post --wait` (ON by
    /// default for `ask` and `task`) short-circuits on `fabric_state()`, so every
    /// correlation id on the instance was lost for messages the live bridge went
    /// on to publish normally.
    #[test]
    fn a_stale_bridge_guard_cannot_report_a_live_replacement_disconnected() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);

            // Incarnation 1: one generation, both lanes.
            let first = next_bridge_generation();
            bridge_attached(first);
            bridge_attached(first);
            assert_eq!(deliver(&store, &sid, 7, "h-a", "task", "go"), "OK 1\n");

            // Its verb lane sees EOF at once.
            assert_eq!(bridge_lost(&store, first), 1);
            assert_eq!(fabric_state(), "disconnected");

            // The supervisor relaunches; incarnation 2 attaches and lifts nothing.
            let second = next_bridge_generation();
            bridge_attached(second);
            assert_eq!(fabric_state(), "connected");

            // NOW incarnation 1's push-lane guard finally unwinds. Its HOLD sweep
            // still runs — fail closed, unconditionally — but the link state is
            // not its to move any more.
            assert_eq!(
                bridge_lost(&store, first),
                1,
                "the hold sweep stays unconditional: a halt a `kill -9` lifts is no halt"
            );
            assert_eq!(
                fabric_state(),
                "connected",
                "a guard from a dead incarnation reported the live one disconnected"
            );

            // And that is what `post --wait` reads. `--wait=0` makes the timeout
            // deterministic: the answer distinguishes "nothing has landed yet"
            // from "there is no bridge", which is the whole point of the gate.
            assert_eq!(
                cmd_post(&ctx, "to=h-andrew kind=note --wait=0 still here", None),
                "ERR timeout id=1\n"
            );

            // The LIVE incarnation's own guard still works, both halves of it.
            assert_eq!(bridge_lost(&store, second), 1);
            assert_eq!(fabric_state(), "disconnected");
            assert_eq!(
                cmd_post(&ctx, "to=h-andrew kind=note --wait=0 gone", None),
                "ERR fabric disconnected id=2 queued=1\n"
            );
        });
    }

    /// A PARKED `post --wait` LEARNS THE BRIDGE DIED instead of sitting out its
    /// whole wait.
    ///
    /// The entry check answers `ERR fabric <state>` when the bridge was already
    /// gone (§11.2 deviation 9), but the wait loop never re-read it, and
    /// `bridge_lost` woke only the sessions in `LINK.touched` — the ones it had
    /// `deliver`ed to or `hold`ed. A session that has only ever POSTED is in
    /// neither set, so its waiter was not even signalled: it sat out the full
    /// `--wait` (up to 600 s) and answered `ERR timeout` for a landing nothing
    /// could ever have reported.
    ///
    /// The session here is deliberately UNTOUCHED by the bridge — no `deliver`,
    /// no `hold` — which is the case that used to hang.
    #[test]
    fn a_parked_post_learns_the_bridge_died_rather_than_timing_out() {
        with_link(|| {
            let store = new_store();
            let (_sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            bridge_attached(generation);

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
            // Wait for the post to be QUEUED — a real happens-before, not a sleep:
            // once the row is in the ring, `cmd_post` is past its queue step and is
            // at (or inside) the wait. Whichever side of the entry check it lands
            // on, the answer below is the same one.
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

    /// `post` MAY NOT CLAIM A VERDICT A BRIDGE RECORDS.
    ///
    /// `expired` and `undeliverable` are what the ASKER's own bridge appends when
    /// a deadline passes or the bus refuses a record (§6.4). `cmd_post` validated
    /// against the nine-kind `KINDS` — the set `deliver` must ACCEPT — so any
    /// in-session agent (Owner, which every `aterm-ctl @self` holds) could forge
    /// one into a peer's inbox for the cost of a single verb call, and nothing
    /// downstream re-checks: `classify_kind` demotes only `task` and `control`, so
    /// it arrives undemoted and indistinguishable from the real verdict.
    ///
    /// The two lists are pinned APART here, so a kind added to `deliver` cannot
    /// silently become postable.
    ///
    /// UNDER [`with_link`], because it POSTS: `ask` and `task` default to
    /// `--wait`, whose reply is read off the process-global link state, and this
    /// test used to read that state unserialized. It passed or failed by whether
    /// a link-state test happened to be inside its own section at the time — a
    /// race that predates the `queued=1` token and has nothing to do with what
    /// this test is about. The guard makes the state deterministically `absent`.
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
        // Every kind a sender MAY claim still works, and the usage line names them.
        for (i, kind) in POSTABLE.iter().enumerate() {
            let id = i + 1;
            let reply = cmd_post(&ctx, &format!("to=@s-peer kind={kind} hi"), None);
            // `ask` and `task` default to `--wait`, and under `with_link` there is
            // deterministically no bridge, so those two are answered `ERR fabric
            // absent id=<n> queued=1`. Either way the post was ACCEPTED and queued
            // — which is what the id in both replies names, and what `queued=1`
            // says in as many words — while the refusal under test is the usage
            // line, which names no id at all.
            let expected = if matches!(*kind, "ask" | "task") {
                format!("ERR fabric absent id={id} queued=1\n")
            } else {
                format!("OK {id}\n")
            };
            assert_eq!(reply, expected, "`post kind={kind}` is one of the seven");
            assert!(POST_USAGE.contains(kind), "POST_USAGE omits {kind}");
        }
        assert!(!POST_USAGE.contains("expired"));
        assert!(!POST_USAGE.contains("undeliverable"));

        // THE TWO LISTS, pinned apart. `deliver` must still ACCEPT both verdict
        // kinds — the receiving bridge writes them — which is why there are two.
        let verdicts: Vec<&&str> = KINDS.iter().filter(|k| !POSTABLE.contains(k)).collect();
        assert_eq!(verdicts, [&"expired", &"undeliverable"]);
        for kind in ["expired", "undeliverable"] {
            assert_eq!(
                cmd_deliver(
                    &store,
                    &format!("{sid} off=1 from=h-a kind={kind} trust=agent text=x")
                ),
                "OK 1\n",
                "`deliver kind={kind}` is the bridge recording a verdict, and stays legal"
            );
            // One offset, one row: the second kind reuses off=1 and dedups.
        }
    }

    /// EVICTION ORDERS BY CLASS FIRST, and a row that leaves unhandled is counted.
    ///
    /// The predicate order was `[listed && !human, listed, !human, any]`, so
    /// predicate 2 matched ANY listed row — a human's included — before predicate 3
    /// ever looked at an unlisted agent row. A ring holding one LISTED human `task`
    /// and 511 UNLISTED agent `note`s therefore evicted the human's on the next
    /// delivery, which is the exact shape of an agent's turn: `inbox` at turn
    /// start lists the human's task, the turn fills the ring, the task disappears.
    /// And `dropped` only counted UNLISTED rows, so the header said `dropped=0`.
    ///
    /// `deliver`'s help ("eviction never drops an `h-*` row ahead of an agent's"),
    /// DESIGN §5 and this module's own comment all promised otherwise.
    #[test]
    fn eviction_takes_an_agent_row_before_a_listed_human_one_and_counts_it() {
        let store = new_store();
        let (sid, ctx) = registered(&store);

        // A human's task arrives and the agent LISTS it at turn start.
        assert_eq!(
            deliver(&store, &sid, 1, "h-andrew", "task", "stop"),
            "OK 1\n"
        );
        assert!(cmd_inbox(&ctx, "").contains("from=h-andrew"));
        assert!(
            ctx.fabric.lock().rows.iter().all(|r| r.listed),
            "the human's row is listed, which is what used to make it the victim"
        );

        // The turn then fills the ring with UNLISTED agent notes. One sender per
        // row keeps the per-sender quota out of it.
        for off in 2..=u64::try_from(RING_CAP).expect("cap fits") {
            assert!(
                deliver(&store, &sid, off, &format!("a-svc{off}"), "note", "x").starts_with("OK ")
            );
        }
        assert_eq!(ctx.fabric.lock().rows.len(), RING_CAP);

        // The row that leaves is an AGENT's, listed state notwithstanding.
        assert!(deliver(&store, &sid, 9999, "a-late", "note", "x").starts_with("OK "));
        let reply = cmd_inbox(&ctx, "--peek");
        assert!(
            reply.contains("from=h-andrew kind=task"),
            "a LISTED human row was evicted ahead of 511 unlisted agent rows"
        );
        assert!(!reply.contains("from=a-svc2 "), "the oldest agent row left");

        // AND IT IS COUNTED. `dropped` bumps for any row evicted past the HANDLED
        // watermark, not merely an unlisted one.
        assert!(
            reply.contains(" dropped=1 "),
            "an evicted unhandled row must be reported: {}",
            reply.lines().next().unwrap_or("")
        );
    }

    /// A LISTED-AND-HANDLED row is the one eviction may take silently: `inbox seen`
    /// is the agent saying it is done with it, so its loss is not news.
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

    /// `pending=` COUNTS WHAT THE REPLY DID NOT CARRY, which is what the verb
    /// table promises with no watermark qualifier.
    ///
    /// A non-peek `inbox` used to advance a `listed: u64` watermark to the highest
    /// row it KEPT while marking only those rows listed, and `pending` was counted
    /// above that watermark — so every row BELOW the newest one carried became
    /// invisible to it FROM THE NEXT REPLY ON. (The reply that moves the watermark
    /// still counts correctly, because `pending` is computed before the move; it is
    /// every drain after it that lies, which is the worse shape — the agent's next
    /// turn is told `pending=0` over mail it has never seen.) That number is also
    /// what `hook run session-start` injects into a model's context.
    ///
    /// So the second assertion below is the discriminating one.
    #[test]
    fn pending_counts_the_rows_a_bounded_or_filtered_reply_left_behind() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        for off in 1..=10 {
            deliver(&store, &sid, off, "a-svc", "note", "x");
        }
        let header = |reply: &str| reply.lines().next().unwrap_or("").to_string();

        // `since=5` lists rows 6..10 and leaves 1..5 unread.
        let filtered = header(&cmd_inbox(&ctx, "since=5"));
        assert!(filtered.starts_with("OK 5 "), "{filtered}");
        assert!(
            filtered.contains(" pending=5"),
            "five rows below the newest listed one are still unread: {filtered}"
        );

        // The NEXT drain is the one the watermark lied to: rows 1..5 are still
        // unlisted and still uncarried, and the watermark has already moved past
        // them.
        let bounded = header(&cmd_inbox(&ctx, "1"));
        assert!(bounded.starts_with("OK 1 "), "{bounded}");
        assert!(
            bounded.contains(" pending=5"),
            "the reply carried one row and left five: {bounded}"
        );

        // And a bare `inbox` that carries everything really is zero.
        assert!(header(&cmd_inbox(&ctx, "")).contains(" pending=0"));
    }

    /// THE DOC'S CITATION RESOLVES. `is_pty_reaching`'s comment justifies a
    /// LITERAL by naming the test that derives it, and for two rungs that name
    /// belonged to no test in the tree: an auditor greping it found nothing and
    /// could not tell "the guard was deleted" from "the guard is called something
    /// else". aterm ships no evidence manifest, so a doc comment naming its own
    /// guard IS the citation, and a citation that does not resolve is the defect.
    #[test]
    fn the_halt_set_doc_cites_a_test_that_exists() {
        let src = include_str!("fabric.rs");
        let (production, tests) = src
            .split_once("\n#[cfg(test)]\nmod inbox_hold {")
            .expect("fabric.rs has a tests module");
        let cited = "the_halt_set_is_derived_from_the_verb_table";
        assert!(
            production.contains(cited),
            "the halt-set doc no longer cites its derivation test by name"
        );
        assert!(
            tests.contains(&format!("fn {cited}(")),
            "`{cited}` is cited by the halt-set doc but no test carries that name"
        );
        // And the sentence must not re-narrow to the rule that let `tab` through:
        // the derivation walks EVERY row, whatever its target.
        assert!(
            production.contains("walks EVERY [`aterm_types::control_verbs::VERBS`] row"),
            "the doc must state the rule the test actually enforces"
        );
    }

    /// A LATE ATTACH FROM A DEAD LAUNCH CANNOT PIN A LIVE BRIDGE AT
    /// `disconnected`.
    ///
    /// The generation fixed the GUARDS; the ATTACHES were left last-writer-wins,
    /// and they are just as unordered — `attach_fabric_bridge` hands each near end
    /// to a freshly spawned thread and each calls `bridge_attached` itself. So
    /// launch N's second lane could run after launch N+1's first, regress `owner`
    /// to N, and then have its OWN guard match and store `disconnected` over the
    /// live bridge. Nothing re-asserts the state: `status fabric=` lies for the
    /// life of the process and every `post --wait` short-circuits.
    ///
    /// Deterministic: the interleaving is CALLED, not raced.
    #[test]
    fn a_late_attach_from_a_dead_launch_cannot_regress_the_live_generation() {
        with_link(|| {
            let store = new_store();
            let (sid, _ctx) = registered(&store);

            let first = next_bridge_generation();
            bridge_attached(first);
            assert_eq!(deliver(&store, &sid, 1, "h-a", "task", "go"), "OK 1\n");

            // The replacement is up and CONNECTED.
            let second = next_bridge_generation();
            bridge_attached(second);
            assert_eq!(fabric_state(), "connected");

            // NOW the dead launch's second lane finally runs its attach. It must
            // not become the owner again.
            bridge_attached(first);
            assert_eq!(
                fabric_state(),
                "connected",
                "a ghost lane's attach must not regress the owning generation"
            );

            // ...which is the whole point: its guard must still be a ghost.
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

            // The live incarnation's own guard still reports the loss.
            assert_eq!(bridge_lost(&store, second), 1);
            assert_eq!(fabric_state(), "disconnected");
        });
    }

    /// THE GOVERNED-SID SET IS BOUNDED BY THE LIVE REGISTRY, not by uptime.
    ///
    /// `touched` had no removal path at all: one `String` per sid the bridge had
    /// EVER delivered to or held, kept for the life of the process, scanned
    /// linearly on the `deliver` path — i.e. once per inbound message, over every
    /// dead session too. The field's doc argued the container from "a handful of
    /// sids on a path that runs at most once per bridge verb", and `deliver` is a
    /// bridge verb.
    #[test]
    fn the_governed_sid_set_drops_sessions_that_have_left_the_registry() {
        with_link(|| {
            let store = new_store();
            // Past the prune threshold, one governed session per local id.
            let n = TOUCHED_PRUNE_AT + 8;
            let mut sids = Vec::with_capacity(n);
            for local in 1..=n as u64 {
                let (sid, _ctx) = registered_as(&store, local);
                assert_eq!(deliver(&store, &sid, local, "h-a", "note", "x"), "OK 1\n");
                sids.push(sid);
            }
            // MEMBERSHIP, not the global count. `touched` is process-global and
            // 17 sibling tests insert into it OUTSIDE `with_link`'s mutex, so an
            // exact `touched_len()` raced the schedule (2 of 3 full-suite
            // samples failed here, 2026-09-01, while the test was green alone
            // and green single-threaded). This test's own sids are unforgeable
            // by siblings, so membership states the same pruning property,
            // schedule-proof.
            for sid in &sids {
                assert!(
                    touched_contains(sid),
                    "every governed sid is remembered while its session lives"
                );
            }

            // All but the last leave. Nothing has pruned yet — the set still
            // carries every dead sid.
            for local in 1..n as u64 {
                store
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .deregister_local(local);
            }
            for sid in &sids {
                assert!(
                    touched_contains(sid),
                    "a session leaving does not itself prune"
                );
            }

            // The next bridge verb prunes against the live registry.
            let last = sids.last().expect("a session").clone();
            assert_eq!(
                deliver(&store, &last, 9_999, "h-a", "note", "again"),
                "OK 2\n"
            );
            for sid in &sids[..n - 1] {
                assert!(
                    !touched_contains(sid),
                    "the set must shrink to the sessions that still exist"
                );
            }
            assert!(
                touched_contains(&last),
                "the one live session survives the prune"
            );
        });
    }

    /// THE QUOTA COUNTS PEERS, WHICH IS WHAT ITS OWN DOC PROMISES.
    ///
    /// It was counted over the whole `from=` string, and half of that string is
    /// the sending NODE's word: `render_from` builds `s-<sid>@n-<node>` from the
    /// record BODY's `from=<sid>`, checking only that it LOOKS like a session
    /// principal. `is_principal` admits ~36^32 sids, so one node minted a fresh
    /// 64-row allowance whenever it liked — rotate the pseudo-sid every 64 notes
    /// and `ERR quota` never fires while the ring turns over continuously.
    #[test]
    fn one_peer_cannot_mint_extra_quota_by_rotating_the_sid_it_claims() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert_eq!(
            deliver(&store, &sid, 1, "h-andrew", "task", "stop"),
            "OK 1\n"
        );

        // One node, a different claimed session on every single message.
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

        // A DIFFERENT node is unaffected: the key is the cap-forced part.
        assert!(
            deliver(&store, &sid, 900, "s-a@n-other", "note", "hello").starts_with("OK "),
            "an honest peer must not be charged for the rogue's flood"
        );
        // And so is a human, who has no `@` at all.
        assert!(deliver(&store, &sid, 901, "h-andrew", "note", "hi").starts_with("OK "));
        assert!(cmd_inbox(&ctx, "--peek").contains("from=h-andrew kind=task"));
    }

    /// AN ACK RELEASES THE SENDER'S QUOTA, which is why `inbox seen` lists the
    /// rows at or below its argument as well as advancing the handled watermark.
    ///
    /// Undocumented until now, and LOAD-BEARING: the file-plane mirror lists with
    /// `--peek` and never runs a bare `inbox`, so this side of `inbox seen` is the
    /// only thing that keeps a mirrored session reachable past its 64th message.
    /// A change that (reasonably, per the old help) stopped touching `listed`
    /// would reinstate that deadlock silently.
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
        // A PEEK moves nothing — the mirror's listing shape.
        cmd_inbox(&ctx, "--peek");
        assert_eq!(
            deliver(&store, &sid, 101, "s-p@n-peer", "note", "x"),
            "ERR quota\n",
            "`--peek` must not relieve the quota"
        );
        // The ACK does, because it lists every row at or below the id.
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

    /// A BRIDGE-TRUNCATED BODY IS DISTINGUISHABLE FROM A COMPLETE ONE, at the one
    /// verb an agent is told to read bodies with.
    ///
    /// The bridge cuts a body that will not fit one control request line and names
    /// the true size in `len=`. `inbox get`'s help promised "the FULL body" and its
    /// reply carried no marker at all, so an agent that followed the documented
    /// route — the route the hook banner prints — read three quarters of a report
    /// as the whole of it. The rest is on the bus, which the recipient cannot
    /// reach, so the answer has to SAY SO; it cannot fetch it.
    #[test]
    fn a_bridge_truncated_body_says_so_in_the_row_and_in_inbox_get() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        // A SHORT surviving body under `TEXT_PREVIEW_MAX` with a much larger
        // declared `len=`: the case that used to read as a complete short message.
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

        // And a body that arrived WHOLE keeps the plain header — the marker means
        // something because it is not always there.
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

    /// A `post --wait` REFUSED FOR WANT OF A LINK SAYS THE MESSAGE IS STILL
    /// QUEUED, because it is.
    ///
    /// A bridge EXIT is the ordinary relaunch path: the supervisor brings a
    /// replacement up, `outbox` is a peek that removed nothing, and the post is
    /// published seconds later. A bare `ERR fabric disconnected` reads as "not
    /// sent", the remedy for "not sent" is to send again, and `post` carries no
    /// idempotency key — so the peer's inbox ends up holding the same `ask` twice.
    #[test]
    fn a_post_refused_for_want_of_a_link_says_it_is_still_queued() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);

            // The ENTRY check: no bridge has ever attached.
            assert_eq!(
                cmd_post(&ctx, "to=@s-peer kind=ask --wait=0 which branch?", None),
                "ERR fabric absent id=1 queued=1\n"
            );

            // The PER-WAKE check: a bridge attached, took the post's session, and
            // died while the waiter was parked.
            let generation = next_bridge_generation();
            bridge_attached(generation);
            assert_eq!(deliver(&store, &sid, 1, "h-a", "note", "x"), "OK 1\n");
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    // The waiter parks; `bridge_lost` wakes it.
                    assert_eq!(
                        cmd_post(&ctx, "to=@s-peer kind=ask --wait=60000 still?", None),
                        "ERR fabric disconnected id=2 queued=1\n"
                    );
                });
                // Park is guaranteed by the condvar protocol, not by sleeping: the
                // waiter holds the fabric lock while it reads the state, and
                // `bridge_lost` signals under that same lock.
                while ctx.fabric.lock().posts.iter().all(|p| p.id != 2) {
                    std::thread::yield_now();
                }
                bridge_lost(&store, generation);
            });

            // AND THE CLAIM IS TRUE: both posts are still in the outbox, bodies
            // and all, for the replacement bridge to drain.
            let drained = cmd_outbox(&store, "");
            assert!(drained.contains("which branch?"), "{drained}");
            assert!(drained.contains("still?"), "{drained}");
        });
    }

    /// ONE `outbox` DRAIN IS BOUNDED IN BYTES, ACROSS EVERY SESSION.
    ///
    /// `OUTBOX_CAP`/`OUTBOX_BYTES_MAX` bound one SESSION's queue. This reply
    /// concatenated every session's queued bodies into one `String` with no
    /// aggregate bound — `max` counts posts, not bytes, and the only caller passes
    /// none — so the peak cost of a drain was `sessions × 4 MiB`, doubled by the
    /// reply copy and again by the bridge's buffer for the announced length.
    #[test]
    fn one_outbox_drain_is_bounded_in_bytes_across_every_session() {
        let store = new_store();
        // THREE sessions, each filling its own (per-session) 4 MiB queue with the
        // largest body `post` accepts: unbounded, one drain would answer 12 MiB.
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
        // A PEEK, so the rest is not lost — the next drain sees the same queue.
        let again = cmd_outbox(&store, "");
        assert_eq!(
            again.len(),
            drained.len(),
            "a bounded drain retires nothing"
        );
    }

    /// THE HALT SET IS DERIVED FROM THE VERB TABLE — the test
    /// [`super::is_pty_reaching`]'s doc cites, carrying the name it cites, beside
    /// the set it derives.
    ///
    /// THE RULE: every [`aterm_types::control_verbs::VERBS`] row that is not
    /// `OpClass::Read` is either in [`super::is_pty_reaching`] or carries a NAMED,
    /// ARGUED exemption in `HALT_EXEMPT` below. A new writing verb therefore FAILS
    /// THIS TEST rather than shipping outside the halt, and a `Read` row that
    /// somehow appears in the set fails it too — the set is checked in both
    /// directions, because a halt that refused a read verb would be a different
    /// lie.
    ///
    /// EVERY TARGET, WHICH IS THE HALF THAT WAS MISSING. The previous derivation
    /// `continue`d on every row whose target was not `Target::Session`, so the
    /// eight `Target::App` write rows and the `Meta` ones were outside it and
    /// `invoke` — the verb whose absence WAS the previous blocking finding — was
    /// pinned instead by a two-element literal three lines below the loop: the
    /// same "two literals that agree with each other" shape the derivation existed
    /// to remove. `tab` went the whole way through that gap: `tab close [N]`
    /// retires a session, which is the third thing the doc says a halt covers, and
    /// a driver refused `@<sid> close` under a fleet halt simply typed `tab close`
    /// instead.
    ///
    /// THE `OpClass` MATCH IS EXHAUSTIVE ON PURPOSE. A new op class does not slip
    /// past a filter here; it fails to COMPILE until someone decides which side of
    /// the halt it is on.
    #[test]
    fn the_halt_set_is_derived_from_the_verb_table() {
        use aterm_types::control_verbs::{OpClass, VERBS};

        /// Rows that can mutate SOMETHING and still stay answerable under a halt,
        /// each with the reason. A halt stops DRIVERS: it must not stop a halted
        /// agent asking why it is halted, marking the notice seen, or escalating,
        /// and it must not stop the bridge that is the only thing that can LIFT it.
        const HALT_EXEMPT: &[(&str, &str)] = &[
            (
                "pointer",
                "moves aterm's OWN pointer so `hover` can resolve a cell; it synthesizes no \
         mouse event — `mouse` is the verb that writes one to the PTY, and that is in \
         the halt set",
            ),
            (
                "open",
                "opens a native settings surface in the app; it reaches no session and writes \
         no byte to any PTY",
            ),
            (
                "act",
                "dispatches a semantic action against the NATIVE app's own UI surface \
         (`app/v1 view ...`), not against a terminal session",
            ),
            (
                "spawn",
                "CREATES a session rather than driving one, and a halt stops drivers. The new \
         session is itself held the moment the bridge attaches it (`reconcile_halt`), \
         and a human at the glass can always open one anyway — the physical keyboard \
         is not on this seam. RESIDUAL, stated rather than hidden: between the spawn \
         and that attach there is a window in which the new session is ungoverned",
            ),
            // The session lane.
            (
                "lease",
                "takes or reports the keyboard; it puts no bytes on a PTY",
            ),
            (
                "inbox seen",
                "records a decision — §5.3 names it exempt by name",
            ),
            (
                "post",
                "the escalation path: an agent may always ask a human to lift it",
            ),
            (
                "copy",
                "moves the SELECTION to the OS clipboard; it types nothing. It is \
                 half of the screen-to-PTY chain, and the halt cuts that chain at \
                 the other half, `invoke Paste`, which IS refused",
            ),
            // The bridge plane. Halting these would halt the only party that can
            // lift a halt, and the only path by which a halted agent is TOLD.
            (
                "deliver",
                "the notice explaining the halt arrives on this verb",
            ),
            ("hold", "the verb that LIFTS the halt"),
            (
                "outbox sent",
                "retires an outbound post the bridge already published; refusing it \
                 would strand the body and re-publish it forever",
            ),
            // The App lane: native surfaces, not the terminal grid.
            (
                "open",
                "opens a native tab app (settings/markdown/editor); no PTY, no session",
            ),
            (
                "act",
                "dispatches a semantic action against a NATIVE app view, not the grid",
            ),
            (
                "settings",
                "rewrites `aterm.toml`; a greater authority than Write, but it puts \
                 no bytes on a PTY and retires no session",
            ),
            ("rain", "a visual effect on the window"),
            ("hover", "toggles the drop-target highlight"),
            (
                "spawn",
                "MINTS a session rather than driving or retiring one. The residual \
                 — a session created under a standing halt carries no hold of its \
                 own — is recorded in `is_pty_reaching`'s doc, not closed here: \
                 halting `spawn` would refuse a human's `aterm new-tab`, which \
                 arrives on this same seam",
            ),
            // Owner/Meta: privilege and provenance, none of which types.
            ("version", "a build-provenance string"),
            (
                "update",
                "`status`/`check` are reads; `apply` re-execs the INSTANCE, ending \
                 every session and every in-memory hold with it — a residual \
                 recorded in `is_pty_reaching`'s doc, and answered as \
                 `AnyScopeMeta` before any session resolves, which is a different \
                 seam from this one",
            ),
            (
                "help",
                "the catalog; a halted agent must be able to read it",
            ),
            ("verbs", "the `help` alias"),
            (
                "operator",
                "queues, claims and acknowledges operator work. The ACTUATION that \
                 reaches a PTY is `operator-propose-bin`, which IS in the set",
            ),
            ("sessions", "the roster; a read in Owner clothing"),
            ("exits", "the exit ledger; a read in Owner clothing"),
            ("whoami", "reports this connection's own scope"),
            (
                "grant",
                "mints an edge token. The halt is SCOPE-BLIND, so a token minted \
                 under a halt still cannot type into a held session",
            ),
            ("revoke", "removes authority; strictly de-escalating"),
            (
                "connect",
                "mints the session-connection ops (a capability graph edge), not a \
                 live pipe; the minted edge meets the same scope-blind halt",
            ),
            ("disconnect", "dissolves such an edge; de-escalating"),
            ("flows", "reads the connection graph"),
            (
                "raise",
                "raises a window and selects a tab: it moves the human's focus and \
                 puts no bytes anywhere",
            ),
            (
                "dial",
                "relays this connection to a REMOTE aterm, whose own halt governs \
                 what may be done there",
            ),
            ("dial-list", "lists saved peers"),
            ("dial-token", "mints a token for the relay above"),
        ];

        // No stale entries: an exemption for a verb that no longer ships is a
        // sentence nobody will ever re-argue.
        for (verb, why) in HALT_EXEMPT {
            assert!(
                VERBS.iter().any(|s| s.name == *verb),
                "HALT_EXEMPT names `{verb}`, which is not a verb row"
            );
            assert!(!why.is_empty(), "`{verb}`'s exemption carries no argument");
            assert!(
                !is_pty_reaching(verb),
                "`{verb}` is both halted and exempt: the two lists disagree"
            );
        }

        for spec in VERBS {
            let mutates = match spec.op {
                // A read verb observes; it cannot put bytes on a PTY, signal a
                // child or retire a session, and that is what the op class MEANS
                // (the authorization engine maps it to `Op::Read`).
                OpClass::Read => false,
                OpClass::Write
                | OpClass::Signal
                | OpClass::ConfigWrite
                | OpClass::ClipboardWrite
                | OpClass::Owner => true,
            };
            let halted = is_pty_reaching(spec.name);
            if !mutates {
                assert!(
                    !halted,
                    "`{}` is a Read row: a halt that refused it would stop an agent \
                     from looking at the screen that explains why it is halted",
                    spec.name
                );
                continue;
            }
            let exempt = HALT_EXEMPT.iter().any(|(v, _)| *v == spec.name);
            assert!(
                halted != exempt,
                "`{}` ({:?}, {:?}) can mutate: put it in `fabric::is_pty_reaching` \
                 or give it a NAMED, argued exemption in HALT_EXEMPT (it is \
                 currently halted={halted} exempt={exempt})",
                spec.name,
                spec.op,
                spec.target
            );
        }
    }

    /// THE HALT SET COVERS `focus` AND THE APP LANE.
    ///
    /// `focus <in|out>` is a `Write`/`Session` verb whose seam writes `\x1b[I` /
    /// `\x1b[O` to the PTY whenever DEC 1004 focus reporting is on, and it was not
    /// in the set — so a driver holding Owner could poke a held session's
    /// focus-aware TUI while `send` on the same connection answered `ERR halted`.
    ///
    /// `invoke` is worse, because it is not refusable at the session gate at all:
    /// it is `Target::App`, answered before any session is resolved, and
    /// `invoke Paste` writes the OS clipboard into the front tab's PTY —
    /// with `invoke SelectAll` + `copy` choosing those bytes off the session's own
    /// screen first. [`app_halt_refusal`] is its gate.
    #[test]
    fn the_halt_covers_focus_and_the_app_lane_invoke() {
        let store = new_store();
        let (_sid, ctx) = registered(&store);

        for verb in ["focus", "invoke"] {
            assert!(
                is_pty_reaching(verb),
                "{verb} puts bytes on a PTY and must be refused under a halt"
            );
        }

        // Nothing is held: both lanes answer normally.
        assert!(halt_refusal(&ctx, "focus").is_none());
        assert!(app_halt_refusal(&store, "invoke").is_none());

        apply_hold_for_test(
            &ctx,
            Some(Hold {
                reason: "main-broken".to_string(),
                origin: "fleet".to_string(),
            }),
        );
        assert_eq!(
            halt_refusal(&ctx, "focus").as_deref(),
            Some("ERR halted reason=main-broken origin=fleet\n")
        );
        assert_eq!(
            app_halt_refusal(&store, "invoke").as_deref(),
            Some("ERR halted reason=main-broken origin=fleet\n"),
            "a fleet halt must stop the App lane too, or it stops nothing that matters"
        );
        // AND THE APP LANE COVERS `tab`, which is the SECOND thing that lane got
        // wrong. `tab` types nothing — but `tab close [N]` reaches
        // `App::close_tab_via_verb` and RETIRES the tab's session, which is the
        // third of the three acts §5.3 says a halt refuses. A driver answered `ERR
        // halted` on `@<sid> close` simply typed `tab close` instead, and the
        // test that used to sit here asserted the exemption ("`tab` reaches no
        // PTY") and so pinned the gap open.
        assert_eq!(
            app_halt_refusal(&store, "tab").as_deref(),
            Some("ERR halted reason=main-broken origin=fleet\n"),
            "`tab close` retires a session; a halt that lets it through refuses \
             `close` and nothing else"
        );
        // And a halt is still not a general App freeze: an App verb that neither
        // types nor retires anything answers normally.
        assert!(
            app_halt_refusal(&store, "hover").is_none(),
            "`hover` toggles a highlight; a halt must not turn into an App freeze"
        );
    }

    /// THE MODULE PROMISES ONLY WHAT IT DOES. `aterm` ships no evidence manifest,
    /// so these doc comments ARE the claims, and two of them promised more than
    /// the code delivers.
    ///
    /// * A `fabric-lost` hold has NO OPERATOR UNDO. `apply_hold` has exactly two
    ///   production callers — `cmd_hold` (`Access::BridgeOnly`, refused to Owner
    ///   and every edge) and `bridge_lost` — and nothing in the GUI reaches it. So
    ///   the only lift is a bridge that reconnects and issues `hold off`, and an
    ///   operator whose bridge cannot come back has one recovery: restart the
    ///   instance. This module and DESIGN §11.2 both said "a human lifts it at the
    ///   GUI". No such path exists.
    /// * `InboxRow::from` said the sender is rendered "never from anything in the
    ///   body" and then listed `s-<sid>@n-<node>`, the one form whose `s-<sid>@`
    ///   prefix `Bridge::render_from` reads off the record BODY's `from=` token.
    ///   That is the field an agent reads as identity and the one `hook.rs` cites
    ///   when it argues the wake path carries no attacker-controlled text, so the
    ///   stronger-than-true version was the one most likely to be relied on.
    #[test]
    fn the_module_docs_claim_no_lift_and_no_undue_attestation() {
        let src = include_str!("fabric.rs");
        let production = src
            .split_once("\n#[cfg(test)]\nmod inbox_hold {")
            .map(|(p, _)| p)
            .expect("fabric.rs has a test module");

        // The structural fact the doc now states: two production callers, no GUI.
        assert_eq!(
            production.matches("apply_hold(").count(),
            4,
            "`apply_hold` appears exactly four times: its own `fn`, the `#[cfg(test)]` \
             `apply_hold_for_test` shim, and its TWO production callers — `bridge_lost` \
             and `cmd_hold`. A fifth is a lift path this module's docs do not describe"
        );
        for (name, gui) in [
            ("menu.rs", include_str!("menu.rs")),
            ("command_registry.rs", include_str!("command_registry.rs")),
            ("palette.rs", include_str!("palette.rs")),
        ] {
            assert!(
                !gui.contains("fabric::apply_hold") && !gui.contains("apply_hold("),
                "{name} reaches apply_hold: the GUI lift now exists, so say so in \
                 the module header instead of withdrawing the promise"
            );
        }

        // And the narrowed claims are the ones the module makes. Positive pins,
        // not negative ones: the withdrawal itself has to QUOTE the sentence it
        // withdraws, so "the phrase is absent" is not a test that can hold.
        assert!(
            production.contains("THE ONLY LIFT IS A RECONNECTING BRIDGE"),
            "the module header must state the narrow truth, not the GUI lift"
        );
        assert!(
            production.contains("CAP-FORCED, WITH ONE ATTESTED EXCEPTION"),
            "`from=`'s doc must name the exception `aterm-link`'s body.rs names — \
             the `s-<sid>@` prefix IS read off the record body"
        );
    }

    /// THE TEST-ONLY LINK RESET RUNS INSIDE THE LOCK THAT SERIALIZES IT.
    ///
    /// `with_link_reset` exists so a parallel test binary cannot let one test's
    /// view of the process-global `LINK` leak into another's. Its body released
    /// the mutex with an explicit `drop(guard)` and only THEN dropped the reset
    /// guard at scope end — so the reset ran outside the lock, and a test blocked
    /// on the mutex could acquire it, attach a bridge, and have the previous
    /// test's reset clear the link underneath it. The assertion that catches a
    /// re-inversion lives in the guard's own `Drop`, so every user of the helper
    /// exercises it; this test is the one that names it.
    #[test]
    fn the_link_reset_runs_inside_the_lock_that_serializes_it() {
        with_link(|| {
            assert_eq!(fabric_state(), "absent", "a section starts from absent");
            bridge_attached(next_bridge_generation());
            assert_eq!(fabric_state(), "connected");
        });
        assert_eq!(
            fabric_state(),
            "absent",
            "the section reset the link on the way out"
        );
    }
}
