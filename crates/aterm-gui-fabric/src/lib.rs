// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FABRIC ENDPOINT — the per-session inbox ring, the outbound post queue, and
//! the hold gate, below the window crate: aterm-gui re-exports it as
//! `crate::fabric`, so a one-line edit in aterm-gui recompiles none of it.
//! STATE, not I/O: nothing in this module opens a socket, speaks to
//! a broker, or knows a subject exists. The bus lives outside the process, in the
//! `aterm-link serve` child the instance launches; this is the endpoint it delivers
//! INTO and drains posts OUT of, over the ordinary control protocol.
//!
//! Three things live here and nothing else does:
//!
//! * [`SessionFabric`] — one per session ([`FabricSession`]): a bounded inbox ring, this
//!   session's un-landed posts, the two watermarks, and the hold flag. A LEAF lock
//!   like `meta`/`timeline`, with exactly one sanctioned nesting (fabric →
//!   timeline, never reversed) so an events watcher can never see two concurrent
//!   `deliver`s in the opposite order from the ring.
//! * The verb handlers `inbox` / `inbox get` / `inbox seen` / `post` / `deliver` /
//!   `hold`, and the `await inbox` predicate. They are plain functions over a
//!   session ([`FabricSession`], aterm-gui's `SessionCtx`) and, for the two
//!   bridge verbs, the registry ([`Registry`]), so every one of them is directly
//!   testable without an event loop.
//! * `LINK` — the INSTANCE's view of its bridge: connected, stale, stalled,
//!   disconnected, or never launched, plus the set of sessions that bridge has
//!   ever touched. Four of the five are STORED on a report; `stale` alone is
//!   derived at read time, because the thing it names — a bridge that owes an
//!   answer and has gone silent — arrives as no event at all ([`STALE_AFTER`]).
//!
//! ## Why `deliver` is not an Owner verb, and which half of `hold` is
//!
//! `deliver` stamps `from=` and `trust=` on a row an agent reads as an attested
//! human order. Owner scope is what every in-session client already holds
//! (`aterm-ctl @self` is Owner), so an Owner classification would put it inside
//! the blast radius of one prompt injection. It is
//! [`aterm_types::control_verbs::Access::BridgeOnly`] instead: the only caller is
//! the connection the instance handed its own child, and there is no token that
//! opens it. See `Scope::Bridge`.
//!
//! `hold` has TWO ORIGINS and the connection chooses ([`HoldIssuer`]). The
//! FLEET hold (`origin=fleet`) is the bridge's: set, replaced or lifted only
//! from the bridge connection, for the reason above — a fleet halt an injected
//! agent could lift locally would be no halt at all. The LOCAL hold
//! (`origin=local`) is the instance Owner's: the Owner token is the local
//! human's own credential, `hold=1` was always described as a human's halt, and
//! a human at the glass who cannot stop the drivers on their own machine has
//! been given less than the fleet has. So `hold` is `Access::OwnerOnly` and
//! [`cmd_hold`] refuses `origin=fleet` from Owner outright. The check runs
//! under the fabric lock ([`apply_hold`]), so a fleet hold landing between an
//! Owner's read and its write is still the one that stands. And the local hold
//! is the owner's STOP SIGNAL, not a containment stop: Owner is the scope every
//! in-session client holds, so the halted session's own agent can lift it —
//! which is exactly why the fleet hold is a different origin it cannot touch.
//!
//! A SESSION HOLDS A SET OF ORIGINS ([`Holds`]), one slot each: the fleet's
//! halt and the owner's stop stand side by side, each set and lifted on its
//! own, and the session is held while either stands. A fleet halt landing on
//! the owner's local hold used to REPLACE it, and the fleet's lift then left
//! the session unheld — the owner's stop silently gone. Now the fleet's lift
//! leaves it standing, and an Owner act never touches the fleet's slot.
//!
//! THE THIRD ORIGIN IS THE PERSON'S (`origin=person`, Take Over at the window,
//! `docs/DESIGN-owner-surfaces-2026-10-16.md` §4). It is the halt the owner's
//! local stop could not be: no socket names it (`hold … origin=person` is
//! [`HOLD_PERSON_USAGE`]), an Owner or bridge `hold off` lifts its own slot and
//! answers `OK hold=1` beside it, and only the window's Take Over and Hand
//! Back ([`person_hold`], [`HoldIssuer::Person`]) move it. While it — or any
//! hold — stands, the app verbs (`invoke`, `tab`, `pane`, `hwkey`, `pointer`,
//! `confirm`) are refused instance-wide ([`app_halt_refusal`]): that is what
//! keeps an agent from `hwkey`ing the Hand Back shortcut or `pointer`-clicking
//! its pill.
//!
//! ## A dead bridge: an advisory by default, fail closed on request
//!
//! By default since the owner's direction of 2026-09-29 ("stop adding blockers
//! and limits"), a lost bridge HALTS NOTHING: [`bridge_lost`] reports
//! `fabric=disconnected` and leaves every session it governed running with
//! `status hold_advisory=fabric-lost`; a hold that stands — a person's — stays
//! a real halt, since a lost bridge lifts nothing. `[fabric] hold_on_bridge_lost
//! = true` restores the fail-closed sweep this section describes next.
//!
//! The halt must not depend on a killable process staying alive. When the bridge
//! connection closes — the child exited, was killed, or the fd was closed —
//! [`bridge_lost`] applies `hold on reason=fabric-lost origin=fleet` to every
//! session that bridge ever delivered to or held. Killing the bridge therefore
//! HALTS the sessions it was governing; it does not free them. The sweep fills
//! the FLEET slot only: an owner's local hold stands beside it, and stays when
//! a reconnecting bridge lifts the sweep's.
//!
//! THE ONLY LIFT OF A FLEET HOLD IS A RECONNECTING BRIDGE, and that is a
//! narrower promise than this header used to make. [`apply_hold`] has exactly
//! three production callers — [`cmd_hold`], [`bridge_lost`] and the window's
//! [`person_hold`], which names the person's own slot and no other — and there
//! is no menu item, key binding, palette action or config path that clears a
//! fleet or local hold. The
//! Owner token's `hold off` lifts a LOCAL hold and nothing else: against
//! `origin=fleet`, or with only a fleet hold standing, it is `ERR denied`.
//! DESIGN §11.2 and an earlier version of
//! this header both said "a human lifts it at the GUI"; no such path exists, so
//! the sentence is WITHDRAWN rather than left standing as a promise the code
//! does not keep. An operator whose bridge cannot come back (a deleted cap file,
//! a `[fabric] command` that exits at startup) has exactly one recovery, and it
//! is quitting and relaunching `aterm-gui`. That is a real gap, recorded as one:
//! building the lift is a change to the GUI modules, not to this one.
//!
//! ## A seamless update is not a restart
//!
//! It CARRIES every standing hold, both origins, onto the adopted session
//! ([`render_holds`] into the handoff record, [`parse_holds`] and
//! [`SessionFabric::seed_holds`] in the successor, before the session can take
//! a single control verb). Before that carry an automatic update lifted every
//! halt in the process, silently, minutes after it was set (the round-four
//! plan, item 3). A carried FLEET hold keeps the fleet's rule in the
//! successor: its own bridge lifts it only from a broker's `Last` that shows
//! no standing halt (`converge_hold`), so under a dead broker it stays exactly
//! where it was. And a hold that moves after the park has drawn the manifest
//! refuses the Commit ([`SessionFabric::hold_serial`]): the successor would
//! not see it. From the instant the Commit reads that serial to its `_exit`,
//! a hold transition waits instead ([`SessionFabric::fence_hold`]): nothing
//! compares the serial again, so one answered in that stretch would be lost.
//!
//! Nor is a PTY keeper's crash recovery, whose sessions keep their sid and
//! their running agents: the session's keeper META carries its holds, its
//! topics and an id lease ([`KeeperCarry`]) from the last sample delivered to
//! the keeper, which may be older than the pump's quarter-second interval.
//! The recovered session is held again before it
//! answers a verb. A quit is still an end: its BYE closes the sessions, so a
//! relaunch after one carries nothing.
//!
//! ## An end nobody chose carries the OWNER's hold, never the fleet's
//!
//! A crash, or the quit of a restart, a logout or a shutdown, leaves the next
//! launch a layout that names each tab's standing holds
//! (`restore::TerminalLeafRestore::holds`, [`render_holds`]; ruling 23 of the
//! robustness handoff, LC-8 of ). The cold restore relaunches
//! no agent of a held tab, whatever the origin, and puts the owner's
//! (`origin=local`) slot back on the reopened tab ([`SessionFabric::seed_holds`]
//! with [`Holds::of_origin`], `carried=1`), lifted as any local hold is:
//! Fabric > Lift Hold, or the Owner's `hold <sid> off` (the session is the
//! argument; a selector is refused). A session the restore pass spawned is
//! seeded before it is registered; a window's bootstrap shell, registered
//! already, takes the hold's own path ([`carry_hold`]: a committing update's
//! fence respected, the band woken). A FLEET hold is not put back on a fresh
//! shell: it stays the process's, so quitting and relaunching remains the
//! recovery from a stuck one (above). A shell the PTY keeper gave back is no
//! fresh shell: it carries its own holds, both origins, from its keeper META
//! (above), and the layout adds the owner's where that carry has none. Neither
//! sample can be ordered before the other (the journal captures at most every
//! two seconds; the META is the last one delivered), so the owner's stop
//! stands if either says it did — the fail-closed direction the keeper carry
//! itself takes ([`KeeperCarry`]'s "sampled, not journaled") — and the fleet's
//! slot is the META's alone. A person's own quit carries no hold at all, as it
//! carries no agent.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock, Weak};

use aterm_control::wire::pct_encode;
use aterm_session::SessionId;
// The fabric's word grammar, defined once for the bridge, this endpoint and
// the supervisor. The endpoint still CHECKS every word on a `deliver` line —
// the bridge is a separate process and its output is still input — it just no
// longer keeps its own spelling of the rule.
use aterm_types::fabric_grammar::{KINDS, VERDICTS, is_principal, is_topic, via_ok};

use aterm_gui_timing::now_ms;

/// WHAT THE FABRIC NEEDS OF A SESSION: aterm-gui's `SessionCtx`, seen from
/// below the window. Every verb handler takes one.
pub trait FabricSession: Send + Sync {
    /// The session's stable id.
    fn self_id(&self) -> &SessionId;
    /// The session's fabric endpoint.
    fn fabric(&self) -> &SessionFabric;
    /// Record one event on the session's timeline. Called with the fabric
    /// lock held — the sanctioned fabric → timeline nesting — so the
    /// implementation takes the timeline lock and, under it, rings the
    /// session's watchers (a leaf lock whose ring never blocks), and nothing
    /// else.
    fn record(&self, kind: &'static str, payload: String);
    /// [`Self::record`], with what the record says TYPED ([`FabricFact`]) for
    /// the session log's mark beside it, written in the same timeline hold.
    /// When that mark owes the log's readers a wake, the implementation rings
    /// the session's watchers for it in the same hold. `true` says one was
    /// owed: a bridge verb handed a `wake` ([`cmd_deliver_waking`],
    /// [`cmd_hold_waking`]) calls it with the session's id once the fabric
    /// lock is released, and a session verb need not act on it. The default
    /// records and owes none.
    fn record_fact(&self, kind: &'static str, payload: String, fact: FabricFact<'_>) -> bool {
        let _ = fact;
        self.record(kind, payload);
        false
    }
    /// Tell the presence band this session's mail or hold moved. Called with
    /// no fabric lock held.
    fn fabric_changed(&self);
    /// The `holder=` token of `inbox`: who holds this session's keyboard, as
    /// `who`/`lease` report it, `None` when nobody does.
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

/// A session's timeline, as the fabric writes it: one record per event.
/// aterm-gui's `SessionTimeline` is one; the seeds a seamless update runs
/// ([`SessionFabric::seed_carry`], [`SessionFabric::seed_holds`]) take its lock.
pub trait Timeline {
    /// Append one `kind` record carrying `payload`.
    fn record(&mut self, kind: &'static str, payload: String);
    /// [`Self::record`] with its typed [`FabricFact`] (see
    /// [`FabricSession::record_fact`]); the default records and owes none.
    fn record_fact(&mut self, kind: &'static str, payload: String, fact: FabricFact<'_>) -> bool {
        let _ = fact;
        self.record(kind, payload);
        false
    }
}

/// WHAT A FABRIC RECORD SAYS, TYPED, for the session log's mark beside it
/// ([`FabricSession::record_fact`]), so nothing parses a timeline payload
/// back: ids, words and offsets — never a body. Every field of the record's
/// row is the fact's (6F-5 and 6F-6), so the mark beside
/// it reads the row's whole payload: a topic's name and its `since=` token,
/// an advisory's or a hold's reason are the record's too, and the session
/// log holds each at its content tier alone, beside a fingerprint where one
/// names it: never journaled, carried or published. A post's
/// relay hops and its caller's key are host tier: never on the bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FabricFact<'a> {
    /// A row delivered to this session's inbox: its `kind`, its sender's
    /// principal, its bus offset and its inbox id.
    MailIn {
        kind: &'a str,
        from: &'a str,
        off: u64,
        inbox: u64,
    },
    /// `inbox seen <id>`, every one (6F-6): the named row's id and offset,
    /// the addressed lane's refill floor (`None`: `floor=-`), the watermark
    /// after, the verdict it decided with the row's kind and sender (`None`:
    /// no verdict, or the same one again), and whether it MOVED anything
    /// (the watermark, or a verdict decided): the fold folds only moves.
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
    /// A post queued for the bridge: its kind, its recipient (as the caller
    /// wrote it; the `post` record pct-encodes it), its post id, and its
    /// options: the inbox id it answers (`re=`), its deadline (`dl=`), its
    /// relay hops (`via=`) and the caller's own key for it (the `post`
    /// row's `key=`, the mark's `idem=`; 6F-6).
    MailOut {
        kind: &'a str,
        to: &'a str,
        post: u64,
        re: Option<u64>,
        dl: Option<u64>,
        via: Option<&'a str>,
        idem: Option<&'a str>,
    },
    /// An outbound post retired: landed on the bus at `off`, or landed
    /// nowhere (`None`, the row's `off=-`) and why when aterm knows
    /// (`not-carried`, `log-replaced`; 6F-6).
    MailLanded {
        post: u64,
        off: Option<u64>,
        why: Option<&'static str>,
    },
    /// The session's hold moved: whether it is held after, whether the
    /// bridge moved it, and the row's fields (6F-6): the origin of the hold
    /// that stands first (or the one just lifted), the origins standing
    /// (`-` for none), the standing hold's reason (`None`: nothing stands),
    /// and whether the halt was carried in rather than set here.
    Hold {
        held: bool,
        bridge: bool,
        origin: &'a str,
        hold_origin: &'a str,
        reason: Option<&'a str>,
        carried: bool,
    },
    /// A broadcast opt-in moved (`topic add <t> since=<s>`, `topic drop
    /// <t>`; phase 6, 6F-5): whether it was added, the topic, and the
    /// `since=` token an add resumes from.
    Topic {
        add: bool,
        name: &'a str,
        since: Option<&'a str>,
    },
    /// A lost bridge's ADVISORY on a session it governed ([`bridge_lost`]'s
    /// default): the hold the fail-closed sweep would have applied, its
    /// origin and its reason, and the bridge generation lost (6F-5).
    Advisory {
        origin: &'a str,
        reason: &'a str,
        generation: u64,
    },
    /// A parked `inbox get @<off>` asked the bridge for the record at `off`
    /// (the `fetch` row the bridge drains).
    Fetch { off: u64 },
}

/// WHAT THE TWO BRIDGE VERBS NEED OF THE REGISTRY: aterm-gui's `SessionStore`,
/// held as a [`Store`] (`Arc<RwLock<…>>`). Read under the store guard and
/// released before any fabric lock is taken.
pub trait Registry: Send + Sync + 'static {
    /// Every live session, in local-id order (the order `outbox` drains them).
    fn sessions(&self) -> Vec<Arc<dyn FabricSession>>;
    /// The live session with this id, if any.
    fn session(&self, sid: &SessionId) -> Option<Arc<dyn FabricSession>>;
    /// A lost bridge left `sessions` running with an advisory instead of
    /// halting them ([`bridge_lost`]): say so once, in the message band.
    fn bridge_lost_advised(&self, sessions: usize);
    /// The bridge that owns the link moved `files` state files it found torn
    /// after an unclean shutdown into its state dir's quarantine (`link
    /// set-aside`, [`cmd_link`]):
    /// a fact for the owner's later look, never an alert. `told_when_empty`:
    /// that bridge said in its `hello` a lane that says what its quarantine
    /// still holds (`link quarantine`, at
    /// [`aterm_types::control_verbs::LINK_QUARANTINE_LANE`] or later), so the
    /// fact will end once the owner empties it ([`Registry::torn_state_held`]);
    /// an older bridge never says, and the fact stands until dismissed.
    fn torn_state_set_aside(&self, files: u64, told_when_empty: bool);
    /// The bridge that owns the link says its quarantine still holds `files`
    /// of the torn files it set aside (`link quarantine`, [`cmd_link`]): at 0
    /// the owner has taken them away, and the fact filed for their look has
    /// ended; any other count changes nothing.
    fn torn_state_held(&self, files: u64);
}

/// The registry as the fabric holds it: one allocation per instance, whose
/// address is its identity ([`TouchedRegistry`]).
pub type Store = Arc<RwLock<dyn Registry>>;

/// A registry handle the bridge verbs take: aterm-gui's own store, a test's,
/// or a [`Store`] already.
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

/// How many message rows one session's inbox retains. Drop-oldest past this, and
/// an eviction of a row the agent has not HANDLED (`inbox seen`) is REPORTED
/// (`dropped=`), never silent — a message that vanished without a trace is worse
/// than one refused.
///
/// UNHANDLED, not merely unlisted. A row listed once at turn start and never
/// acted on is exactly the message whose silent loss matters most (a human's
/// `task` waiting through a long turn), and counting only unlisted rows made
/// that the one case the header never mentioned.
pub const RING_CAP: usize = 512;

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
pub const SENDER_QUOTA: usize = 64;

/// How much of a body an `inbox` row carries inline before `more=1` sends the
/// reader to `inbox get`. Bytes of the DECODED body, measured before encoding.
const TEXT_PREVIEW_MAX: usize = 512;

/// The largest body `deliver`/`post` accept, and the largest `inbox get` returns.
pub const BODY_MAX: usize = 256 * 1024;

/// The largest INLINE `post` text (the argument form). A longer body arrives as
/// the length-prefixed frame instead.
pub const POST_INLINE_MAX: usize = 4 * 1024;

/// How many outbound posts one session may hold WAITING for the bridge, and how
/// many bytes of retained body they may hold together.
///
/// TWO BOUNDS BECAUSE THERE ARE TWO WAYS TO OVERFLOW: 128 messages of 4 B, and 16
/// messages of 256 KiB, are the same hazard from opposite directions, and a count
/// alone would let one session pin 32 MiB behind an unreachable broker.
///
/// AND THE BYTE BOUND COUNTS EVERY CALLER-SIZED FIELD, not the body alone
/// ([`PostRow::caller_sized_bytes`]).
/// It once folded `p.body.len` alone, which left `via=` — the other
/// caller-chosen string on the same request line, and at the time bounded by
/// nothing at all — outside the number this constant names.
///
/// AND THE OVERFLOW IS REFUSED AT THE DOOR, not evicted. The inbox ring may
/// drop-oldest because every row it drops is still on the log and its loss is
/// reported (`dropped=`); a dropped OUTBOUND message has no record anywhere —
/// the sender was told `OK`, the bus never saw it, and nothing on either side
/// can notice. So `post` answers `ERR outbox full` and the caller still holds
/// its text.
pub const OUTBOX_CAP: usize = 128;
pub const OUTBOX_BYTES_MAX: usize = 4 * 1024 * 1024;

/// How many bytes ONE `outbox` drain may answer with, across ALL sessions.
///
/// The two bounds above are per session, so `sessions × OUTBOX_BYTES_MAX` is what
/// a single reply used to cost — three times over, counting the reply copy and
/// the bridge's own buffer for the announced length. This is the aggregate the
/// caller and the endpoint MEET on, and it is enforced where the reply is built
/// so no caller can disagree with it. Sized at one session's full queue: large
/// enough that the common case (a handful of sessions, small bodies) is still one
/// round trip. A partial drain retires nothing and says `more` ([`cmd_outbox`]),
/// so the bridge's queued `post` events resume it at once, or with none queued
/// its two-second roster backstop.
const OUTBOX_DRAIN_BYTES_MAX: usize = OUTBOX_BYTES_MAX;

/// How many RETIRED posts (landed or undeliverable) a session keeps after the
/// bridge is done with them. They are kept at all because an incoming answer's
/// `re=<offset>` is resolved to `re-id=<post id>` through exactly this table.
///
/// EVERY CALLER-SIZED FIELD IS DROPPED AT RETIREMENT, which is what makes 512 of
/// them a bounded cost: [`retire_post`] clears the body AND the `via` chain, so
/// what survives is the fixed-size half (id, off, re, dl, dead) plus three
/// individually bounded words — `to` (a principal), `kind` (one of [`KINDS`]) and
/// `dead_reason` (24 bytes, [`valid_dead_reason`]). This sentence used to say
/// "only (id, to, kind, off)" while `via` — one unbounded token off the `post`
/// line — was in fact retained on all 512.
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

/// The seven session-timeline record kinds the fabric writes, which are ALSO their
/// wire names on the `events` digest (`EVENT <local> inbox <id> …`). One list, read
/// by `subscribe`'s `timeline_wire_kind`, so the recorder and the digest cannot
/// disagree about which rows leave the process. NONE of them carries a body — that
/// is the discipline the digest exists to keep. `topic` is the endpoint telling
/// its bridge a broadcast opt-in moved (`topic add <t> since=<s>` / `topic drop
/// <t>`), on the lane the bridge already holds, so it acts at once instead of
/// sampling `topic ls` on a timer.
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

/// Return `true` iff adding `add_bytes` to an outbox with `(queued_posts, queued_bytes)` stays within
/// [`OUTBOX_CAP`], [`BODY_MAX`], and [`OUTBOX_BYTES_MAX`].
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

/// The kinds a SENDER may claim on `post`: [`KINDS`] (the set `deliver`
/// accepts, and the `await inbox` vocabulary) minus the two a bridge RECORDS on
/// a sender's behalf.
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
/// `expired` and `undeliverable` are verdicts a bridge records, never something
/// a sender may claim — what [`POST_USAGE`] and the `post` verb row have said all
/// along. `post_may_not_claim_a_verdict_a_bridge_records` pins the difference, so
/// a kind added to `deliver` cannot silently become postable.
const POSTABLE: &[&str] = &["ask", "answer", "task", "report", "note", "ack", "control"];

/// The trust labels the RECEIVING bridge computes (§4.3) — the endpoint stores
/// what it is told on a `BridgeOnly` line and refuses anything else, so an
/// unknown label can never reach an agent's screen as if it meant something.
const TRUSTS: &[&str] = &["human", "agent", "relayed", "screen"];

/// The kinds `await inbox` latches on when the caller lists none: everything but
/// `note`. A note is the class that must never wake anybody by itself.
const AWAIT_DEFAULT_SKIP: &str = "note";

/// How many `inbox get @<off>` reads one session may have PARKED for the
/// bridge at once, and how long one waits. The bridge answers each with one
/// bounded `Fetch` on its next drain (prompted by the `fetch` timeline event,
/// with the two-second roster round as a backstop), so ten seconds is a hang detector, never a
/// synchronisation sleep — and a slot is one offset and one bounded body.
const FETCH_SLOTS: usize = 8;
const FETCH_WAIT_MS: u64 = 10_000;

/// How many receipts one session may OWE at once: `inbox seen <id>
/// handled|refused|deferred` decisions on `ask`/`task` rows that the bridge has
/// not yet put on the bus as `kind=ack` (R8) — see [`OwedReceipt`]. The ring's
/// own bound, one per row it can hold; only deciding the same rows again under
/// a changed word can reach it. Past it the OLDEST owed receipt is dropped: a
/// bounded queue behind a broker that is not coming back, never an unbounded
/// one.
const RECEIPTS_OWED_MAX: usize = RING_CAP;

/// How much longer than `dl=` a bare `post --wait-ack` waits for the ASKER's
/// own bridge to record `expired`. The endpoint's clock starts at the landing
/// report, which follows the publish the bridge's clock starts at, so the
/// endpoint is never EARLY; but the bridge notices on a 250 ms sweep, reads the
/// asker's lane, publishes the verdict, and the verdict is then delivered back
/// here. With no grace the wait ended first every time — `ERR timeout` at
/// 1.5–1.8 s for a 1.5 s `dl=`, the `expired` row landing just after it — while
/// the help promised `ERR expired`. The grace bounds only the case where no
/// verdict ever comes: a verdict (or an `ack`) that arrives ends the wait at
/// once.
const EXPIRY_GRACE_MS: u64 = 5_000;

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// Test-only override of [`EXPIRY_GRACE_MS`] for the CALLING thread's own
    /// `post --wait-ack`, so a test of the `dl=` + grace bound waits a short
    /// grace instead of the real five seconds. Thread-local, so it moves no
    /// other thread's wait; the shipped value is pinned by an `assert_eq!` on
    /// the constant in the test that sets this.
    static TEST_EXPIRY_GRACE_MS: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

/// The grace a bare `post --wait-ack` adds to `dl=`: [`EXPIRY_GRACE_MS`], or a
/// test's thread-local override.
fn expiry_grace_ms() -> u64 {
    #[cfg(any(test, debug_assertions))]
    if let Some(ms) = TEST_EXPIRY_GRACE_MS.with(std::cell::Cell::get) {
        return ms;
    }
    EXPIRY_GRACE_MS
}

/// One delivered message. Every field is a wire token the `inbox` row prints
/// verbatim; free text is pct-encoded ONCE, at delivery, so the listing never
/// re-encodes and the body `inbox get` returns is the decoded original.
#[derive(Clone, Debug)]
pub struct InboxRow {
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
    /// One of [`TRUSTS`]: the receiver's verdict on what the content is.
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
    /// On a `kind=ack` row: the RECIPIENT's verdict on the `ask`/`task` at
    /// `re=` — one of [`VERDICTS`], published by their bridge when they ran
    /// `inbox seen <id> <verdict>` (R8). Printed as `verdict=` on the row.
    pub verdict: Option<String>,
    /// THIS session's verdict on the row, as `inbox seen <id> <verdict>` last
    /// recorded it. Internal: it is what keeps a repeated `inbox seen` under
    /// the same word from recording — and so from acking — twice.
    pub decision: Option<String>,
    /// The kind this message CLAIMED before the receiver demoted it (a relayed
    /// row is always `note`, whatever it says it is).
    pub demoted: Option<String>,
    /// The relay chain, comma-joined, when the message came through one.
    pub via: Option<String>,
    /// Body length in bytes, before truncation.
    pub len: usize,
    /// The body, as delivered (pct-DECODED, so already lossy-valid UTF-8).
    pub body: String,
    /// THE BROADCAST TOPIC this row arrived on, when it is a broadcast.
    ///
    /// It is the row's ANSWER TO "why is this in my mailbox?": an addressed
    /// message is here because somebody chose this session, and a broadcast is
    /// here because this session chose the topic (`topic add`). Nothing else
    /// about the row changes — the ring, the quota, the demotion and `trust=`
    /// are the same — so an agent that ignores the field reads a broadcast as
    /// the `note` it is.
    pub topic: Option<String>,
    /// Whether a non-peek `inbox` has listed this row. Unlisted rows are what the
    /// quota counts and what an eviction reports.
    pub listed: bool,
    /// A BROADCAST the delivering bridge OFFERS AGAIN to a successor that keeps
    /// this sid while it is unhandled — it said `refill=…say` in its `hello`
    /// ([`bridge_refills_broadcasts`]), and keeps the row's refill floor in its
    /// state dir. A seamless update counts an unhandled broadcast in `dropped=`
    /// only when this is false ([`FabricCarry::broadcasts_refilled`]). Always
    /// false on an addressed row, and cleared by a `topic drop` of its topic,
    /// which takes the floor with it ([`cmd_topic`]).
    pub refill: bool,
    /// WHETHER `off` STILL NAMES THIS ROW'S RECORD on the log the bridge reads
    /// NOW: set when it is delivered, and cleared by the first log move that
    /// does not keep the offset ([`Inbox::forget_bus_offsets`]) — another log,
    /// or a moved generation that continues the old one only below it. Never
    /// set again: an offset one move took names another record on every log
    /// after it, whatever a later move keeps.
    on_log: bool,
}

impl InboxRow {
    /// Whether this row's sender is a HUMAN principal. Eviction never drops one of
    /// these ahead of an agent's row: a burst from a peer must not be able to push
    /// out the one message a person sent.
    #[inline]
    fn is_human(&self) -> bool {
        self.from.starts_with("h-")
    }
}

/// The sender CLASS of a delivered row, read from the prefix the subject's
/// `<src>` segment carries: `h-` a human, `s-` another session's agent, `a-` a
/// service. Anything else is `other`, which `from=` can still name — a filter
/// never silently matches nothing it did not ask for.
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
    /// `from=<class|principal>`: a [`sender_class`] or an exact principal.
    /// `None` means anyone.
    pub from: Option<String>,
    /// `topic=<t>`: only BROADCAST rows that arrived on that topic. `None`
    /// means every row, broadcast or addressed — it is not a "no broadcasts"
    /// filter, because a subscriber that named no topic asked for its mailbox,
    /// and a broadcast it opted into is its mail.
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

/// One `MAIL` frame's fields — METADATA ONLY.
///
/// There is no body field and there is no `text=`: the ring's bodies are not
/// read to build this, so the push stream cannot carry one by mistake. Every
/// wake surface follows the same rule — the events digest's `EVENT <sid>
/// inbox` row and the manager lane's `MAIL` line print an id, an offset,
/// `from=` and a kind, never a body — for the same reason: a peer's words must
/// reach an agent only through a drain it ran itself. `from` is the sender's
/// full principal for every row, accepted or not; [`valid_principal`] is all
/// that bounds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailLine {
    pub id: u64,
    pub off: u64,
    pub from: String,
    pub kind: String,
    pub re: Option<u64>,
    /// The broadcast topic, when the row is one. METADATA, like every other
    /// field here: it names which face the row came in on, never what it said.
    pub topic: Option<String>,
}

/// What [`SessionFabric::mail_since`] answers.
pub struct MailSince {
    /// The matching rows newer than the cursor, oldest first.
    pub rows: Vec<MailLine>,
    /// The highest row id ever delivered here, matched or not — the cursor to
    /// resume from, so a filtered-out row is stepped over and never re-read.
    pub high: u64,
    /// How many rows with id above the cursor the ring still HOLDS — counted
    /// WITHOUT the filter, because a GAP is about what was lost, not about
    /// what was wanted.
    ///
    /// `(high - cursor) - present` is what was evicted unseen. The ring's
    /// OLDEST id cannot answer that: eviction is BY CLASS ([`Inbox::evict_for`]
    /// drops an agent's row first, wherever it sits), so one human row pinned
    /// at the front holds `oldest` still while agent rows behind it vanish —
    /// measured, 612 rows into a 512 ring with a human row first lost 100 and
    /// a front-anchored gap reported 0.
    pub present: u64,
}

/// One outbound message this session sent. It stays listed until the bridge
/// reports the offset it landed at, so an agent sees at turn start what is still
/// in flight rather than assuming.
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
    /// Whether `off` still names THIS POST'S record on the log the bridge
    /// reads now, as [`InboxRow::on_log`] says of a row's: set when the bridge
    /// reports the landing (an unlanded post names no record yet) and cleared
    /// by the first log move that does not keep the offset
    /// ([`Inbox::forget_bus_offsets`]). A reply threads to the post by offset
    /// only while it is set ([`deliver_row`]).
    landed_on_log: bool,
    /// The offset this post ANSWERS, carried through to the bridge so the
    /// published record's body can say `re=`.
    pub re: Option<u64>,
    /// Advisory deadline in ms, carried through the same way.
    pub dl: Option<u64>,
    /// The relay chain, when the sender declared one.
    pub via: Option<String>,
    /// The caller's idempotency key (`post key=<token>`), carried to the bridge
    /// on every `outbox` drain — a PEEK, so a relaunched bridge reads the same
    /// key from the same row — and cleared at retirement like the body. The
    /// bridge maps it to a durably reserved producer sequence BEFORE publishing
    /// and reuses that sequence on any later post under the same key, so the
    /// broker's own `(producer_id, producer_seq)` dedup makes a re-post exactly
    /// once and answers the ORIGINAL offset. Grammar-bounded ([`valid_key`]),
    /// so not folded into [`PostRow::caller_sized_bytes`] for the reason `to`
    /// is not.
    pub key: Option<String>,
    /// The bridge retired this post `dup=1`: the broker deduped the publish
    /// because an earlier post under the same `key=` had already reserved the
    /// sequence, so `off` is that earlier record's and nothing was appended.
    /// The parked `post --wait` answers `OK <id> off=<n> dup=1`.
    pub dup: bool,
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
    /// refusing. `len` is reported on the `post` row so the bytes are visible.
    pub body: String,
}

/// One standing halt on one session: one origin's slot of its [`Holds`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hold {
    /// Pct-encoded reason, or `-`.
    pub reason: String,
    /// `person` (the person at the window took the tab over: Take Over,
    /// [`HoldIssuer::Person`]), `fleet` (a human's halt through the bridge, or
    /// a lost bridge) or `local` (the instance Owner's own halt, over the local
    /// control socket). The origin is what [`cmd_hold`] gates an act on: a
    /// `fleet` hold is not the owner's to set or lift, and a `person` hold is
    /// no socket's at all.
    pub origin: String,
}

/// The origin word of the person's own slot ([`Holds`]): Take Over at the
/// window (`docs/DESIGN-owner-surfaces-2026-10-16.md` §4).
pub const PERSON_ORIGIN: &str = "person";

/// The reason the person's hold carries: the tab was taken over at the window.
pub const TAKEN_OVER: &str = "taken-over";

/// The origin word of a remote node's slot ([`Holds`]): another Mac took
/// the tab over the fleet (the step-in,
/// `docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5.1).
pub const REMOTE_ORIGIN: &str = "remote";

/// A REMOTE NODE'S HOLD: the node of another Mac that took the tab over the
/// fleet's drive op ([`HoldIssuer::Remote`]), proved by its node key, and
/// the host this Mac's book names it by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteHold {
    /// Pct-encoded reason, or `-`.
    pub reason: String,
    /// The node that holds it.
    pub node: String,
    /// That node as a person reads it ([`Driver::host`]).
    pub host: String,
}

/// The node of another Mac a drive connection serves for ([`DrivingAs`]):
/// what its acts are credited to and what its own hold exempts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Driver {
    /// Its node id, proved by its node key.
    pub node: String,
    /// It as a person reads it: the host this Mac's book names it by, with
    /// its node's short id (`mac-03.local (node 1a2b3c4d)`) — a host is the
    /// node's own word, so the id tells it from one that named itself the
    /// same — else `node <8 hex>`.
    pub host: String,
}

thread_local! {
    /// The node of another Mac this thread serves a drive connection for
    /// ([`DrivingAs`]), `None` everywhere else.
    static DRIVING: std::cell::RefCell<Option<Driver>> = const { std::cell::RefCell::new(None) };
}

/// Marks the connection this thread serves as a remote node's drive
/// connection until dropped: its `hold` acts on the remote slot
/// ([`HoldIssuer::Remote`]) as that node, and its own hold does not refuse
/// its writes ([`halt_refusal`]). Nests: a drop restores what it found.
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

/// THE STANDING HALTS ON ONE SESSION: one slot per origin, each set and lifted
/// on its own ([`apply_hold`]). The session is held while ANY slot stands.
///
/// TWO SLOTS, AND IT USED TO BE ONE. A fleet halt landing on a session the
/// owner had held locally REPLACED the owner's hold, and when the fleet lifted
/// its halt the session was left UNHELD: the owner's own stop was silently
/// gone, with nothing refused and nothing said. The `[fabric]
/// hold_on_bridge_lost = true` sweep did the same with `reason=fabric-lost`,
/// so a reconnecting bridge freed every session the owner had stopped. Now the
/// fleet's halt and the owner's stand side by side: the bridge sets and lifts
/// the fleet's slot, the owner the local one, and neither act touches the
/// other's.
///
/// AND A THIRD, THE PERSON'S (2026-10-16, Take Over). The `local` slot is the
/// Owner token's, which every in-session client holds, so the held agent can
/// lift it (`aterm ctl @self hold off`). The person at the window needs a halt
/// nothing on the wire can touch: the `person` slot is set and lifted only by
/// [`HoldIssuer::Person`], which only the window's own Take Over and Hand Back
/// construct ([`person_hold`]); a socket naming `origin=person` is `ERR usage`
/// and an Owner or bridge `hold off` lifts its own slot and answers `OK
/// hold=1` while the person's stands.
///
/// AND A FOURTH, A REMOTE NODE'S (2026-10-19, the step-in). Another Mac this
/// one lets drive it takes a tab with `hold <sid> on` over the fleet's drive
/// op: the `remote` slot, set and lifted only by [`HoldIssuer::Remote`] as
/// the node its connection proved. It refuses every write but that node's
/// own, it is never set while the person's hold stands or another node's
/// does, the person's Take Over takes it away and their Hand Back lifts it
/// ([`Self::hand_back_origin`]), and no carry writes it ([`render_holds`]):
/// the driving Mac takes it again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Holds {
    /// The person's own hold (`origin=person`): its reason.
    person: Option<String>,
    /// The fleet's hold (`origin=fleet`): its reason.
    fleet: Option<String>,
    /// The owner's own hold (`origin=local`): its reason.
    local: Option<String>,
    /// A remote node's hold (`origin=remote`).
    remote: Option<RemoteHold>,
}

impl Holds {
    /// The slot an origin names. Anything but `person` or `fleet` is the
    /// local slot, as [`parse_hold`] reads an origin it does not know.
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

    /// Whether any hold stands: the session is halted.
    pub fn is_held(&self) -> bool {
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
    pub fn remote(&self) -> Option<&RemoteHold> {
        self.remote.as_ref()
    }

    /// THE SLOT HAND BACK LIFTS ([`person_hold`]): the person's own, else an
    /// Owner client's (`local`) where no person's stands; `None` where
    /// neither does. The fleet's is never the window's to lift. Lifting a
    /// client's hold confers nothing an Owner client lacks — every one of
    /// them lifts that slot with `hold off` — and it is the only way out at
    /// the window of a hold that halts the instance's app verbs (the owner's
    /// own Fabric ▸ Hold a seamless update carried from an older build among
    /// them).
    ///
    /// And where neither stands, a remote node's: the person takes the tab
    /// from the Mac that drives it.
    pub fn hand_back_origin(&self) -> Option<&'static str> {
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

    /// Whose hold the session shows (`status hold_by=`, the survey's
    /// `hold_by=`, the need classifier's [`aterm_types::needs::HoldWord`]):
    /// the person's first, then a remote node's, then the owner's local
    /// stop, then the bridge's.
    pub fn word(&self) -> Option<aterm_types::needs::HoldWord> {
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

    /// The hold of `origin` alone, the other slots empty: what a cold restore
    /// puts back of a layout's holds (the owner's, ruling 23).
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

    /// Every standing hold, the person's first, then a remote node's, then
    /// the fleet's.
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

    /// The hold that refuses a write from the remote node `driver` (`None`:
    /// any other writer) — [`Self::first`], past that node's own hold: a
    /// node's hold keeps everyone out but the node.
    pub fn first_against(&self, driver: Option<&str>) -> Option<Hold> {
        let own = |hold: &Hold| {
            hold.origin == REMOTE_ORIGIN
                && driver.is_some_and(|node| self.remote.as_ref().is_some_and(|r| r.node == node))
        };
        self.standing().find(|hold| !own(hold))
    }

    /// The hold a one-hold surface names — `ERR halted reason= origin=`, the
    /// `hold` event's `reason=`/`origin=`: the PERSON'S when it stands (the
    /// one act that lifts it is the person's Hand Back), else a REMOTE
    /// NODE'S (the Mac driving the tab, which the person takes it from), else
    /// the FLEET'S,
    /// because that is the one the session's own agent cannot lift (lifting
    /// the owner's leaves the session held), else the owner's.
    pub fn first(&self) -> Option<Hold> {
        self.standing().next()
    }

    /// The standing origins as `hold_origin=` prints them: comma-joined, in
    /// [`Self::standing`]'s order (`person`, `fleet`, `local`, `fleet,local`,
    /// `person,fleet,local`), `-` for none.
    pub fn origins(&self) -> String {
        let words: Vec<String> = self.standing().map(|h| h.origin).collect();
        if words.is_empty() {
            "-".to_string()
        } else {
            words.join(",")
        }
    }

    /// Set `hold`'s slot — a seed's only act: it never lifts. The first hold
    /// of an origin wins, so a carry naming one origin twice keeps a halt and
    /// cannot replace it with another.
    fn seed(&mut self, hold: Hold) {
        let slot = self.slot_mut(&hold.origin);
        if slot.is_none() {
            *slot = Some(hold.reason);
        }
    }

    /// The `hold` timeline event's payload for these holds, after a
    /// transition: `<held> reason=<r> origin=<o> hold_origin=<origins>`, with
    /// `reason=`/`origin=` the hold [`Self::first`] names, so a reader of the
    /// last event alone (`aterm fabric`, the bridge) reads what stands. When
    /// nothing stands it is `0 reason=- origin=<the origin just lifted>`.
    fn event(&self, lifted: &str) -> String {
        let (origin, reason, hold_origin) = self.event_fields(lifted);
        format!(
            "{} reason={} origin={origin} hold_origin={hold_origin}",
            u8::from(reason.is_some()),
            reason.as_deref().unwrap_or("-"),
        )
    }

    /// [`Self::event`]'s fields, for the [`FabricFact::Hold`] beside its
    /// record (6F-6): the origin and the reason of the hold that stands
    /// first, or `lifted` and `None` when nothing stands, and the standing
    /// origins ([`Self::origins`]).
    fn event_fields(&self, lifted: &str) -> (String, Option<String>, String) {
        match self.first() {
            Some(first) => (first.origin, Some(first.reason), self.origins()),
            None => (lifted.to_string(), None, self.origins()),
        }
    }
}

/// The `reason=` of the fleet hold a lost bridge leaves under `[fabric]
/// hold_on_bridge_lost = true` ([`bridge_lost`]): a hold no person set, which
/// a reconnecting bridge lifts and `aterm fabric halt … off` cannot.
pub const FABRIC_LOST: &str = "fabric-lost";

impl PostRow {
    /// The bytes this row keeps alive whose SIZE THE CALLER CHOSE — the quantity
    /// [`OUTBOX_BYTES_MAX`] is a bound on.
    ///
    /// TWO FIELDS, AND IT USED TO BE ONE. `body` was folded and `via` was not,
    /// which was safe only for as long as `via` was small — and nothing made it
    /// small: it is one whitespace-free token on the `post` request line, so a
    /// single post could carry ~64 KiB of it and 128 of them ~8 MiB, twice over a
    /// 4 MiB bound whose own doc says it exists because "there are two ways to
    /// overflow". `via` is bounded at the door now ([`via_ok`]), and it is
    /// counted here as well so the number this fold produces is the number the
    /// constant names.
    ///
    /// `to`, `kind` and `dead_reason` are deliberately NOT counted. Each is
    /// grammar-bounded to tens of bytes ([`valid_principal`], [`KINDS`],
    /// [`valid_dead_reason`]) and folding them in would make the budget
    /// unfillable by the bodies it is denominated in — 16 posts of [`BODY_MAX`]
    /// would no longer fit a bound of exactly `16 * BODY_MAX`, which is a
    /// different, quieter kind of wrong answer. The rule is "every field a caller
    /// can make big", not "every field".
    ///
    /// `to` IS in that second list ONLY BECAUSE [`cmd_post`] BOUNDS IT. It is a
    /// caller-typed token like the other two, and its grammar check used to
    /// begin `to.trim_start_matches('@')` — which strips a RUN of `@`, so
    /// `@@@…@s-b` validated as `s-b` and was stored, and retired, verbatim. See
    /// the check itself; the claim on this line is only sound with it.
    fn caller_sized_bytes(&self) -> usize {
        caller_sized_bytes(&self.body, self.via.as_deref())
    }
}

/// [`PostRow::caller_sized_bytes`] BEFORE THE ROW EXISTS — the same fold, over
/// the same two fields, for the post [`cmd_post`] is deciding whether to admit.
///
/// ONE FUNCTION BECAUSE IT IS ONE QUANTITY. The door and the fold are the two
/// ends of [`OUTBOX_BYTES_MAX`], and they were computing different numbers: the
/// fold counted `body + via` while the door added `body.len` of the RAW frame
/// bytes and nothing else. Both halves of that were wrong in the same direction.
/// `via` went uncounted for exactly the row being admitted, and the raw byte
/// count is not the byte count the row keeps — the body is stored through
/// `String::from_utf8_lossy`, which turns every invalid byte into three, so a
/// 128 KiB frame of non-UTF-8 was admitted against a 384 KiB row. A bound whose
/// two ends are computed by hand in two places has to be re-derived by hand
/// every time a field or a conversion is added, which is how both halves came to
/// be wrong; there is one place now, and the caller hands it the STORED body.
fn caller_sized_bytes(body: &str, via: Option<&str>) -> usize {
    body.len() + via.map_or(0, str::len)
}

/// The mutable half of [`SessionFabric`], behind one leaf mutex.
#[derive(Default)]
#[doc(hidden)]
pub struct Inbox {
    pub rows: VecDeque<InboxRow>,
    pub posts: VecDeque<PostRow>,
    /// The last drain that exposed a post from this session. This schedules
    /// peeks only: it retires nothing and is not carried across a handoff.
    /// Sessions omitted by a byte or row bound keep their older turn and lead
    /// the next drain, even while every earlier head stays refused.
    outbox_turn: u64,
    pub last_msg_id: u64,
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
    /// A BROADCAST REFUSED BY THE QUOTA COUNTS TOO: an addressed refusal rides
    /// back to the sender, but a broadcast's sender did not choose this
    /// recipient and is told nothing, so the recipient is the only party who
    /// can be.
    ///
    /// Monotone: `inbox` reports it, so a drop is always visible to the next
    /// drain even if the agent missed the one it happened on — across a
    /// seamless update too, which carries it and adds the unread broadcasts it
    /// cannot carry ([`FabricCarry::dropped`],
    /// [`FabricCarry::broadcasts_uncarried`]).
    pub dropped: u64,
    /// The highest offset ever delivered here.
    bus_head: u64,
    /// The highest ADDRESSED (topic-less) offset ever delivered here — what
    /// [`addressed_floor`] reports once no unhandled addressed row is left in
    /// the ring. A maximum over deliveries, so an eviction never lowers it.
    addressed_head: Option<u64>,
    /// The LOWEST offset ever delivered (or fetched) here — the header's
    /// `oldest_on_bus=@<off>`. Every one of THIS SESSION's records from it to
    /// `bus_head` that the broker's retention still keeps is on its log
    /// whether or not the ring still holds it, so `inbox get @<off>` can fetch
    /// it, and one retention gave up answers `ERR retained-from=<floor>`
    /// (astream's R4); the offsets in between
    /// are the whole fleet's, and most are other lanes' (`ERR no such
    /// record`). `None` until the first delivery.
    oldest_on_bus: Option<u64>,
    /// Offsets already delivered, newest last, capped at [`DEDUP_CAP`].
    dedup: VecDeque<u64>,
    dedup_set: HashSet<u64>,
    /// The standing halts, one slot per origin ([`Holds`]).
    holds: Holds,
    /// THE HALT A LOST BRIDGE DID NOT MAKE ([`bridge_lost`] under the default
    /// since 2026-09-29): the hold it would have applied, and the bridge
    /// incarnation it was lost under. Read by `status hold_advisory=` only
    /// while that incarnation still owns the link
    /// ([`SessionFabric::hold_advisory`]): a bridge that attaches since
    /// governs the session again, and the advisory is moot. Never a halt —
    /// nothing is refused on it.
    advisory: Option<(Hold, u64)>,
    /// A seamless update is COMMITTING this session: its Commit has read the
    /// hold serial it compares against the park's and is on its way to `_exit`
    /// ([`SessionFabric::fence_hold`]). While it stands, a hold transition
    /// waits ([`apply_hold`]) instead of landing: past that one comparison
    /// nothing reads the serial again, so a halt answered `OK` here would be
    /// lost with this process while the successor runs the session unhalted.
    /// A direct manual reset waits under it the same way
    /// ([`SessionFabric::note_direct_reset`]) before it is answered.
    /// Cleared when the attempt stands down; never cleared on a Commit that
    /// lands, because the process that set it is gone.
    hold_fenced: bool,
    /// Whether the fence ([`Self::hold_fenced`]) holds back the bridge's
    /// RETIREMENTS too — `outbox sent` and `deliver … receipt=` — which it
    /// does only when the successor is known to read the carry
    /// ([`SessionFabric::fence_hold`]'s `successor_reads_carry`). An older
    /// successor ignores [`FabricCarry`] and restarts the post and receipt ids
    /// at zero under the same sid, so a retirement left unanswered keeps the
    /// bridge's durable sequence pin for an id that successor hands out again,
    /// and its post (or receipt) with that id is deduplicated into the old
    /// record. For that pairing the retirement is
    /// answered, as every earlier build answered it.
    retirements_fenced: bool,
    /// The `inbox get @<off>` reads parked for the bridge (`Pending`, listed on
    /// the `outbox` peek as `fetch sid= off=`) and the ones it answered
    /// (`Done`/`Failed`, kept so a second read of an immutable record costs no
    /// second bus read). Bounded at [`FETCH_SLOTS`]: an answered slot is
    /// evicted to make room, a pending one never is.
    fetches: VecDeque<(u64, FetchSlot)>,
    /// The offsets the PARKED WAITS of this session hold ([`OffsetWatch`]):
    /// every log move reaches each one while it sleeps, as it reaches the rows
    /// and posts ([`Inbox::forget_bus_offsets`]). A wait that ended drops its
    /// watch, and the next registration or move prunes the dead entry.
    watches: Vec<Weak<OffsetWatch>>,
    /// The log this session's clock names. Carried through seamless restart;
    /// `None` with live offset references is an unknown historical clock.
    bus_log: Option<String>,
    /// The receipts this session OWES a sender (R8), oldest first, bounded at
    /// [`RECEIPTS_OWED_MAX`]: listed on every `outbox` peek as `receipt sid=
    /// rid= off= verdict= kind= from=` until the bridge retires one with
    /// `deliver <sid> receipt=<rid> off=<n|->`. See [`OwedReceipt`].
    receipts: VecDeque<OwedReceipt>,
    /// The last [`OwedReceipt::rid`] handed out. Monotone per session.
    next_receipt_id: u64,
    /// How many times what a handoff CARRIES of the outbound half has moved —
    /// a post queued or retired, a receipt owed or retired ([`FabricCarry`]).
    /// Read with the hold serial by the Commit
    /// ([`SessionFabric::carry_serial`]).
    outbox_moves: u64,
    /// The inbox row id the last handoff manifest told its successor to count
    /// on from ([`SessionFabric::handoff_carry`]: [`Self::last_msg_id`] plus
    /// [`INBOX_ID_RESERVE`]): rows this process numbers up to it can never
    /// share an id with one the successor numbers. `None` until a park draws
    /// a manifest. See [`SessionFabric::past_handoff_ceiling`].
    msg_id_ceiling: Option<u64>,
    /// The session ended — its program exited or its tab closed
    /// ([`SessionFabric::close`]). Never cleared.
    closed: bool,
}

/// ONE BUS OFFSET A PARKED WAIT HOLDS ACROSS LOG MOVES: the record a `post
/// --wait-ack` waits for a receipt to ([`wait_receipt_on_log`]), and the post
/// an `await inbox re=<off>` waits for a reply to ([`cmd_await_inbox`]). A
/// wait sleeps without the session's lock, and any number of moves can come
/// and go before it wakes, so it cannot tell from what it finds then which
/// ones took its offset; registered with the session ([`Inbox::watch`]), each
/// move reaches it as it reaches the session's rows and posts
/// ([`Inbox::forget_bus_offsets`]) — the same rule, applied once per move,
/// with no window on how many. The wait reads it under the session's lock,
/// which every move holds while it clears one, so `Relaxed` is all the
/// ordering it needs.
#[derive(Debug)]
struct OffsetWatch {
    off: u64,
    /// [`InboxRow::on_log`]'s meaning for `off`, cleared the same way.
    on_log: AtomicBool,
}

impl OffsetWatch {
    /// Whether `off` still names the record the wait was armed on.
    fn on_log(&self) -> bool {
        self.on_log.load(Ordering::Relaxed)
    }
}

/// One receipt a session owes a sender (R8), HELD HERE — in the one process
/// that outlives both a broker outage and a bridge relaunch — until the bridge
/// reports the `ack` published.
///
/// IT USED TO RIDE THE EVENTS DIGEST ALONE: the bridge published the `ack`
/// straight from the `inbox-seen` event, and an event is gone once it is
/// emitted. Three ordinary moments lost it for good — a bridge reconnecting to
/// the broker (its back-off discards every mailbox item but aterm's closure), a
/// publish that failed (only logged), and a bridge relaunch (the new one
/// subscribes with no replay) — and since a verdict is recorded once per row per
/// change, the recipient could not even say it again. The `outbox` peek is how
/// a POST survives exactly those three, so a receipt rides it too: listed on
/// every drain until retired, and published under a producer sequence the
/// bridge pins to `(sid, rid)`, so a retry after a crash is the broker's
/// `deduped` rather than a second `ack` on the sender's lane.
#[derive(Clone, Debug)]
struct OwedReceipt {
    /// Per-session and monotone: the bridge's pin key, and the retirement's
    /// name.
    rid: u64,
    /// The decided row's broker offset — the receipt's `re=`.
    off: u64,
    /// One of [`VERDICTS`].
    verdict: String,
    /// The row's kind as this endpoint classified it: `ask` or `task`.
    kind: String,
    /// The row's `from=` token — what addresses the receipt.
    from: String,
    /// `off` names a record of a log EARLIER than the one the bridge reads
    /// now: the row was delivered before `link log` said the log changed
    /// ([`Inbox::forget_bus_offsets`]) — on a moved generation, at or above
    /// the fork it continues below. Listed `log=earlier`, and the bridge
    /// retires the receipt `unacked … reason=log-replaced` rather than put
    /// `re=<off>` on a log where that number is another record.
    earlier_log: bool,
}

/// Queue one receipt, handing out its `rid` and keeping the bound.
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

/// THE MAILBOX A SEAMLESS UPDATE CARRIES beside the topics and the hold:
/// the three per-session id
/// counters, every post still waiting for a bridge, and every receipt still
/// owed — on the session's handoff record (`SessionRecord::fabric`), put back
/// by [`SessionFabric::seed_carry`] before the adopted session is registered.
///
/// WHY EACH PART:
///
/// * THE POSTS AND THE RECEIPTS were held only here, "in the one process that
///   outlives both a broker outage and a bridge relaunch" ([`OwedReceipt`]) —
///   and a seamless update was the one exit it did not outlive: a post answered
///   `OK <id>` or `queued=1` while the broker was down, and a verdict's receipt,
///   were gone with the old process's `_exit`, and nobody was told. The sender
///   had been told never to post again.
/// * THE COUNTERS: the sid survives the update, so an id minted from zero again
///   named something else. A pre-update `await inbox since=<id>` waited for a
///   count the successor had not reached, `inbox seen <id>` could decide (and
///   receipt) another row, and — worse — the bridge's durable `(sid, post-id)`
///   and `(sid, rid)` sequence pins, which outlive both processes, published a
///   NEW post at an OLD post's sequence, which the broker deduplicated into the
///   old record and answered as landed.
///
/// WHAT KEEPS IT EXACT: every change to the posts or the receipts moves
/// [`Inbox::outbox_moves`], which the Commit compares with the park's reading
/// ([`SessionFabric::carry_serial`]) exactly as it compares the holds — so a
/// post queued, published or retired after the manifest was drawn refuses the
/// Commit (a lossless retry) rather than being carried stale (a retired post
/// carried as queued would go on the bus twice) — and from the Commit's
/// reading to the `_exit` each such change waits on the same fence a hold
/// waits on ([`SessionFabric::wait_unfenced`]). A retirement that waits there
/// is never answered, so the bridge keeps the sequence it pinned and the
/// successor's bridge publishes the carried copy at that sequence: the
/// broker's dedup answers the record it already holds.
///
/// BOUNDED: the rows ride the manifest, whose cap every successor enforces
/// whole, so a handoff carries at most [`CARRY_MAIL_BYTES`] of them — posts
/// AND receipts, each charged what TOML WRITES for it ([`toml_cost`]: an ESC
/// in a coloured body costs six bytes, not one) — across every session; a
/// post past that is named in `uncarried` and the successor retires it DEAD,
/// `reason=not-carried`, on its timeline — said, never silent — and a receipt
/// past it is counted in `receipts_uncarried`, which the successor logs. A row
/// that cannot be written as TOML (a number past `i64::MAX`) goes the same
/// way. And when the manifest is still over its cap once the held panes are
/// gone, the writer sheds the carry the same way ([`Self::shed`]) before it
/// would refuse the update: the mailbox never costs a desk its update.
///
/// `not-carried` is exact about what the SUCCESSOR holds, not about the bus:
/// a post shed here that the old process's bridge published in the last
/// instant before the `_exit` (its retirement then waits on the fence and is
/// never answered) is on the bus all the same. A sender that must never send
/// a message twice posts with `key=`, which the broker deduplicates. Additive and lenient on the way in: an older reader
/// skips the key, and a row that does not validate is dropped (and, for a
/// post, retired dead) — never the manifest.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FabricCarry {
    /// The log the carried receipts and reply references name. Missing in an
    /// older carry: its live offset references remain unverified until attach.
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
    /// Receipts whose offsets name an earlier log. Kept apart so an older
    /// successor that ignores this key cannot publish them as current receipts.
    /// They share the normal receipts' count and byte bounds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub earlier_receipts: Vec<ReceiptCarry>,
    /// Waiting posts this handoff could not carry, by id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncarried: Vec<u64>,
    /// How many owed receipts this handoff could not carry (their asks'
    /// senders wait out their own deadlines, as for any receipt that never
    /// comes); the successor logs the count.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub receipts_uncarried: u64,
    /// The session's `dropped=` at the draw ([`Inbox::dropped`]). The counter
    /// is monotone so that a drop is visible to the next drain even when the
    /// agent missed the one it happened on; a successor that started it at 0
    /// erased every drop counted before the update.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dropped: u64,
    /// The BROADCASTS this session held unhandled at the draw (`topic=`, id
    /// past `seen`). The ring is not carried, so none of them is in the
    /// successor's ring; [`Self::broadcasts_refilled`] of them come back by
    /// the bridge's refill, and the successor adds the rest to `dropped=` and
    /// logs the count. Every unhandled one, whoever refills it: a successor
    /// from before the refill reads this key alone, and its bridge refills no
    /// broadcast, so it counts each of them lost — as it is.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub broadcasts_uncarried: u64,
    /// Of [`Self::broadcasts_uncarried`], the ones the successor's bridge
    /// OFFERS AGAIN, which are therefore not lost: delivered by a bridge that
    /// refills broadcasts ([`InboxRow::refill`]), on a topic this session
    /// still takes (a successor refuses another `ERR not-subscribed`) and has
    /// not dropped since (the drop took the floor), from the log the bridge
    /// reads now, and young enough that the bridge's bound on what it
    /// remembers cannot have dropped it ([`SessionFabric::handoff_carry`]).
    /// That bridge kept each one's topic floor in its state dir, and the
    /// successor's bridge rewinds the topic's cursor to it
    /// ([`INBOX_ID_RESERVE`]). Never more than `broadcasts_uncarried`; an
    /// older successor reads past it and counts them all.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub broadcasts_refilled: u64,
}

/// `skip_serializing_if` for a zero count.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// One waiting post on the handoff record — [`PostRow`]'s queued fields.
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

/// One owed receipt on the handoff record — an [`OwedReceipt`].
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
/// — one handoff manifest carries across every session ([`FabricCarry`]). The
/// manifest's whole cap is 4 MiB and the screens' metas may take three of it;
/// this keeps the mailbox to a small part of the rest. A post past it is
/// retired dead in the successor, `reason=not-carried`; a receipt past it is
/// counted in [`FabricCarry::receipts_uncarried`].
pub const CARRY_MAIL_BYTES: usize = 256 * 1024;

/// What one carried row costs the manifest beyond its escaped strings: its
/// table header, its keys, its integers and their punctuation — an upper
/// bound for [`PostCarry`] (eight keys) and [`ReceiptCarry`] (six).
pub const CARRY_ROW_OVERHEAD: usize = 256;

/// What `s` costs the manifest as TOML writes it: one byte per byte, plus one
/// per `"` or `\`, plus five per control byte (`\uXXXX`) — an upper bound
/// whichever string form the writer picks, the same rule the screens' metas
/// are charged by (`seamless::meta_json_manifest_cost`).
pub fn toml_cost(s: &str) -> usize {
    s.bytes()
        .map(|b| match b {
            b'"' | b'\\' => 2,
            0..=0x1f | 0x7f => 6,
            _ => 1,
        })
        .fold(0, usize::saturating_add)
}

/// The reason a post the handoff could not carry is retired with in the
/// successor ([`FabricCarry::uncarried`]).
pub const NOT_CARRIED: &str = "not-carried";

/// The reason a queued reply is retired with when the bus log it answers was
/// replaced by one that does not hold the record its `re=` names
/// (`Inbox::forget_bus_offsets`).
pub const LOG_REPLACED: &str = "log-replaced";

/// The largest number a TOML integer holds (an `i64`): a carried value past it
/// would fail the whole manifest's serialization, so its row is not carried.
const CARRY_INT_MAX: u64 = i64::MAX as u64;

/// How many inbox row ids a seamless update leaves this process between the
/// park's draw and its `_exit` ([`SessionFabric::handoff_carry`]). The bridge
/// keeps delivering in that overlap — a peer's post, a topic broadcast, a
/// refill — and each row takes an id; the successor counts on from above the
/// reserve, so no id is ever handed to two rows.
///
/// WHAT REACHES THE SUCCESSOR AGAIN IS EVERY ROW ITS AGENT HAS NOT HANDLED.
/// The ring is not carried: the successor's bridge refills each session's
/// ADDRESSED mail from the floor `inbox seen` last reported
/// ([`addressed_floor`]: the lowest unhandled addressed offset), so an
/// addressed row taken in the overlap is listed there again under an id of
/// its own, and an `inbox seen <old id>` there names no row rather than a
/// wrong one. A BROADCAST comes back the same way from a bridge that refills
/// them (`hello … refill=in,say`, [`InboxRow::refill`]; open problem 201):
/// the bridge moves the topic cursor past a
/// broadcast on the `OK`, but first writes down the topic's REFILL FLOOR —
/// the lowest offset the ring took and `inbox seen` has not passed — and the
/// successor's bridge rewinds the cursor to it and offers the stretch again,
/// whether the row came before the draw or in the overlap. The floor is one
/// offset per topic, so the walk also offers a broadcast the quota refused
/// between two unhandled ones, which the successor then takes after the old
/// process counted it: `dropped=` can over-report across an update, never
/// under. One a bridge that does not refill answered is lost, and one unread
/// at the draw is SAID: the successor counts it in `dropped=` and logs the
/// count ([`FabricCarry::broadcasts_uncarried`] less
/// [`FabricCarry::broadcasts_refilled`]); one such a bridge answered in the
/// overlap, after the manifest was written, is not in that count. Under the
/// Commit's fence a broadcast is therefore never answered: it waits for the
/// attempt ([`SessionFabric::wait_fenced_delivery`]), the `_exit` drops the
/// bridge's lane with the cursor unmoved, and the successor's bridge offers it
/// again, whatever the bridge. Between the draw and the fence it is answered
/// as usual. Standing the Commit down for it would not keep it from a bridge
/// that does not refill — the next update that lands drops it the same way if
/// it is still unread — and would spend the automatic lane's activity retries
/// on mail from outside the desk.
///
/// Inbound mail is NOT activity that vetoes an update: only a process that
/// numbers more than this many rows in one overlap stands its Commit down (a
/// lossless retry), and a delivery that would do so under the Commit's fence
/// waits for the attempt to land or stand down ([`deliver_row`]) — never
/// answered an `ERR` its bridge would book as a verdict.
pub const INBOX_ID_RESERVE: u64 = 1 << 12;

/// How often a delivery waiting on a committing update — one past
/// [`INBOX_ID_RESERVE`], or a broadcast — logs that it is still waiting
/// ([`SessionFabric::wait_fenced_delivery`]): a cadence, never a bound. Its
/// production length; the wait reads it through the process's timing value
/// (`aterm_gui_timing`), so a test, on the test values, drives several rounds
/// of the wait's timeout arm and sees that none of them answers.
pub const MSG_ROOM_NOTICE_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

/// The production bound every fenced verb waits for a committing update
/// before it answers [`HOLD_BUSY`] ([`SessionFabric::fence_wait`], which reads
/// it through the process's timing value).
pub const HOLD_FENCE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

impl FabricCarry {
    /// Nothing to write: every counter at zero and no row.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// What this carry's rows cost the manifest ([`CARRY_MAIL_BYTES`]'s
    /// measure): the largest carry is the one [`Self::shed`] gives up first.
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

    /// GIVE UP THE ROWS, KEEP THE COUNTERS — what the manifest writer does
    /// when the manifest is over its cap with nothing else optional left:
    /// every carried post is named in
    /// `uncarried` instead (the successor retires it dead, `not-carried`),
    /// every receipt is counted in `receipts_uncarried`. The id counters stay,
    /// and so do the loss counts (`dropped`, `broadcasts_uncarried`,
    /// `broadcasts_refilled`): they are a few bytes, and they are what keeps
    /// the successor's ids above the old process's and its `dropped=` honest. `false` when there was nothing to
    /// shed.
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

    /// The counters a PTY keeper recovery starts from: the lease its META
    /// carried ([`KeeperCarry::ids`]), each held where a later update's
    /// manifest can carry it on, and no rows.
    #[cfg(unix)]
    pub fn leased([msg, post, rid]: [u64; 3]) -> Self {
        Self {
            last_msg_id: msg.min(CARRY_INT_MAX),
            last_post_id: post.min(CARRY_INT_MAX),
            last_receipt_id: rid.min(CARRY_INT_MAX),
            ..Self::default()
        }
    }

    /// THE FENCE FOR A KEPT SID WITH NO LEASE: the counters a session the PTY
    /// keeper recovered starts from when its META carried no id lease
    /// ([`KeeperCarry::ids`]) — one an older window sent, or none because the
    /// crash came before the first. The sid survives the recovery and so do the
    /// bridge's sequence pins keyed by it, so counting from zero again could
    /// meet one ([`KeeperCarry`]); a fresh random base in `[2^32, 2^52]`,
    /// above anything a window counts to from zero or a lease, keeps the ids
    /// apart from every earlier window's, a second fence's included, but for
    /// a one-in-a-million draw. Far under [`CARRY_INT_MAX`], so a later
    /// update carries it on.
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

/// THE FABRIC A PTY KEEPER RECOVERY CARRIES (the fabric audit's fourth round):
/// what a session's keeper META says of its fabric beside the
/// screen's scalars ([`SessionFabric::keeper_carry`],
/// `seamless::keeper_meta_of`), read back by `seamless::recovered_fabric`
/// into the [`FabricCarry`], topics and hold `spawn::seed_adopted_fabric`
/// puts back. A session the keeper kept through a crash or a Force Quit keeps
/// its sid, and its agent keeps running; it used to come back as a fresh
/// mailbox under that sid.
///
/// * THE HOLDS ([`render_holds`]): a keeper recovery is not a lift. The
///   owner's stop signal and a fleet halt stand again, each in its own slot,
///   before the recovered session answers a verb, as an update carries them;
///   a carried fleet hold is lifted only by a bridge whose broker shows no
///   standing halt (`converge_hold`), so with the broker or the bridge away
///   it stands, and the owner's stays through that lift.
/// * THE ID LEASE ([`id_lease`]): each counter rounded up past the next
///   [`INBOX_ID_RESERVE`] block. The bridge's durable `(sid, post id)` and
///   `(sid, rid)` sequence pins outlive both processes: counting from zero
///   again published a new post at an old one's sequence, which the broker
///   deduplicated into the old record and answered as landed, and an `inbox
///   seen <id>` the agent learned before the crash decided another row.
///   Numbered from the lease, every id the recovered session hands out is
///   above every one the dead process did, and an old id names no row. A
///   lease moves once a block, so mail does not churn the META.
/// * THE TOPIC ROWS ([`render_topics`]): broadcast consent. Lost, the new
///   bridge's first `topic ls` read `OK 0` as the session's own answer and
///   deleted the durable cursors each topic resumes from.
/// * THE QUEUED POST IDS AND THE OWED RECEIPTS' COUNT. The posts and receipts
///   stay with the dead process — a 4 KiB META cannot hold them, and a
///   sample cannot say which the bridge published since — but the recovered
///   session retires each queued id dead, `reason=not-carried`, with its
///   timeline row, and logs the receipts, as for an update that could not
///   carry them ([`SessionFabric::seed_carry`]). Said, never silent.
/// * THE LOSS COUNT, `dropped=` ([`Self::dropped`]): monotone across an update
///   ([`FabricCarry::dropped`]), and so across a recovery too, where it read 0
///   again and an agent that watches it for gaps saw none. A META from before
///   it carries none and reads 0, as before.
///
/// SAMPLED, NOT JOURNALED: the pump attempts a changed META every
/// `keeper_link::META_POLL`; lock contention, a full queue or a failed link can
/// leave the keeper with an older sample. A hold lifted after that sample
/// comes back held (the fail-closed direction: `hold off` lifts it again);
/// a hold set, a topic added or a post queued after it can still be lost, as
/// can ids minted beyond its stored lease before a newer META arrives.
///
/// BOUNDED: the holds, the lease and the two counts always fit beside the
/// most a META sheds to; the lists give way to their counts when nothing
/// else will fit ([`Self::rungs`]), and the recovery logs what they named.
#[cfg(unix)]
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct KeeperCarry {
    /// The first standing hold, as [`render_hold`] writes it — the person's
    /// when it stands, else the fleet's ([`render_holds`]) — under the key
    /// every META reader since the keeper carry reads, so a build from before
    /// the slots still recovers a halt (it reads `origin=person` as a hold, as
    /// [`parse_hold`] reads every row). A crash is not a Hand Back: the
    /// recovered session comes back with the person's Take Over standing,
    /// the fail-closed answer for a hold nobody lifted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold: Option<String>,
    /// Every other standing hold, one row each (the fleet's and the owner's
    /// beside the person's, the owner's beside the fleet's). An older reader
    /// reads past the key.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub more_holds: Vec<String>,
    /// The id lease: inbox row, post, receipt.
    pub ids: [u64; 3],
    /// The broadcast opt-ins, as [`render_topics`] writes them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<String>,
    /// Opt-ins the META had no room to name ([`Self::rungs`]).
    #[serde(skip_serializing_if = "is_zero")]
    pub topics_lost: u64,
    /// The posts still waiting for a bridge, by id, oldest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub queued: Vec<u64>,
    /// Waiting posts the META had no room to name.
    #[serde(skip_serializing_if = "is_zero")]
    pub queued_lost: u64,
    /// The receipts still owed.
    #[serde(skip_serializing_if = "is_zero")]
    pub owed: u64,
    /// The session's `dropped=` at the sample ([`Inbox::dropped`]), which the
    /// recovered session goes on counting from. At most `i64::MAX`, as the
    /// update's carry holds it.
    #[serde(skip_serializing_if = "is_zero")]
    pub dropped: u64,
}

#[cfg(unix)]
impl KeeperCarry {
    /// The carry as the META's cap tries it, most first: whole; the queued
    /// post ids given up for their count; and the topic rows too. The holds,
    /// the lease, the receipt count and the drop count are in every rung.
    ///
    /// Past those, the holds' REASONS give way, never the holds: first every
    /// reason but the first hold's, then that one too, each read back as
    /// [`REASON_NOT_CARRIED`]. Two holds whose reasons are 128 bytes that JSON
    /// doubles do not fit beside the widest scalars a session can reach; the
    /// halts they are, origin by origin, always do.
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

/// A counter's LEASE ([`KeeperCarry::ids`]): `n` rounded up past the next
/// [`INBOX_ID_RESERVE`] block, so it is at least a whole block above every id
/// handed out when it was taken and changes only when a block is crossed.
#[cfg(unix)]
fn id_lease(n: u64) -> u64 {
    (n / INBOX_ID_RESERVE)
        .saturating_add(2)
        .saturating_mul(INBOX_ID_RESERVE)
        .min(CARRY_INT_MAX)
}

impl PostCarry {
    /// The bytes this post costs the carry's budget: what the manifest's TOML
    /// spends on it ([`toml_cost`]), never its raw length — a body of ANSI
    /// colour codes costs up to six times its length once escaped.
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

    /// Whether the successor accepts this row: the grammar `post` held it to,
    /// re-checked, because the manifest is another build's word.
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
    /// The bytes this receipt costs the carry's budget (see
    /// [`PostCarry::cost`]).
    pub fn cost(&self) -> usize {
        [&self.verdict, &self.kind, &self.from]
            .into_iter()
            .map(|s| toml_cost(s))
            .fold(CARRY_ROW_OVERHEAD, usize::saturating_add)
    }

    /// Whether the successor accepts this row (see [`PostCarry::valid`]).
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

/// One `inbox get @<off>` read, from parked to answered.
#[derive(Clone, Debug)]
enum FetchSlot {
    Pending,
    /// The bridge is answering in chunks (`more=1`) and this much has come:
    /// still waiting, and still listed on the `outbox` peek — a bridge that
    /// dies mid-answer leaves it here, and its replacement starts again at
    /// `at=0`.
    Partial(Fetched),
    Done(Fetched),
    /// The bridge's one-token reason: `no-record` (not on this session's lane,
    /// or nothing there), `unreadable`, `observer`, `forged-self`, `via`,
    /// `oversize`, or `refused` (the bridge's follow-up to an answer this
    /// endpoint refused) — and `malformed`, which [`refuse_fetched`] records
    /// here itself for the read an unacceptable answer was meant for.
    Failed(String),
}

/// One record as the bridge fetched it from the log — the fields a `msg` row
/// carries, without a row id, because it was never (re)appended to the ring.
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
    /// The same answer, for a record the ring still holds.
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

    /// `OK <nbytes> off=<n> from=<p> kind=<k> trust=<t> … [truncated=1 len=<n>]`
    /// then the body: only the first token after `OK` is the frame length, so
    /// the record's own fields ride the tail exactly as `truncated=1` does.
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
    /// FORGET THE BUS OFFSETS THIS SESSION HOLDS THAT THE LOG NO LONGER
    /// NAMES: the bridge says the log they index is not the one it reads now
    /// ([`cmd_link_log`]), and astream gives another log's records the same
    /// numbers. `keep_below` is `None` for ANOTHER LOG, and every offset goes;
    /// `Some(f)` for a MOVED GENERATION of the same log that continues the old
    /// one below `f` (`link log … continues=<f>`), and only what names an
    /// offset at or above `f` goes — below it both generations hold the same
    /// records, and forgetting them retired a session's reply, its verdict's
    /// receipt and its reads over a move that never touched them.
    ///
    /// ONE MOVE AT A TIME, AND NO WINDOW. The bridge tells the endpoint every
    /// move it attaches across, each as a fact about two identities: `f` is
    /// already the lowest common over astream's lineage steps between them,
    /// capped at the head — the very point the bridge's own clamp takes
    /// (`aterm_link::state::LogWitness::continues`, which `FabricReaderResume`
    /// binds), or no fork at all where astream's window has no step to go by.
    /// So the endpoint keeps no lineage of its own. It applies each move once,
    /// to every reference it holds, as the bridge clamps its own state at each
    /// attach: an offset survives a chain of moves exactly when it is below
    /// every fork in it, however long the chain, and one a move took stays
    /// taken ([`InboxRow::on_log`]).
    ///
    /// WHAT GOES, offset by offset. The `off=` dedup entry (a new record at
    /// that offset would be answered `OK <old id>` and lost); the
    /// `bus_head=`/`oldest_on_bus=` span and the addressed refill floor
    /// ([`addressed_floor`]), which start again from what is kept (the floor at
    /// `f`, where the bridge's own `seen/` watermark resumes); every read
    /// answered from the old log, and every one still parked fails
    /// `reason=log-replaced` (the bridge would answer it from the new log);
    /// and a queued reply whose `re=` names one is retired dead. A row, a
    /// landed post or a parked wait ([`OffsetWatch`]) at such an offset is
    /// marked an earlier log's: no `re=` threads to it, no `inbox get @<off>`
    /// answers it, and the wait ends `log-replaced`. The rows themselves stay:
    /// they were delivered, and what they say is still what was sent. So do
    /// the receipts owed for them, each at such an offset marked as an earlier
    /// log's ([`OwedReceipt::earlier_log`]) — as is one a later verdict owes —
    /// so a bridge that understands the mark retires it rather than publish
    /// its `re=` here. Older bridges are not offered those receipts.
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
        // The addressed lane is delivered in offset order, so a session that
        // was handed one at or above the fork was handed every one below it:
        // its refill resumes at the fork, as the bridge's `seen/` does.
        self.addressed_head = self.addressed_head.and_then(|h| {
            if kept(h) {
                Some(h)
            } else {
                fork.checked_sub(1)
            }
        });
        if !kept(self.bus_head) {
            // The highest offset still known delivered here: never one the
            // session was not handed.
            let rows = self.rows.iter().filter(|r| r.on_log).map(|r| r.off);
            self.bus_head = rows
                .chain(self.dedup.iter().copied())
                .chain(self.oldest_on_bus)
                .max()
                .unwrap_or(0);
        }
        // A queued new message can land on the new log. A queued reply whose
        // `re` names a record the new log does not hold cannot. Report that
        // distinction as an explicit retirement instead of silently rebinding
        // the reference.
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

    /// ARM A WATCH ON `off` for a wait about to park ([`OffsetWatch`]):
    /// `on_log` is what the session knows of the offset now — true for one
    /// the bridge just named on the log it reads, a landed post's
    /// [`PostRow::landed_on_log`] for a post's — and every move from here on
    /// reaches it. Prunes the watches whose waits have ended first, so the
    /// list never holds more than the waits parked at the newest
    /// registration, this one included.
    fn watch(&mut self, off: u64, on_log: bool) -> Arc<OffsetWatch> {
        self.watches.retain(|w| w.strong_count() > 0);
        let watch = Arc::new(OffsetWatch {
            off,
            on_log: AtomicBool::new(on_log),
        });
        self.watches.push(Arc::downgrade(&watch));
        watch
    }

    /// Whether this session holds any live reference into SOME bus log: a
    /// delivery span, an owed receipt, a read, a landed post or a queued
    /// reply. A session with none and no [`Self::bus_log`] has nothing for a
    /// first identity to forget; one with some and no clock (a carry from a
    /// build before the clock was carried) has an UNKNOWN old clock, which no
    /// identity can claim.
    fn holds_bus_references(&self) -> bool {
        self.oldest_on_bus.is_some()
            || !self.receipts.is_empty()
            || !self.fetches.is_empty()
            || self
                .posts
                .iter()
                .any(|p| p.off.is_some() || (!p.dead && p.re.is_some()))
    }

    /// The ring row delivered at `off` on the log the bridge reads NOW — never
    /// one an earlier log's offset names ([`InboxRow::on_log`]).
    fn row_at(&self, off: u64) -> Option<&InboxRow> {
        self.rows.iter().find(|r| r.off == off && r.on_log)
    }

    /// The outbound load `post` is refused against: how many posts still wait for
    /// the bridge, and how many CALLER-SIZED bytes they hold between them
    /// ([`PostRow::caller_sized_bytes`], which is body + `via`).
    ///
    /// THE BODY WAS ONCE THE ONLY TERM, and it was not the only caller-sized one.
    /// `via` is a single whitespace-free token on the same request line and was
    /// bounded by nothing at all, so 128 queued posts could pin ~8 MiB above a
    /// stated 4 MiB bound whose own doc says "TWO BOUNDS BECAUSE THERE ARE TWO
    /// WAYS TO OVERFLOW". It is bounded at the door now ([`via_ok`]) AND
    /// counted here, because a bound that folds one of a row's two caller-sized
    /// fields has to be re-derived by hand every time a field is added — which is
    /// how this one came to be wrong.
    pub fn queued_load(&self) -> (usize, usize) {
        self.posts
            .iter()
            .filter(|p| p.off.is_none() && !p.dead)
            .fold((0, 0), |(n, b), p| (n + 1, b + p.caller_sized_bytes()))
    }

    /// Drop the oldest RETIRED posts past [`RETIRED_KEEP`]. A post still waiting
    /// for the bridge is never dropped here — that is what the refusal at the door
    /// is for — so this can only ever discard a row [`retire_post`] has already
    /// stripped of its body and its `via` chain, and whose only remaining use is
    /// resolving an old answer's `re-id=`.
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

/// ONE BROADCAST OPT-IN as the session holds it: what the caller asked to
/// resume from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopicEntry {
    /// The `since=` TOKEN the add carried: `head` or `@<off>`. An intent, not
    /// a position — only the bridge knows where the bus is.
    pub since: String,
}

/// The per-session fabric state, hung on aterm-gui's `SessionCtx` ([`FabricSession`]).
///
/// LOCK DISCIPLINE. `inbox` is a LEAF, with exactly one sanctioned nesting:
/// fabric → `ctx.timeline`, never reversed, so two concurrent `deliver`s cannot
/// append to the ring in one order and record their `EVENT`s in the other. The
/// condvar is signalled under the same guard, which is what makes `await inbox`
/// and `post --wait` event-driven instead of polled — no sleep anywhere in this
/// module. `topics` nests the same way ([`SessionFabric::topic_add`]), and under
/// `inbox` when it is taken with it: `topic add|drop` hold the `inbox` guard
/// across the change and `deliver` reads the set under it
/// ([`SessionFabric::opted_into`]), so the order is inbox → topics → timeline.
#[derive(Default)]
pub struct SessionFabric {
    inbox: Mutex<Inbox>,
    /// Signalled on every state change a parked waiter could care about: a new
    /// row, a post landing, a hold transition.
    changed: Condvar,
    /// THE BROADCAST TOPICS THIS SESSION OPTED INTO (`topic add`).
    ///
    /// Receiver-side opt-in is the whole safety story of broadcast: a record on
    /// `say/<topic>` reaches a session only because that session asked for the
    /// topic, so a fleet-wide shout cannot put anything in front of an agent
    /// that did not want it. Empty by default, and an empty set receives
    /// NOTHING — enforced HERE: `deliver` refuses a `topic=` row whose topic
    /// this set does not hold (`ERR not-subscribed`), so the bridge's copy is a
    /// routing hint, and a `drop` takes effect at this endpoint the moment it
    /// answers.
    ///
    /// Its own lock, not the ring's: the bridge reads it (`topic ls`) once per
    /// session and after a `GAP`, and the ring is a leaf taken under `deliver`,
    /// so sharing one would put that read in the path of every delivery. The
    /// `topic` timeline record is written under this lock, so two connections
    /// cannot change the set in one order and record it in the other.
    /// Keyed by topic, valued by the `since=` TOKEN the add asked for (`head`
    /// or `@<off>`) — the bridge resolves the token to an offset once, when it
    /// first learns the entry, and remembers that resolution in its own state,
    /// so a bridge restart resumes the topic instead of re-reading `head`.
    /// Every change is pushed to the bridge as a `topic` event, so on a session
    /// the bridge's push lane watches, a `drop` then an `add` are two events in
    /// order and the second's `since=` is read. A session past the watch cap is
    /// polled instead, and read as its final set.
    topics: Mutex<std::collections::BTreeMap<String, TopicEntry>>,
    /// How many times this session's hold has MOVED — set, replaced, lifted or
    /// seeded by a handoff — bumped under the `inbox` guard that moves it.
    ///
    /// The seamless update's Commit reads it (`App::hold_serials`): the park
    /// draws the handoff manifest, holds included, and then the successor
    /// boots while this process's control thread is still serving. A `hold
    /// on` answered in that window would land here and never reach the
    /// successor — an update lifting a halt it had just acknowledged. A serial
    /// that moved since the park refuses the Commit instead (the attempt is
    /// retried, lossless). A counter rather than a comparison of holds, so a
    /// hold set and lifted inside the window still counts as the change it
    /// was.
    ///
    /// A manual reset run directly on the engine moves it too
    /// ([`Self::note_direct_reset`]): on a parked session it lands after the
    /// carry was taken, the same change-the-successor-would-not-see a hold is.
    hold_serial: std::sync::atomic::AtomicU64,
    /// How many acts [`halt_refusal`] refused on this session while the
    /// PERSON'S hold stood, since it was set (Take Over zeroes it): what the
    /// menu bar's You hold row and the hover say the hold kept out ("refused
    /// 3 writes"). Moved only by a refusal, never by a read.
    person_refusals: AtomicU64,
    /// A test's own bound for every fenced wait ([`Self::fence_wait`]).
    #[cfg(any(test, debug_assertions))]
    fence_wait: Mutex<Option<std::time::Duration>>,
    /// How many times a waiter on this session has blocked on a committing
    /// update's fence ([`note_fence_block`]): a test awaits it to know a
    /// waiter is parked, instead of sleeping and hoping it got there.
    #[cfg(any(test, debug_assertions))]
    fence_waits: AtomicU64,
    /// The bound the latest waiter that blocked on this session's fence waits
    /// ([`note_fence_block`]): a test reads which bound a fenced verb took
    /// without sitting it out.
    #[cfg(any(test, debug_assertions))]
    fence_bound: Mutex<Option<Option<std::time::Duration>>>,
}

impl SessionFabric {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inbox> {
        self.inbox.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Run `f` on this session's ring under its lock: aterm-gui's handoff
    /// tests set and read the counters a seamless update carries. Debug
    /// builds only; nothing shipped reaches an [`Inbox`] from outside.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn with_inbox_for_test<T>(&self, f: impl FnOnce(&mut Inbox) -> T) -> T {
        f(&mut self.lock())
    }

    /// The bound every fenced verb on this session waits for a committing
    /// update before it answers [`HOLD_BUSY`], or a test's own
    /// ([`Self::set_fence_wait`]), so a test of what outlasts the bound need
    /// not sit out the real one. The only door to it: a path that capped a
    /// wait with the real bound directly would escape every test's own.
    ///
    /// The fenced stretch is the successor's activation, the Commit's
    /// answers, the identity markers, the harness suspend and one pipe write —
    /// milliseconds — and, when those answers took real time, one more
    /// input-dispatch fence the Commit holds its claim through (#18):
    /// up to the drain gate's deadline
    /// (`HANDOFF_INPUT_DRAIN_DEADLINE`, 3 s), across turns of the main
    /// thread's run loop. This is the bound for a Commit that is stuck, so a
    /// control connection never waits on one for good. The main thread never
    /// waits at all: it alone raises and lifts the fence, so the person's
    /// Take Over and Hand Back answer [`HOLD_BUSY`] at once
    /// (`App::take_over_session`).
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

    /// How many times a waiter on this session has blocked on the fence
    /// ([`note_fence_block`]), every thread's.
    #[cfg(any(test, debug_assertions))]
    pub fn fence_waits(&self) -> u64 {
        self.fence_waits.load(Ordering::SeqCst)
    }

    /// The bound the latest waiter that blocked on this session's fence waits
    /// ([`note_fence_block`]): `Some(None)` for one that waits until the
    /// fence lifts, `None` while no waiter has blocked.
    #[cfg(any(test, debug_assertions))]
    pub fn fence_bound(&self) -> Option<Option<std::time::Duration>> {
        *self.fence_bound.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Wake every waiter parked on this session's condvar WITHOUT changing
    /// anything: a waiter on a fence still raised must block again.
    #[cfg(any(test, debug_assertions))]
    pub fn wake_waiters_for_test(&self) {
        let _inbox = self.lock();
        self.changed.notify_all();
    }

    /// The session ended: its program exited or its tab closed. Every waiter
    /// parked on this endpoint wakes now and answers `ERR exited` (`await
    /// inbox`; `wait` reads [`Self::closed`]), instead of `OK timeout` at its
    /// deadline while its lane holds the closed tab's PTY master open.
    pub fn close(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
    }

    /// Whether [`Self::close`] has run. Not named `closed`: the lock-order census
    /// resolves a call by its name, and `DeferredReaderGate::closed` would
    /// then read as taking this inbox lock.
    pub fn endpoint_closed(&self) -> bool {
        self.lock().closed
    }

    /// The delivered rows newer than `after` that `want` asks for, with the
    /// cursor to resume from and the oldest id still in the ring.
    ///
    /// THE PUSH FACE OF THE RING, and a LEAF READ: one short lock, no
    /// allocation beyond the matched rows' two short strings, and nothing
    /// written — `listed`, `seen` and the eviction counters are untouched, so a
    /// `subscribe … mail` subscriber never changes what a drain the agent runs
    /// itself would show. That is the whole difference between watching a
    /// mailbox and reading it.
    pub fn mail_since(&self, after: u64, want: &MailFilter) -> MailSince {
        let inbox = self.lock();
        // Every row id is minted from the pre-incremented `last_msg_id`, which
        // only ever rises, so a cursor at or past it has nothing newer: skip
        // both ring walks (the idle `mail` subscriber's every tick).
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
            // The LAST id assigned, not the ring's maximum: a filtered-out row
            // still advances the cursor, and an emptied ring must not rewind
            // it. The field was called `next_msg_id` while holding exactly
            // this — it is pre-incremented — and reading the name instead of
            // the code cost a `- 1` that re-pushed every row's first sighting
            // twice, which `subscribe_mail_pushes_…` caught. Renamed so the
            // next reader cannot make that trade.
            high: inbox.last_msg_id,
            present: inbox.rows.iter().filter(|r| r.id > after).count() as u64,
        }
    }

    /// The id of the newest row ever delivered here, 0 before the first: the
    /// [`Self::mail_since`] `high` any filter reports, read without a walk. A
    /// live `subscribe … mail` watch starts from it.
    #[must_use]
    pub fn last_msg_id(&self) -> u64 {
        self.lock().last_msg_id
    }

    /// The topics this session opted into with each one's `since=` token, in
    /// topic order.
    pub fn topics(&self) -> Vec<(String, TopicEntry)> {
        self.topics
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(t, e)| (t.clone(), e.clone()))
            .collect()
    }

    /// Whether this session holds `topic` in its opt-in set — what
    /// [`deliver_row`] asks of every `topic=` row, under the `inbox` guard
    /// (inbox → topics, the order `topic add|drop` already take). Not named
    /// `holds` or `contains`: the lock-order census resolves a call by its
    /// name.
    pub fn opted_into(&self, topic: &str) -> bool {
        self.topics
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(topic)
    }

    /// Add a topic with the `since=` token to resume it from. `Ok(true)` when it
    /// was not already there, `Ok(false)` when it was, and `Err(TopicsFull)`
    /// when it was not and the set already holds [`MAX_TOPICS`].
    ///
    /// A REPEATED ADD DOES NOT REWIND. The token is recorded only on the first
    /// add, because the bridge has by then resolved it to an offset and
    /// delivered from it: re-arming `since=@0` on a topic already running would
    /// replay the whole backlog into a ring that has already seen it. `drop`
    /// then `add` is how a caller asks for that on purpose.
    ///
    /// A change is recorded on `timeline` under the topics lock — the push the
    /// bridge acts on, in the order the set changed.
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

    /// [`Self::topic_add`], recording through `record` (a session's own
    /// [`FabricSession::record_fact`]), with the move's [`FabricFact::Topic`]
    /// for the session log's mark beside the row.
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
        // Bounded under the same lock that adds, so two adds racing at the
        // bound cannot both pass it.
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

    /// Drop a topic. `true` when it was there; recorded like [`Self::topic_add`].
    pub fn topic_drop(&self, t: &str, timeline: &Mutex<impl Timeline>) -> bool {
        self.topic_drop_recording(t, &|kind, payload, fact| {
            let _ = timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record_fact(kind, payload, fact);
        })
    }

    /// [`Self::topic_drop`], recording through `record`, as
    /// [`Self::topic_add_recording`] does.
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

    /// Replace the whole set — the handoff's carry, and nothing else.
    pub fn set_topics(&self, topics: impl IntoIterator<Item = (String, TopicEntry)>) {
        let mut set = self.topics.lock().unwrap_or_else(|p| p.into_inner());
        *set = topics.into_iter().collect();
    }

    /// Whether broker offset `off` names mail THIS session holds (what `send|turn cause=@<off>` may name):
    /// a row of its inbox
    /// ring, an offset its dedup set remembers delivering, or the offset a
    /// post of its own landed at. Never an offset of the fleet's lane merely
    /// below `bus_head` (other sessions' mail): an answer about one of those
    /// would be an oracle on traffic this session never saw. A leaf read of
    /// the inbox lock alone.
    pub fn holds_offset(&self, off: u64) -> bool {
        let inbox = self.lock();
        inbox.rows.iter().any(|r| r.off == off)
            || inbox.dedup_set.contains(&off)
            || inbox.posts.iter().any(|p| p.off == Some(off) && !p.dead)
    }

    /// The session's standing halt, if any — the one read `status` and the
    /// dispatch gate both take. With both origins standing it is the fleet's
    /// ([`Holds::first`]); [`Self::holds`] has both.
    pub fn hold(&self) -> Option<Hold> {
        self.lock().holds.first()
    }

    /// Every standing halt on the session, one slot per origin.
    pub fn holds(&self) -> Holds {
        self.lock().holds.clone()
    }

    /// How many acts the person's hold refused since it was set
    /// ([`halt_refusal`]); `0` with no person hold ever set.
    pub fn person_refusals(&self) -> u64 {
        self.person_refusals.load(Ordering::Relaxed)
    }

    /// THE HALT A LOST BRIDGE DID NOT MAKE on this session, while it still
    /// speaks for the link (`status hold_advisory=`): the hold [`bridge_lost`]
    /// would have applied under `[fabric] hold_on_bridge_lost = true`, as long
    /// as no newer bridge has attached since and no hold stands. Advisory
    /// only — the dispatch gate reads [`Self::hold`], never this.
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

    /// THE COMMIT'S READING OF THIS SESSION'S HOLD, and the fence that keeps
    /// it true until the process exits: under the guard every hold transition
    /// takes, mark the session committing and return its hold serial.
    ///
    /// Reading the serial and raising the fence under ONE guard is the whole
    /// point. A `hold` that took the guard first has already bumped the serial
    /// this returns, so the Commit sees it and stands down; one that takes it
    /// after finds the fence and waits ([`apply_hold`]). Before the fence the
    /// Commit compared the serial once and then ran the successor's
    /// activation, the harness suspend and the identity markers before its
    /// `_exit`, and a `hold on` answered `OK hold=1` inside that stretch
    /// reached neither the manifest nor the successor (the round-four review).
    ///
    /// `successor_reads_carry` says whether the fence holds back the bridge's
    /// retirements as well ([`Inbox::retirements_fenced`]): only when the
    /// successor carries the outbox on, which an older one does not.
    #[cfg(any(unix, test, debug_assertions))]
    pub fn fence_hold(&self, successor_reads_carry: bool) -> u64 {
        let mut inbox = self.lock();
        inbox.hold_fenced = true;
        inbox.retirements_fenced = successor_reads_carry;
        Self::serial_of(&self.hold_serial, &inbox)
    }

    /// Everything a handoff carries of this session's fabric that can move
    /// after the park drew the manifest: the hold serial and the outbox's
    /// moves ([`FabricCarry`]) — a post queued after the draw refuses the
    /// Commit (a lossless retry). Inbound rows are not in it: their ids are
    /// kept apart by [`INBOX_ID_RESERVE`] ([`Self::past_handoff_ceiling`]).
    /// What `App::hold_serials` sums at
    /// the park and the Commit compares again under [`Self::fence_hold`],
    /// which returns the same sum.
    #[cfg(any(unix, test, debug_assertions))]
    pub fn carry_serial(&self) -> u64 {
        let inbox = self.lock();
        Self::serial_of(&self.hold_serial, &inbox)
    }

    /// The one sum [`Self::carry_serial`] and [`Self::fence_hold`] return.
    ///
    /// The inbox row id is NOT in it:
    /// mail from outside this desk arriving in the overlap would revoke
    /// every attempt. Ids are kept apart by [`INBOX_ID_RESERVE`] instead, and
    /// only a process that outruns it stands down
    /// ([`Self::past_handoff_ceiling`]).
    #[cfg(any(unix, test, debug_assertions))]
    fn serial_of(hold_serial: &AtomicU64, inbox: &Inbox) -> u64 {
        hold_serial
            .load(Ordering::Acquire)
            .wrapping_add(inbox.outbox_moves)
    }

    /// Whether this session has numbered an inbox row past the id the last
    /// handoff manifest carried ([`Inbox::msg_id_ceiling`]): the successor
    /// would number a row with the same id, so the Commit stands down. Read
    /// by the Commit after [`Self::fence_hold`], under which no delivery
    /// crosses the ceiling ([`deliver_row`] waits instead).
    #[cfg(any(unix, test, debug_assertions))]
    pub fn past_handoff_ceiling(&self) -> bool {
        let inbox = self.lock();
        inbox
            .msg_id_ceiling
            .is_some_and(|ceiling| inbox.last_msg_id > ceiling)
    }

    /// Wait, for as long as the Commit's fence stands, before a delivery this
    /// process must not answer while the Commit is on its way to `_exit`:
    ///
    /// * a row that would be numbered past the manifest's carried id
    ///   ([`Inbox::msg_id_ceiling`]) — the successor hands that id out too;
    /// * ANY BROADCAST (`topic=`), whatever its id (open problem 201).
    ///   The bridge refills a relaunched session's
    ///   ADDRESSED lane from its refill floor ([`addressed_floor`]), but it
    ///   moves a topic's cursor past a broadcast on the `OK` and resumes the
    ///   successor from there, so a broadcast answered under the fence would
    ///   land only in the ring the `_exit` discards. Left unanswered, the
    ///   `_exit` drops the bridge's lane with the cursor unmoved
    ///   (`Delivery::Unaccounted` in aterm-link) and the successor's bridge
    ///   offers it again.
    ///
    /// NO DEADLINE OF ITS OWN, on purpose: the only caller is the bridge's
    /// `deliver`, which books `ERR quota`, `ERR too large`, `ERR no such
    /// session` and `ERR not-subscribed` as the recipient's verdict
    /// (`Delivery::Accounted` in
    /// aterm-link — the record refused to its sender, a broadcast dropped, the
    /// cursor committed past it), so a timeout spelled as one of those would
    /// lose the message; any other answer is a lane fault that holds the
    /// session back (`Delivery::Owed`), which a busy fence is not either. The
    /// fence's own end is the deadline —
    /// the update lands (`_exit`, and the successor's bridge delivers the
    /// record again) or stands down (the fence lifts and it lands here) — and
    /// every [`MSG_ROOM_NOTICE_EVERY`] spent waiting is a WARN in `aterm.log`
    /// naming the session, never a line on the GUI's stderr, which nobody
    /// reads (open problem 203). An addressed row below the ceiling does not
    /// wait: the reserve is what keeps inbound mail from stalling or vetoing
    /// an update.
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
                // Logged under the guard, as `seed_carry` logs: one line per
                // round, and the guard is what the next round waits with — a
                // helper that dropped and took it again would return a guard
                // it acquired, which the lock census cannot place.
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

    /// Wait out a committing update's fence ([`Self::fence_hold`]) before a
    /// change the handoff carries — a post, a receipt owed, a retirement —
    /// lands: `None` past `wait`, with the guard released and nothing moved,
    /// which the caller answers [`HOLD_BUSY`]. An update that lands `_exit`s
    /// with the waiter still waiting, so nothing is ever acknowledged that the
    /// successor did not adopt; one that stands down lifts the fence and the
    /// change lands here.
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

    /// Run `change` — a change the handoff carries that lives off this
    /// endpoint (a task mark, `task_record::mark_session_task`) — once no
    /// committing update's fence stands ([`Self::fence_hold`]), and under the
    /// guard the fence is raised with, so a Commit that raises it later reads
    /// the change, and one that raised it first is waited out
    /// ([`Self::wait_unfenced`]). `None` past `wait`, with `change` not run.
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

    /// A MANUAL RESET THAT RAN DIRECTLY ON THIS SESSION'S ENGINE
    /// (`manual_reset::run_direct`), outside the byte stream, made known to a
    /// seamless Commit before it is answered: `true` once it moved
    /// [`Self::hold_serial`], `false` when a Commit's fence stood for all of
    /// `fence_wait`, with the serial unmoved (the engine is reset all the same).
    ///
    /// On a parked session (the park closes the reset lane) the reset lands
    /// after the carry the successor restores was taken. The serial is moved
    /// under the guard [`Self::fence_hold`] takes: a Commit that reads it later
    /// stands down (a lossless retry; this engine runs on, reset), and while a
    /// Commit's fence stands — it has read the serial and is on its way to
    /// `_exit` — this waits ([`Self::wait_unfenced`]), so the update lands with
    /// the reset unanswered, never answered `OK` and then undone by the
    /// successor's restore.
    ///
    /// AFTER THE RESET, NEVER BEFORE: a park reads the serial before it takes
    /// the carry, so a move it read is a reset already on the engine, in that
    /// carry. Moved first, a park reading it in the gap before the reset took
    /// the term lock would carry the engine unreset and match at its Commit.
    ///
    /// `fence_wait` is `None` on the main thread (the menu), which raises and
    /// lifts the fence itself: it moves the serial without waiting, and a
    /// claim held across the run loop's dispatch reads it again
    /// (`HoldFence::serials`).
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

    /// [`Self::wait_unfenced`] for a RETIREMENT (`outbox sent`, `deliver …
    /// receipt=`): it waits only when the fence holds retirements back
    /// ([`Inbox::retirements_fenced`]); for a successor that does not read the
    /// carry it is answered at once, as every earlier build answered it.
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

    /// THE OUTBOUND HALF THE HANDOFF CARRIES ([`FabricCarry`]): the id
    /// counters, the posts still waiting and the receipts still owed, read
    /// under one guard. `budget` is what is left of [`CARRY_MAIL_BYTES`] for
    /// the whole manifest; a post past it (or one holding a number TOML cannot
    /// write) is named in `uncarried` instead, and a receipt past it is
    /// counted in `receipts_uncarried`.
    ///
    /// The inbox row id it carries is [`Inbox::last_msg_id`] PLUS
    /// [`INBOX_ID_RESERVE`], remembered as this process's ceiling
    /// ([`Inbox::msg_id_ceiling`]): the bridge goes on delivering until the
    /// `_exit`, and the rows it numbers in that overlap stay below every id
    /// the successor hands out.
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
        // THE ONES THE SUCCESSOR'S BRIDGE OFFERS AGAIN: a refilling bridge
        // took them, on this log, and the session still takes the topic. The
        // topic set is read under the inbox guard (inbox → topics, the order
        // `topic add|drop` take).
        //
        // AND THE BRIDGE STILL REMEMBERS THEM. It keeps a session's newest
        // [`RING_CAP`] unhandled broadcasts and drops the oldest past that
        // (aterm-link's `UNHANDLED_KEEP`), so a row this ring kept ahead of
        // newer ones it evicted — a person's, or one left unlisted while newer
        // ones were listed — can be gone from the floor while still held
        // here, and no successor is offered it. Dropping a row takes that many
        // entries newer than it, each a row minted here after it, so a row
        // with fewer than [`RING_CAP`] rows minted after it is still
        // remembered; one with more is counted lost — once too often, when
        // the bridge kept it after all, never once too few.
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

    /// What this session's keeper META carries of its fabric ([`KeeperCarry`]):
    /// the topics under their lock, then the hold, the lease, the queued post
    /// ids, the owed count and `dropped=` under the leaf guard — never nested.
    /// Read by the keeper link's META pump with no other lock held.
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

    /// Put back what a SEAMLESS UPDATE carried of this session's mailbox — the
    /// handoff's carry ([`FabricCarry`]), or what a PTY keeper recovery read
    /// off the session's META (counters and queued ids, never rows:
    /// [`KeeperCarry`]). Called by
    /// `spawn_session` with the hold and the topics, before the adopted session
    /// is registered, so the bridge's first `outbox` drain here already lists
    /// the carried posts and receipts, and every id this session hands out from
    /// now on is above the ones the old process handed out.
    ///
    /// A post the handoff could not carry, or whose row does not validate, is
    /// retired DEAD here with `reason=not-carried` and a `post-landed <id>
    /// off=- reason=not-carried` timeline row: the sender learns the post is
    /// gone, which is what it was owed. A receipt that does not validate is
    /// dropped (its ask's sender waits out its own deadline, as for any
    /// receipt that never comes).
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
        // `dropped=` GOES ON COUNTING, and the update's own loss is counted in
        // it: the old process's drops and the broadcasts it held unhandled
        // that no bridge offers again are both mail this session will not be
        // shown again. The refilled ones are not lost: this session's bridge
        // offers them again ([`FabricCarry::broadcasts_refilled`]), and
        // counting them as well would say so twice.
        //
        // BOUNDED AS THE CARRY THAT WROTE IT IS ([`CARRY_INT_MAX`]): a keeper
        // META is read leniently and its count can be any `u64`, and from
        // `u64::MAX` the ring's next `+= 1` (`evict_for`) panics under overflow
        // checks or wraps to `dropped=0` — the reset the count is carried to
        // prevent.
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
        // The carry separates clocks for older readers, not queue order.
        // Restore the shared oldest-first bound even when a later verdict on
        // an old row was owed after a receipt of the current log.
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

    /// The attempt that fenced this session stood down: let the holds that
    /// waited for it land ([`Self::fence_hold`]).
    #[cfg(any(unix, test, debug_assertions))]
    pub fn lift_hold_fence(&self) {
        let mut inbox = self.lock();
        inbox.hold_fenced = false;
        inbox.retirements_fenced = false;
        drop(inbox);
        self.changed.notify_all();
    }

    /// Put back the holds another process carried ([`parse_holds`] already
    /// made them holds): a SEAMLESS UPDATE's and a PTY keeper's recovery's,
    /// each origin's slot, so the owner's stop and the fleet's halt both stand
    /// again, apart, as they stood; and a cold restore layout's, the owner's
    /// slot alone, also filling an absent owner slot on a keeper's recovered
    /// shell (this module's header).
    ///
    /// A seamless update's and a keeper recovery's are seeded by
    /// `spawn_session` on the adopted session before `register_session`, so
    /// before any control verb can resolve the session, let alone drive it:
    /// there is no instant at which the adopted shell is reachable unhalted. A
    /// cold restore's is seeded as its reopened tab takes the leaf
    /// (`App::carry_restored_identity`): a window's first tab, its shell
    /// started at launch, is reachable before that (ruling 23's residual).
    /// Recorded on the timeline under this guard, like every hold transition,
    /// as one `1 reason=… origin=… hold_origin=… carried=1` ([`Holds::event`])
    /// — the `carried=1` says the halt was not set in this process, which is
    /// the question a reader of `events` after an update or a restore asks.
    /// Never refused and never a lift: a seed only ever halts, and the origin
    /// it carries is the origin the fleet fence reads from here on. Nothing
    /// carried, or nothing moved (every slot it carried already stood),
    /// records nothing.
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
        // Seeded before the session is registered: nobody can be watching
        // its log yet, so the owed wake is moot.
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

    /// **THE PRESENCE FACTS' MAIL READ** (aterm-gui's `App::sense`): the mail
    /// counts the session's presence slot levels on (`status level=` and
    /// `why=`, `aterm_messages::presence`) and the `(kind, from, trust)` of the
    /// row it names — an unread `task` or `ask` if one waits (the row that
    /// makes it a wait state, so a later note never hides it), else the newest
    /// unread row — trust FIRST on the wire because it is the receiver's verdict.
    /// `unread` is every row above the HANDLED watermark, `pending` those of
    /// them a non-peek `inbox` has already LISTED (read, not yet decided — a
    /// fresh delivery is unread and not pending), `dropped` the monotone
    /// eviction count,
    /// `queued` the posts still waiting for a bridge to carry them, `head` the
    /// newest row id ever delivered (how the story counts arrivals). One leaf
    /// lock, taken at CHANGE rate (never per frame); the only allocation is the
    /// three-token copy of the newest row.
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
        // The newest unread row — unless a TASK or ASK waits anywhere above
        // the watermark: that is the row the amber rim is lit for, so it is
        // the row the mail slot names (a later note never hides it).
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

    /// **THE ROOM'S READ** (the resident pet, `kitty_pet` panel #9(e)) —
    /// `(unseen, overdue, hold)`: the newest row above the HANDLED watermark
    /// by id (`0` when every row is handled), whether any such row's advisory
    /// deadline has passed (`dl=` is milliseconds from the row's arrival
    /// `t_ms`, on [`now_ms`]'s clock — advisory, so a passed one is a fact
    /// about the mail, never a verdict on anyone), and whether a hold stands.
    /// A short leaf-lock read with no allocation, taken by the render thread
    /// on frames it already draws; the ring is bounded by [`RING_CAP`], so
    /// the scan is too.
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

// The instance's bridge link

/// `fabric=` states, in the order [`FabricLink::state`] encodes them.
const FABRIC_ABSENT: u8 = 0;
const FABRIC_CONNECTED: u8 = 1;
const FABRIC_DISCONNECTED: u8 = 2;
/// A bridge is ATTACHED (both lanes served, the process alive) but its own link
/// to the broker is DOWN — the dial failed, the broker closed on it, or an ack
/// never came. The state a killed broker, a wrong socket path and a wedged
/// broker all produce, and the one `connected` used to hide (measured
/// 2026-09-12: a bridge pointed at a socket nothing served reported
/// `connected` for as long as it lived). See [`link_report`].
const FABRIC_STALLED: u8 = 3;

/// The bridge's own `LINK_REFRESH` (`aterm-link/src/bridge.rs`), mirrored here
/// because `aterm-gui` does not depend on `aterm-link`: after an ack, the
/// longest a bridge lets its last `link up` go unrefreshed WHILE IT IS ACKING.
/// A bridge that is getting answers therefore re-reports within this, and one
/// that is not says nothing at all.
const LINK_REFRESH: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a bridge may OWE an answer before `fabric=` reads `stale` — the
/// contract's three refreshes.
///
/// IT IS MEASURED FROM THE WORK, NOT FROM THE LAST ACK, and that is the whole
/// design. `fabric_link_age_ms=` on a quiet link grows without bound by
/// construction ([`LinkReport`]: "a quiet link sends nothing, and `acked_at`
/// simply ages"), so a rule reading "the last ack is older than this" is FALSE
/// on every healthy idle fleet. Measured on the owner's live instance
/// 2026-09-20, twice, twenty seconds apart, against a broker that answered a
/// head query in 20 ms:
///
/// ```text
/// fabric=connected fabric_rtt_ms=14 fabric_link_age_ms=277532
/// fabric=connected fabric_rtt_ms=14 fabric_link_age_ms=297547
/// ```
///
/// 297 s of "staleness" on a link with nothing whatever wrong with it — and
/// `aterm ctl status`'s own shipped prose already says so
/// (`aterm-types/src/control_verbs.rs`: "a large age on `connected` means a
/// quiet link, not a dead one, and only the next exchange can tell"). Reading
/// that as `stale` would refuse the wait on every `ask` and `task` on the
/// fleet at once — the exact regression [`link_evidence`] was added to undo.
///
/// So the clock starts when the endpoint HANDS THE BRIDGE WORK
/// ([`note_work_owed`]) and settles on any evidence the bridge is moving it
/// ([`settle_work_owed`]: cleared when nothing is outstanding, re-stamped
/// otherwise). Silence is not read as death; an unanswered ASK is.
const STALE_AFTER: std::time::Duration = LINK_REFRESH.saturating_mul(3);

/// The development seam (`aterm_types::dev_seam!`, so a debug or `dev-seams`
/// build's only: a shipped instance reads none) that sets the owed-answer
/// horizon, [`STALE_AFTER`], in whole milliseconds ([`stale_after`]).
///
/// FOR A WORLD WHOSE PROOF IS NOT THE HORIZON: `fabric_world`'s instances set
/// it past every verb's own wait. A loaded machine's live bridge has owed an
/// answer for six seconds while still moving it (its push wake, outbox peek,
/// roster read, state writes and commit to the broker, no one exchange past
/// its own ack deadline), and a `post --wait` cut short there answered `ERR
/// fabric stale id=<n> queued=1` for a post that landed moments later. Never
/// for a test whose proof IS the stale verdict: those age the owed clock
/// (`backdate_work_owed`) and keep the production horizon.
const STALE_AFTER_SEAM: &str = "ATERM_FABRIC_STALE_AFTER_MS";

/// How long a bridge may owe an answer before `fabric=` reads `stale`:
/// [`STALE_AFTER`], or [`STALE_AFTER_SEAM`]'s whole milliseconds in a build
/// that reads seams. Read once.
fn stale_after() -> std::time::Duration {
    static AT: std::sync::OnceLock<std::time::Duration> = std::sync::OnceLock::new();
    *AT.get_or_init(|| {
        aterm_types::dev_seam!(STALE_AFTER_SEAM)
            .and_then(|ms| ms.to_str()?.parse::<u64>().ok())
            .map_or(STALE_AFTER, std::time::Duration::from_millis)
    })
}

/// The reason token the link carries between a bridge's attach and its first
/// report: no dial has been answered yet, one way or the other.
const LINK_STARTING: &str = "starting";
/// The reason [`bridge_lost`] stamps: the bridge itself is gone, so its link is
/// by definition not up.
#[cfg(any(unix, test, debug_assertions))]
const LINK_BRIDGE_LOST: &str = "bridge-lost";
/// The most bytes a `link down reason=` token is kept at.
const LINK_REASON_MAX: usize = 32;

/// What the bridge last said about ITS broker link — the record behind
/// `status`'s `fabric_rtt_ms=` / `fabric_link_age_ms=` and `fabric status`'s
/// `reason=`.
///
/// NO HEARTBEAT. The bridge sends a `link` record on every change and after an
/// ack that moved the round trip by more than 2x (and, so the age stays honest on
/// a busy link, after an ack at most once per 2 s otherwise); a quiet link sends
/// nothing, and `acked_at` simply ages. That is the design's rule (§7 forbids a
/// heartbeat storm): the endpoint reports what the bridge OBSERVED, and how long
/// ago, and never infers liveness from silence.
struct LinkReport {
    /// Whether the last report said the link was up.
    up: bool,
    /// The last `down` reason, or [`LINK_STARTING`] / [`LINK_BRIDGE_LOST`].
    /// Empty while up.
    reason: String,
    /// The round trip of the last acknowledged exchange, in ms.
    rtt_ms: Option<u64>,
    /// When that ack was reported.
    acked_at: Option<std::time::Instant>,
    /// When the CURRENT stall began — the instant the link was last reported
    /// DOWN after being up (or after `starting`, when the dial itself
    /// failed). `None` while up, and `None` for a fresh attach that has said
    /// nothing yet: a link that never came up has no stall to date, and the
    /// band prints `~` with no figure rather than a number it made up
    /// (the prior review measured `~ 0s` for the life of a dialing bridge,
    /// and `~ 3600s` one second into the stall of a link that had been quiet
    /// for an hour — the ack's age, [`fabric_link_facts`], is NOT the stall's).
    stalled_at: Option<std::time::Instant>,
    /// Whether THIS incarnation has ever sent a `link` record. A bridge from
    /// before the link report (0.85 and earlier) never does, and for one the
    /// endpoint takes a delivery or a landing as the evidence it is — see
    /// [`link_evidence`].
    reported: bool,
    /// When the endpoint last handed this bridge work it has shown no sign of
    /// having moved — the instant a post was queued into the outbox with no
    /// ack, landing or link report since. `None` when the bridge owes nothing,
    /// which is the state of every quiet fleet.
    ///
    /// THIS IS THE ONE FIELD `stale` IS DERIVED FROM, and it is deliberately
    /// not `acked_at`: see [`STALE_AFTER`]. It is an `Option<Instant>` rather
    /// than a count so that a second post while one is already owed does not
    /// restart the clock — the bridge has been silent since the FIRST one.
    owed_since: Option<std::time::Instant>,
    /// What THIS incarnation said it is in `hello` ([`cmd_hello`]): its lane
    /// grammar and its build. `None` for a bridge from before the handshake,
    /// and before the first `hello` of a fresh attach.
    hello: Option<BridgeHello>,
    /// THE BUS MIRROR as THIS incarnation last reported it (`link mirror`):
    /// `None` before any report, and always for a
    /// bridge whose `hello` did not ask for the mirror.
    mirror: Option<MirrorReport>,
    /// How many of THIS incarnation's bridge-only lines this endpoint answered
    /// `ERR usage` — a line it could not parse ([`note_lane_fault`]). A matched
    /// pair writes none, so any count is the two not reading the lane alike.
    lane_faults: u64,
}

/// A bridge's `hello`: the lane grammar it reads and writes, its build, and
/// whether it refills a kept session's broadcasts (`refill=…say`).
#[derive(Clone)]
struct BridgeHello {
    lane: u32,
    build: Option<String>,
    refills_broadcasts: bool,
    /// What it said of the session-log mirror (`log=`):
    /// `turns` is acknowledged in the answer only when the hello was recorded.
    log: HelloLog,
}

/// A bridge `hello`'s `log=` word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HelloLog {
    /// No `log=`, or a word this endpoint does not read: no mirror.
    Off,
    /// `log=auto` (`[fabric] log = "auto"`, the default):
    /// the bridge mirrors once it has attached to a broker that
    /// retains its node's `ev` faces, and says `hello` again with `log=turns`
    /// then; until it does it asks for nothing, and over a broker that
    /// retains nothing it reports `link mirror unretained`.
    Auto,
    /// `log=turns`: it asks this endpoint to serve `log-take`/`log-landed`.
    Turns,
}

/// The bus mirror's state as the bridge reported it: `on`, `stopped` (a
/// definite refusal that is not the subject budget: the mirror stops for that
/// bridge's life, its pin kept for a later one), `capped` (the
/// node's total of mirrored bytes reached on a broker that retains nothing),
/// `budget` (the node's bytes in a retaining broker's window
/// at their budget: the mirror waits, and goes on once the broker gives the
/// oldest up) or `unretained` (an
/// `auto` bridge over a broker that retains none of its `ev` faces: the
/// mirror does not run), with a reason token.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MirrorReport {
    state: &'static str,
    reason: String,
}

/// WHAT THE OWNING BRIDGE LAST SAID OF THE BUS LOG (`link log`): the
/// identity its offsets index, and — when a bridge at lane 3 found a MOVED
/// GENERATION — the earlier identity it continues and the offset below which
/// the two name the same records (`continues=<f> was=<id>`). Kept beside the
/// identity, not only applied to the sessions live at the switch, because a
/// session registered later still carries the clock it was carried under
/// ([`register_on_log`]): one on `was` keeps what is below `f` too.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LogClock {
    /// `<32 hex>.<generation>`.
    ident: String,
    /// `(was, f)`: `ident` continues `was` below offset `f`.
    continues: Option<(String, u64)>,
}

impl LogClock {
    /// Where a session whose clock names `held` keeps its offsets on this
    /// log: `None` — none (another log, or no clock at all) — or `Some(f)`,
    /// below `f`. The same identity is never asked: it keeps everything.
    /// The bridge's `f` is already the minimum common prefix over every
    /// lineage step from `was` to `ident`, capped at the current head. A
    /// session holding an intermediate generation therefore keeps that prefix
    /// too. Generation ordering selects a member of that producer-attested
    /// chain; ordering alone does not establish a common prefix.
    fn keep_below(&self, held: Option<&str>) -> Option<u64> {
        let (was, fork) = self.continues.as_ref()?;
        let held = held?;
        let in_chain = held == was.as_str()
            || (is_earlier_generation(was, held) && is_earlier_generation(held, &self.ident));
        in_chain.then_some(*fork)
    }
}

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
    /// halts, and it grows only through BRIDGE-issued verbs (`deliver`, `outbox
    /// sent`, and a `hold` from the bridge connection — never an Owner's local
    /// one), so a session the bridge never governed is never halted by its death.
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
    ///
    /// KEYED BY REGISTRY, because the prune's evidence is. The set is
    /// process-global; the predicate that shrinks it — "still in the registry" —
    /// can only ever speak for the ONE store the calling bridge verb was handed.
    /// Unkeyed, a verb on registry A intersected the whole set with A's live
    /// handles and so evicted every sid registry B was still owed a halt for,
    /// silently, on a path whose entire purpose is to fail closed. Production
    /// runs one registry and never saw it; the test suite builds one per test
    /// and saw it as a schedule-dependent flake (see
    /// `one_registrys_prune_does_not_evict_another_registrys_governed_sids`).
    touched: Mutex<BTreeMap<usize, TouchedRegistry>>,
    /// Whether a bridge SUPERVISOR is running in this process.
    ///
    /// `state` answers "is a bridge attached RIGHT NOW"; this answers the
    /// different question a parked `post --wait` needs — "can one ever attach".
    /// [`FABRIC_ABSENT`] covers both "the first bridge has not finished
    /// attaching" and "this build has no `[fabric] command`, so no bridge will
    /// EVER attach", and those two owe a caller opposite advice. Set once by
    /// aterm-gui's `fabric_launch::arm` — at startup, or later when `fabric attach`
    /// arms a running instance; never cleared, because the supervisor relaunches
    /// forever and a supervisor that has run is a bridge that can come back. See
    /// [`fabric_wait_refusal`].
    supervised: AtomicBool,
    /// The bridge's last word on its broker link. WRITTEN ONLY under
    /// `generation`, like `state`, because a report and an attach/loss must not
    /// interleave: a `link up` from a ghost lane landing between a newer attach's
    /// compare and its store is the exact shape [`BridgeGeneration`] closes.
    link: Mutex<LinkReport>,
    /// The identity of the bus log the owning bridge last said its offsets
    /// index (`link log <identity>`, astream's `<id>.<generation>`), and where
    /// it continues an earlier generation when a lane-3 bridge said so
    /// ([`LogClock`]). `None` until a bridge has said one; a change is a log
    /// switch, and every session forgets the offsets the new log does not
    /// hold ([`cmd_link_log`]).
    log: Mutex<Option<LogClock>>,
    /// Orders a complete log switch before another bridge incarnation can
    /// attach or a newly registered session can become visible. Taken before
    /// the registry, session and link locks, never under any of them.
    log_transition: Mutex<()>,
    /// `[fabric] hold_on_bridge_lost` ([`set_hold_on_bridge_lost`]): whether a
    /// lost bridge HALTS every session it governed (`true`, the fail-closed
    /// sweep as it was before 2026-09-29) or leaves them running with an
    /// advisory on each (`false`, the default). Set once at startup from the
    /// config; a test sets it inside its [`with_link_reset`] section.
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
    /// A new bridge incarnation attached ([`bridge_attached`]): `stalled`.
    Attached,
    /// A bridge that never reports was read by what it did
    /// ([`link_evidence`]): `connected`.
    Evidence,
    /// The bridge's own report of its broker link ([`link_report`]).
    Report,
    /// The owning incarnation's lane closed ([`bridge_lost`]): `disconnected`.
    Lost,
}

/// One STORED transition of the link: the instance node log's `link state=`.
/// The node log is aterm-gui's and this crate
/// has no aterm-slog dependency, so a transition reaches it through
/// [`on_link_moved`]'s hook. Only stored states move: the derived `stale`
/// ([`fabric_state`]) has no event to report it at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkMove {
    /// The stored state's [`fabric_state`] word: `stalled`, `connected` or
    /// `disconnected`.
    pub state: &'static str,
    /// The bridge incarnation it speaks for.
    pub generation: u64,
    /// How it came about.
    pub said: LinkSaid,
}

/// What hears each [`LinkMove`].
pub type LinkMoved = fn(LinkMove);

/// The hook [`on_link_moved`] installed.
static LINK_MOVED: std::sync::OnceLock<LinkMoved> = std::sync::OnceLock::new();

/// Have every stored link transition call `hook`: an attach (a new
/// incarnation, so always), a change of the stored state by a report or a
/// delivery's evidence, and the owner's loss. The hook runs UNDER THE
/// GENERATION LOCK every writer of the state stores under, so it hears the
/// transitions in the order they were stored. LOCK ORDER: `LINK.log_transition`
/// → `LINK.generation` → `LINK.link` → the hook's own leaf lock; the hook must
/// take nothing else and call nothing of this crate. The first caller's hook
/// stands (one instance, one node log).
pub fn on_link_moved(hook: LinkMoved) {
    let _ = LINK_MOVED.set(hook);
}

/// Tell the installed hook that `state` was stored for `generation`. Called
/// with `LINK.generation` held, and only for a transition.
fn link_moved(state: u8, generation: u64, said: LinkSaid) {
    if let Some(hook) = LINK_MOVED.get() {
        hook(LinkMove {
            state: state_word(state),
            generation,
            said,
        });
    }
}

/// A stored state's word, as [`fabric_state`] spells it.
const fn state_word(state: u8) -> &'static str {
    match state {
        FABRIC_CONNECTED => "connected",
        FABRIC_DISCONNECTED => "disconnected",
        FABRIC_STALLED => "stalled",
        _ => "absent",
    }
}

/// Publish a newly created or adopted session into the registry. The callback
/// takes the registry lock only after this session's fabric is ready for readers.
///
/// A `link log` sweep snapshots the registry under this same transition lock:
/// either it includes this session, or registration first reconciles the session
/// with its completed result. Carried offsets are never exposed between those
/// two steps. This runs before publication, so no Commit can have fenced the
/// newly created context's carry yet. A session carried on the generation the
/// current log CONTINUES ([`LogClock`]) keeps what is below the fork, as the
/// sessions the switch found live did.
pub fn register_on_log<T>(ctx: &dyn FabricSession, publish: impl FnOnce() -> T) -> T {
    let _transition = LINK
        .log_transition
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let current = {
        // Parallel tests own different instances; use the same isolation as
        // fabric_state_seen rather than importing another test's clock.
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
    /// The [`BridgeGeneration`] the CURRENT THREAD's bridge lane was served
    /// under, set by `control::serve_bridge` for the life of that lane. A
    /// `link` report is accepted only for the generation that owns the link,
    /// and the dispatch — which has the verb line and the registry, but not the
    /// lane — reads it from here. `None` on any other thread (a test calling the
    /// handler directly, an Owner connection that is refused before it gets
    /// here).
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

/// One registry's governed sids, held under [`FabricLink::touched`].
///
/// THE KEY IS THE ALLOCATION ADDRESS, AND IT CANNOT BE REUSED WHILE WE REMEMBER
/// IT. `Store` is an `Arc`, and an `Arc`'s allocation is freed only once BOTH
/// its strong and its weak count reach zero — so the `Weak` this slot holds pins
/// the address for exactly as long as the slot exists. No second registry can
/// ever be handed a key that still names a first one, and the classic
/// pointer-as-identity hazard is structurally absent rather than argued about.
///
/// The `Weak` is also what keeps the size bound honest: a registry that has been
/// dropped fails to upgrade, and the next prune takes its sids with it, so the
/// set stays bounded by the LIVE sessions of the LIVE registries.
struct TouchedRegistry {
    /// The registry these sids were governed under, held weakly — `touched` must
    /// never be the reason a registry outlives the code that owns it.
    registry: Weak<RwLock<dyn Registry>>,
    /// The sids this registry has governed and not yet been pruned of.
    sids: BTreeSet<String>,
}

/// The [`FabricLink::touched`] key for a registry. See [`TouchedRegistry`] for
/// why an address is a sound identity here.
fn registry_key(store: &Store) -> usize {
    Arc::as_ptr(store) as *const () as usize
}

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
/// aterm-gui's `control::BridgeLostGuard`s exist per bridge (the verb lane and the
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
/// This is the link-state analogue of the hold's `converge_hold`/`apply_fleet_halts`
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
pub struct BridgeGeneration(u64);

/// Mint the identity of one bridge incarnation. Called ONCE per launch, before
/// either near end is served. Never `0`, which is reserved for "no bridge has
/// ever attached" so a zero-valued guard cannot match a fresh link.
#[cfg(any(unix, test, debug_assertions))]
pub fn next_bridge_generation() -> BridgeGeneration {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    BridgeGeneration(NEXT.fetch_add(1, Ordering::Relaxed).wrapping_add(1))
}

/// A bridge SUPERVISOR has started in this process — record it once.
///
/// Called by aterm-gui's `fabric_launch::arm` when, and only when, a supervisor
/// thread really started — the ONE seam shared by the startup path
/// (aterm-gui's `fabric_launch::spawn_supervisor`) and the `fabric attach` control
/// verb, so the latch also flips at RUNTIME on an instance that started with no
/// `[fabric] command`. It is not a claim that a bridge is up; it is the claim
/// that one is coming, which is the half [`fabric_state`] cannot express
/// (`absent` means both "not yet" and "never").
#[cfg(any(unix, test, debug_assertions))]
pub fn note_bridge_supervised() {
    LINK.supervised.store(true, Ordering::Relaxed);
}

/// Whether a bridge SUPERVISOR is running in this process — the latch
/// [`note_bridge_supervised`] sets, read back for `fabric status`'s
/// `supervised=` token. This is the SAME bit [`fabric_wait_refusal`] reads, so
/// `supervised=1` and `post`'s `queued=1` cannot disagree.
#[cfg(unix)]
pub fn bridge_supervised() -> bool {
    LINK.supervised.load(Ordering::Relaxed)
}

/// Whether any bridge can EVER attach to this instance: a supervisor is running,
/// or one already has and the link is merely down.
fn bridge_reachable() -> bool {
    LINK.supervised.load(Ordering::Relaxed) || LINK.state.load(Ordering::Relaxed) != FABRIC_ABSENT
}

/// Whether the bridge has owed an answer for longer than [`stale_after`].
///
/// Read at call time from [`LinkReport::owed_since`], exactly as the ack's age
/// is ([`fabric_link_facts`]) — there is no timer and no sweep, because the
/// fact IS a duration and a stored flag would only be a copy of one that could
/// go out of date. Lock order: this takes `LINK.link`, and every writer of it
/// releases it before touching a session (see [`wake_parked`]), so the one
/// order that exists is `ctx.fabric` → `LINK.link`.
fn link_is_stale() -> bool {
    let link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
    link.owed_since
        .is_some_and(|t| t.elapsed() >= stale_after())
}

/// How long until an owed answer goes [`stale_after`]-stale, or `None` when the
/// bridge owes nothing. `Some(0)` means it already is.
///
/// A parked `post --wait` uses this to BOUND ITS SLEEP: the condvar is only
/// signalled by an event, and a wedged bridge produces none, so without a cap
/// the waiter would sleep out its whole `--wait` (up to `WAIT_MAX_MS` = 600 s)
/// for a verdict that became true six seconds in. This is the same argument
/// [`wake_parked`] makes for the `down` transition, for the case where there
/// is no transition to hang a wake on.
fn stale_in() -> Option<std::time::Duration> {
    let link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
    link.owed_since
        .map(|t| stale_after().saturating_sub(t.elapsed()))
}

/// Note that the endpoint has handed the bridge work. The clock starts on the
/// FIRST unanswered one and is not restarted by later ones.
fn note_work_owed() {
    // ONLY A CONNECTED BRIDGE CAN OWE ANYTHING. `stale` is a refinement of
    // `connected` and is read from nowhere else, so arming the clock while the
    // state is `absent`, `disconnected` or `stalled` records a debt no reader
    // can see and no event is guaranteed to clear — those three already say
    // "your post is queued and not moving", which is the whole message.
    //
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
    // A SIBLING TEST'S `post` OWES THIS BRIDGE NOTHING (see [`LINK_SECTION`]),
    // and the gate above is not enough to say so: it refuses a write
    // while the link is not connected, which is exactly the state a live
    // section has moved it OUT of. So a test running outside the section, whose
    // own store has posted, armed the debt inside someone else's measurement
    // and `a_bridge_that_owes_an_answer_reads_stale_and_a_quiet_one_does_not`
    // read `stale` where it had established `connected`. Every other writer of
    // this struct already carries this guard; this one was the last without it.
    #[cfg(any(test, debug_assertions))]
    if link_section_is_foreign() {
        return;
    }
    let mut link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
    if link.owed_since.is_none() {
        link.owed_since = Some(std::time::Instant::now());
    }
}

/// Whether ANY session on this instance is still waiting on the bridge for a
/// post: queued, not landed (`off` unset), and not retired as dead.
///
/// Derived from the registry rather than counted, because a count drifts. A post
/// can leave a ring without passing `outbox sent` — the session closes, or the
/// ring trims — and a counter that missed one of those would leave the instance
/// permanently in debt and permanently `stale`. Reading the rows cannot drift.
///
/// LOCK ORDER. The ctxs are cloned out from under the registry guard first and
/// the fabric locks are taken after it is dropped, which is the discipline
/// [`wake_parked`] states: the fabric lock is a leaf. Every caller of this must
/// therefore already have released any fabric lock of its own, and must not hold
/// `LINK.link` — the one order in this file is `ctx.fabric` → `LINK.link`.
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

/// Reconcile the owed clock with what is ACTUALLY outstanding, after any event
/// that proves the bridge is moving: an ack, a landing, a retirement.
///
/// THE CLOCK BELONGS TO THE OLDEST OUTSTANDING POST, NOT TO THE LAST EVENT, and
/// getting that wrong was a real hole. The first version cleared unconditionally
/// on any evidence, so: queue post 1 and post 2, the bridge lands 1 and wedges,
/// and the debt is cleared by 1's landing while 2 is owed forever. The instance
/// reads `connected`, and a `post --wait` already parked on 2 sleeps its whole
/// wait — exactly the freeze `stale` exists to cut short, reintroduced by the
/// fix for it.
///
/// So evidence CLEARS the debt only when nothing is outstanding, and otherwise
/// RE-STAMPS it: the bridge is alive, so the remaining work gets a fresh
/// [`STALE_AFTER`] rather than either a stale verdict it has not earned or an
/// exemption it has not earned either.
fn settle_work_owed(store: &Store) {
    // A SIBLING TEST'S RETIREMENT SETTLES NO SECTION'S DEBT (see
    // [`LINK_SECTION`]). `outbox sent` and `deliver landed=` reconcile against
    // their CALLER's store, and a test outside a live section has a store of
    // its own with nothing outstanding: its retirement cleared the debt the
    // section was measuring, and
    // `a_bridge_that_owes_an_answer_reads_stale_and_a_quiet_one_does_not` read
    // `connected` where it had made the debt `stale` (the gate, 2026-10-09).
    // The arm, `note_work_owed`, already took this guard; the settle had not.
    #[cfg(any(test, debug_assertions))]
    if link_section_is_foreign() {
        return;
    }
    let outstanding = posts_outstanding(store);
    let mut link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
    link.owed_since = outstanding.then(std::time::Instant::now);
}

/// Age the owed clock by `by`, so a test reaches [`stale_after`] without
/// sleeping for it. The alternative is a six-second unit test, and a test that
/// slow is one that gets marked `#[ignore]` and stops holding anything.
#[cfg(test)]
fn backdate_work_owed(by: std::time::Duration) {
    let mut link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
    link.owed_since = link.owed_since.map(|t| t.checked_sub(by).unwrap_or(t));
}

/// The `status` reply's `fabric=` token.
///
/// `stale` is DERIVED and the other four are stored, because `stale` is the
/// only one that becomes true with no event to store it on: a wedged helper
/// holds both its fds open and says nothing, so there is no report to write it
/// down at. See [`STALE_AFTER`].
pub fn fabric_state() -> &'static str {
    match LINK.state.load(Ordering::Relaxed) {
        FABRIC_CONNECTED if link_is_stale() => "stale",
        FABRIC_CONNECTED => "connected",
        FABRIC_DISCONNECTED => "disconnected",
        FABRIC_STALLED => "stalled",
        _ => "absent",
    }
}

/// [`fabric_state`] as an App's presence facts read it: the same token in
/// production, where one process is one instance and so one App.
///
/// A TEST BINARY BREAKS THAT ONE-TO-ONE: every test builds its own App, and
/// the link stays one `static`. A [`with_link_reset`] section holds an
/// attached or lost bridge (`stalled`, `disconnected`) for its whole body, and
/// either one is [`Link::Stalled`]/[`Link::Disconnected`] — `Level::Note`, a
/// committed row — to any App that refreshed during it. So while a section is
/// live on ANOTHER thread, an App reads `absent`: the state every section
/// resets to on entry and on exit. The section's own thread reads the link
/// itself.
///
/// The check and the read are ONE step: [`LINK_SECTION`]'s guard is held
/// across the load, so no section can take ownership — and so none can attach
/// — between "no foreign section" and the read (a check that released the
/// guard first left that gap open to a preempted reader). No lock nests the
/// other way: every writer checks and releases before it takes `LINK.link`,
/// and a section sets and clears its owner under acquisitions of their own.
/// What this cannot see is a test that moves the link WITHOUT a section; every
/// test that attaches or loses a bridge takes one ([`with_link_reset`]).
///
/// Measured 2026-09-28: an idle-frame presence test (since deleted) that
/// took no section read `presence_fp == 256` — a committed row — at
/// its fresh stub window's first idle frame once in three parallel `-p
/// aterm-gui --lib` runs;
/// `review_r1_focus_alone_never_reads_another_sessions_story` had read
/// `Note` for `Quiet` the same way on 2026-09-20 and was mended by taking a
/// section. This keys the read, so no App reader has to remember to.
///
/// [`Link::Stalled`]: aterm_messages::presence::Link::Stalled
/// [`Link::Disconnected`]: aterm_messages::presence::Link::Disconnected
pub fn fabric_state_seen() -> &'static str {
    #[cfg(any(test, debug_assertions))]
    {
        let section = LINK_SECTION.lock().unwrap_or_else(|p| p.into_inner());
        if section.is_some_and(|owner| owner != std::thread::current().id()) {
            return "absent";
        }
        // Read while the guard is still held: see the doc comment.
        fabric_state()
    }
    #[cfg(not(any(test, debug_assertions)))]
    fabric_state()
}

/// The link's three reported facts, as `status` and `fabric status` print them:
/// `(reason, rtt_ms, link_age_ms)` — `reason` is `-` while the link is up or no
/// bridge has attached, and the two numbers are `None` before the first ack.
///
/// The age is computed at READ time from the ack's own instant, so two readers a
/// second apart see it grow by a second whether or not the bridge said anything
/// in between — which is the whole point of carrying an age rather than a flag.
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

/// How long the link has been DOWN, in ms — the age of the stall itself,
/// which the presence facts' link carries (`Link::Stalled`). `None`
/// while the link is up, before a bridge has attached, and for an attached
/// bridge that has never reported (its dial has not been answered either
/// way: there is no stall to date). Computed at read time like the ack's age,
/// so two readers a second apart see it grow by a second.
pub fn fabric_stalled_ms() -> Option<u64> {
    let link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
    if link.up {
        return None;
    }
    link.stalled_at
        .map(|t| u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX))
}

/// The tail `status` appends after `fabric=`: `fabric_rtt_ms=<n|->
/// fabric_link_age_ms=<n|->`.
pub fn fabric_status_tail() -> String {
    let (_, rtt, age) = fabric_link_facts();
    let n = |v: Option<u64>| v.map_or_else(|| "-".to_string(), |n| n.to_string());
    format!("fabric_rtt_ms={} fabric_link_age_ms={}", n(rtt), n(age))
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
///
/// AN ATTACH IS `stalled`, NOT `connected`. A served lane proves a bridge
/// PROCESS exists; whether that process can reach a broker is the bridge's to
/// report, and it has not yet. Storing `connected` here was the measured lie
/// (a bridge pointed at a socket nothing served stayed `connected` for life),
/// so the link starts down with reason [`LINK_STARTING`] and the bridge's own
/// `link up` ([`link_report`]) moves it — which, for a broker that answers, is
/// milliseconds after this call — or, for a bridge that never reports, its
/// first delivery or landing does ([`link_evidence`]).
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
        // A NEW incarnation has no ack to its name. The old one's numbers are
        // history, and `fabric_rtt_ms=-` on a fresh attach is the truth.
        link.rtt_ms = None;
        link.acked_at = None;
        // And no stall to date: `starting` is a dial nobody has answered,
        // not a link that fell over ([`fabric_stalled_ms`]).
        link.stalled_at = None;
        link.reported = false;
        // A NEW incarnation inherits no debt: whatever the last one was asked
        // for and never moved, this one has not been asked yet — it drains the
        // same outbox on its first round. The clock comes back on the first
        // thing that proves it is moving, which re-stamps rather than clears
        // while posts remain ([`settle_work_owed`]); this function has no
        // `&Store` to reconcile against and does not need one, because an
        // attach is `stalled` and `stale` is only read from `connected`.
        link.owed_since = None;
        // And has said nothing about itself yet ([`cmd_hello`]).
        link.hello = None;
        link.mirror = None;
        link.lane_faults = 0;
    }
    LINK.state.store(FABRIC_STALLED, Ordering::Relaxed);
    // A new incarnation is a transition whatever the state was: the node log
    // says which bridge the link now speaks for (under `owner`, in order).
    link_moved(FABRIC_STALLED, generation.0, LinkSaid::Attached);
}

/// A BRIDGE THAT NEVER REPORTS ITS LINK IS READ BY WHAT IT DOES. Called from
/// the two bridge verbs that can only follow a broker exchange — `deliver`
/// (a record fetched from the bus) and `outbox sent` / `deliver landed=` (a
/// publish the broker acknowledged). While the attached incarnation has sent
/// no `link` record at all (`reported == false`: a bridge from before the
/// report, 0.85 and earlier), such a verb is the proof its link is up, and
/// `fabric=` moves to `connected` with no round trip (`fabric_rtt_ms=-`) and
/// the verb's arrival as the ack.
///
/// Measured 2026-09-14 (the review): the installed 0.85.0 bridge
/// under this endpoint read `stalled reason=starting` for life while its mail
/// flowed, every `post --wait` from its sessions failed at once with `ERR
/// fabric stalled`, and `aterm fabric on` could not arm it. Once an
/// incarnation HAS reported, its word stands and this does nothing: a bridge
/// that said `down` is down, whatever else it manages to do.
///
/// Under the generation lock like every other writer of `state`, and only for
/// the explicit incarnation that owns the link: an unscoped or ghost lane's
/// delivery proves nothing about the live bridge.
pub fn link_evidence(generation: Option<BridgeGeneration>) {
    // A SIBLING TEST'S `deliver` SPEAKS FOR NO BRIDGE (see [`LINK_SECTION`]).
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
    // `stalled` → `connected`, checked above: a transition.
    link_moved(FABRIC_CONNECTED, *owner, LinkSaid::Evidence);
}

/// THE BRIDGE'S REPORT OF ITS BROKER LINK — the `link` verb's handler, and
/// the writer of `fabric=connected` for every bridge that reports (the one
/// other writer, [`link_evidence`], speaks only for a bridge that never has).
///
/// Accepted only from an explicit incarnation that OWNS the link (`generation` is the
/// lane's, read from [`lane_generation`] by the dispatch), and never once that
/// incarnation's guard has fired: either fd closing means the link is gone
/// (§11.2), and a `link up` still in flight on the surviving lane must not
/// resurrect it. Both checks and the store happen under the generation lock,
/// for the reason [`bridge_lost`] gives.
///
/// A transition to DOWN wakes every parked `post --wait` on every session the
/// registry holds — the same sweep [`bridge_lost`] makes, for the same reason:
/// a waiter re-reads [`fabric_state`] only when it is signalled, and a link that
/// stalls under a parked waiter would otherwise cost it its whole timeout for a
/// landing the bridge already knows cannot be reported. The FIRST report of a
/// fresh attach counts as a transition too when it is `down`: a waiter parked
/// under `reason=starting` ([`wait_is_futile`]) has to hear that the dial
/// failed.
///
/// `true` when the report was applied; `false` for unscoped, stale or already
/// disconnected incarnations.
pub fn link_report(
    store: &impl AsStore,
    generation: Option<BridgeGeneration>,
    up: bool,
    reason: &str,
    rtt_ms: Option<u64>,
) -> bool {
    let store = &store.as_store();
    // BEFORE ANY LINK LOCK: reading what is outstanding takes the sessions'
    // fabric locks, and the one lock order in this file is `ctx.fabric` →
    // `LINK.link`. Only an `up` report settles the debt, so only an `up` pays
    // for the read.
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
            // AN ACK IS EVIDENCE THE BRIDGE IS MOVING, NOT THAT OUR WORK IS
            // DONE. It clears the debt only if nothing is outstanding, and
            // otherwise re-stamps it — see [`settle_work_owed`], and note that
            // `outstanding` was computed BEFORE this lock was taken, because
            // reading it needs the fabric locks and the one order here is
            // `ctx.fabric` → `LINK.link`.
            link.owed_since = outstanding.then(std::time::Instant::now);
        } else {
            link.reason = reason.to_string();
            if rtt_ms.is_some() {
                link.rtt_ms = rtt_ms;
            }
            // THE STALL IS DATED FROM ITS OWN TRANSITION: the moment the link
            // was last up (or the dial was first refused), never the last
            // ack — a quiet link acks nothing for an hour and is not an hour
            // stalled when it drops. A second `down` while already down (a
            // reason that changed) is the same stall and keeps its date.
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

/// A presence wake for every live session, and nothing else — the link's
/// recovery moves the band's fabric slot but answers no parked waiter.
fn wake_presence(store: &Store) {
    let live = store.read().unwrap_or_else(|p| p.into_inner()).sessions();
    for session in &live {
        session.fabric_changed();
    }
}

/// Whether the bridge that owns the link said, in its `hello`, that it offers
/// a kept session's unhandled broadcasts again after a relaunch (`refill=…say`).
/// Read at each broadcast's delivery ([`InboxRow::refill`]): a bridge from
/// before the key, one that has not said `hello` yet, and no bridge at all
/// answer `false`, which keeps the update counting the row.
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

/// The most bytes a `build=` token may carry: `<version>+g<12 hex>-dirty` with
/// room to spare.
const BUILD_TOKEN_MAX: usize = 64;

/// `hello lane=<n> [build=<token>] [refill=<lanes>]` — the bridge naming the
/// lane grammar it reads and writes, the build it is and what it refills, from
/// the lane whose generation `generation` is
/// (`aterm_types::control_verbs::BRIDGE_LANE`). Answered with this instance's
/// own, `OK lane=<n> build=<b>`, so each side speaks the older lane of the
/// two. `refill=in,say` says the bridge offers a kept session's unhandled
/// broadcasts again after a relaunch ([`bridge_refills_broadcasts`]); a bridge
/// that does not say so leaves a seamless update counting them lost.
///
/// RECORDED, AND COMPARED, NEVER REFUSED ON: the bridge binary is whatever
/// `[fabric] command` names — a dev build, the installed CLI an update
/// replaced — so a build other than this
/// instance's is a fact `fabric status` reports and the log says once, not a
/// reason to turn the bridge away. The lane is what decides what each side may
/// write, and the bridge acts on the number it is answered.
///
/// A bridge is a separate process and its output is still input: `lane=` is a
/// number and `build=` one bounded token of `[0-9A-Za-z.+-]`, or the line is
/// refused. Any other `key=value` is read past, so a later bridge can say more
/// to this endpoint. Accepted only for the incarnation that owns the link, like
/// `link`; a ghost lane's is answered `OK stale=1` and recorded nowhere.
pub fn cmd_hello(rest: &str, generation: Option<BridgeGeneration>) -> String {
    let mut lane: Option<u32> = None;
    let mut build: Option<String> = None;
    let mut refills_broadcasts = false;
    let mut log = HelloLog::Off;
    for tok in rest.split_whitespace() {
        if let Some(v) = kv(tok, "log") {
            // THE BUS MIRROR: `log=turns` asks this
            // endpoint to serve `log-take`/`log-landed`; `log=auto` asks for
            // nothing yet; any other word is read past, as any
            // other `key=value` is.
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
    // Only a recorded hello is told its mirror is served: a ghost
    // lane's `OK stale=1` above never says it.
    if log == HelloLog::Turns {
        format!("OK {answer} log=turns\n")
    } else {
        format!("OK {answer}\n")
    }
}

/// Whether `generation` is the bridge incarnation that owns the link: the
/// gate every bridge-only line that moves state passes (`link`, `hello`, and
/// the bus mirror's `log-take`/`log-landed`). A ghost lane's line
/// is answered `OK stale=1` and changes nothing.
#[must_use]
pub fn lane_owns(generation: Option<BridgeGeneration>) -> bool {
    let owner = LINK.generation.lock().unwrap_or_else(|p| p.into_inner());
    *owner != 0 && generation.is_some_and(|g| g.0 == *owner)
}

/// Run an owning bridge's operation while attach, log replacement and
/// registration cannot cross it. The transition precedes generation and
/// store/timeline/retired locks, as `register_on_log` does. `operation` must
/// not re-enter a transition-taking entry point.
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

/// Whether the bridge that owns the link asked for the bus mirror in its
/// recorded `hello` (`log=turns`).
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

/// WHETHER THE NODE MIRRORS, as the owning bridge last said it (the word the
/// store's `session_log_mirror::Shared::served` keeps): `Some(true)` for a
/// recorded `hello … log=turns`, `Some(false)` for one without `log=` and for
/// an `auto` one whose bridge then reported `link mirror unretained`, and
/// `None` for an `auto` hello not yet judged by an attach,
/// which leaves the store's word as it was: an `auto` bridge's
/// restart is the window the word is sticky across, as a `turns` one's is.
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

/// `fabric status`'s bus-mirror tail: ` bridge_log=<turns|auto>
/// mirror=<state>[:<reason>]` while the owning bridge's recorded `hello`
/// said `log=turns` or `log=auto`, and nothing otherwise, so a bridge with
/// `[fabric] log = "off"` leaves the line the bytes it was. An `auto` bridge
/// over a broker that retains nothing says `bridge_log=auto
/// mirror=unretained`; over one that retains,
/// its second `hello` makes it `turns`.
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

/// `link mirror on|stopped|capped|budget|unretained [reason=<token>]` from
/// the lane whose generation `generation` is: what `fabric status` says of
/// the bus mirror. `budget` and `unretained` are lane 6's (`BRIDGE_LANE`).
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

/// `link set-aside files=<n>` from the lane whose generation `generation` is
/// (LANE 4): after an unclean stop in an earlier boot, the bridge moved `n`
/// state files it found torn into its state dir's quarantine
/// (`aterm_link::state::StateDir::set_aside_torn`) rather than refuse them at
/// every attach. A fact for the owner's later look, handed to the registry
/// ([`Registry::torn_state_set_aside`]) — once per line, from the bridge that
/// owns the link; a ghost lane's is `OK stale=1` and hands nothing over — with
/// whether that bridge will say when its quarantine is empty: the lane its
/// recorded `hello` named, at
/// [`aterm_types::control_verbs::LINK_QUARANTINE_LANE`] or later (no `hello`,
/// none). The registry is called with no link lock held; the bridge
/// transition stays gated until the callback returns.
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

/// `link quarantine torn=<k>` from the lane whose generation `generation` is
/// (LANE 5): how many of the torn state files set aside
/// ([`cmd_link_set_aside`]) the bridge's quarantine still holds, said at its
/// start and as the owner empties it. Handed to the registry
/// ([`Registry::torn_state_held`]), which at 0 ends the fact it filed for the
/// owner's look — the owner has taken the bytes away — from the bridge that
/// owns the link; a ghost lane's is `OK stale=1` and hands nothing over. The
/// registry is called with no link lock held; the bridge transition stays
/// gated until the callback returns.
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

/// The endpoint answered one of `generation`'s bridge-only lines `ERR usage`:
/// count it for `fabric status`'s `lane_faults=` ([`LinkReport::lane_faults`]).
/// Only for the incarnation that owns the link.
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

/// What the attached bridge said about itself and how its lane has gone, as
/// `fabric status` prints it: `(bridge_lane, bridge_build, lane_faults)` —
/// the first two `None` before its `hello` (or for a bridge from before it).
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

/// HOW MANY `bye`s THE LIVE BRIDGE HAS TAKEN — its `link bye <word>`, the
/// answer to the `EVENT * bye <word>` this instance pushed before closing its
/// lanes (aterm-gui's `fabric_launch::bye`). A COUNT, not a latch, so a second
/// word — a handoff that rolled back, then a quit — waits for its own ack:
/// the sayer reads [`byes_taken`] before it speaks and waits past it
/// ([`await_bye_taken`]).
static BYES_TAKEN: Mutex<u64> = Mutex::new(0);

/// Signalled on every [`BYES_TAKEN`] increment.
static BYE_TAKEN: Condvar = Condvar::new();

/// The [`BYES_TAKEN`] count now.
#[must_use]
pub fn byes_taken() -> u64 {
    *BYES_TAKEN.lock().unwrap_or_else(|p| p.into_inner())
}

/// Wait at most `wait` for the bridge to take a `bye` past `before`; whether
/// it did.
#[must_use]
pub fn await_bye_taken(before: u64, wait: std::time::Duration) -> bool {
    let taken = BYES_TAKEN.lock().unwrap_or_else(|p| p.into_inner());
    let (taken, _) = BYE_TAKEN
        .wait_timeout_while(taken, wait, |n| *n <= before)
        .unwrap_or_else(|p| p.into_inner());
    *taken > before
}

/// Whether a bridge incarnation owns the link right now — attached and not
/// lost (`connected`, `stalled`, or `stale`, which is `connected` owing an
/// answer): one that can hear a `bye`.
#[must_use]
pub fn bridge_attached_now() -> bool {
    matches!(
        LINK.state.load(Ordering::Relaxed),
        FABRIC_CONNECTED | FABRIC_STALLED
    )
}

/// `link bye <word>` from the lane whose generation `generation` is: counted
/// only for the incarnation that owns the link, like every `link` report — a
/// ghost lane's ack speaks for no bridge that can still withdraw anything.
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

/// `link up rtt=<ms>` / `link down reason=<token> [rtt=<ms>]` / `link log
/// <identity>` / `link bye <quit|handoff>` / `link mirror …` / `link set-aside
/// files=<n>` / `link quarantine torn=<k>`, from the bridge lane whose
/// generation `generation` is. See [`link_report`], [`cmd_link_log`],
/// [`note_bye_taken`], [`cmd_link_mirror`], [`cmd_link_set_aside`] and
/// [`cmd_link_quarantine`].
pub fn cmd_link(store: &impl AsStore, rest: &str, generation: Option<BridgeGeneration>) -> String {
    cmd_link_after_log_switch(store, rest, generation, || {})
}

/// `cmd_link` with the endpoint's additional log-clock retirement, called
/// exactly on a completed `forgot=1` switch before releasing the transition.
/// The callback may take store/timeline locks, not re-enter this transition.
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
            // LANE 3's two tokens, both or neither, each once, in either order.
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

/// `link log <identity>` — WHICH BUS LOG the offsets the bridge hands this
/// endpoint index: astream's log identity, `<32 lowercase hex>.<generation>`,
/// told on every attach before any row is delivered. A CHANGE from the one
/// the owning bridge said before is a log switch — bus.log deleted and
/// recreated, repaired, restored, or another host's broker — and astream gives
/// another log's records the same numbers, so every live session forgets the
/// offsets it holds ([`Inbox::forget_bus_offsets`]) and the reply says
/// `forgot=1`. An empty session's first identity forgets nothing. A carried
/// identity must match; live references without one are an unknown old clock
/// and are retired conservatively. From a bridge that no
/// longer owns the link, `OK stale=1` and nothing changes.
///
/// `continues=<f> was=<identity>` (LANE 3, `continues`): the new identity is
/// a MOVED GENERATION of `was` — a power loss under a weak tier, a poisoned
/// log's reopen, a repair's cut — and astream recorded that the two name the
/// same records below `f` (the bridge's own clamp,
/// `aterm_link::state::LogWitness::continues`, already capped at the head).
/// A session whose clock is `was` forgets only what names an offset at or
/// above `f`, so a reply it queued while the broker was down, a receipt its
/// verdict owes and the dedup that keeps a refill from delivering a row twice
/// survive the move; a session on any other clock forgets everything, as
/// before. Still `forgot=1`: the clock moved, and the bus mirror's cursors
/// start over as the bridge's own pinned batch does. `was` must be an earlier
/// generation of the same log ([`is_earlier_generation`]): nothing else can
/// be continued, and a line that says otherwise is a usage error, not a
/// guess. The pair is kept with the identity ([`LogClock`]) for a session
/// registered later.
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
    // A new incarnation cannot deliver on a newer log while this incarnation
    // is still sweeping the old one. This lock is above both fabric and link
    // locks; the per-session wait below releases its fabric guard at the fence.
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
    // A bare identity the link already names keeps what was said of it: that
    // it continues an earlier generation stays true of the pair.
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
        // A partial busy sweep may already have applied this identity to some
        // sessions. Their own witnesses, not the global one, make retries
        // idempotent. A legacy carry with live references has an UNKNOWN old
        // clock; the first identity cannot simply claim those references.
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
    // Only a fully applied transition becomes the global witness. A busy
    // session keeps the old global witness and will be retried before delivery.
    *LINK.log.lock().unwrap_or_else(|p| p.into_inner()) = Some(clock);
    if changed {
        on_forgot();
        "OK forgot=1\n"
    } else {
        "OK\n"
    }
    .to_string()
}

/// astream's log identity as text: 32 lowercase hex digits, a dot, and a
/// generation in decimal — exactly that, so nothing else is taken for one.
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

/// Whether log identity `was` can be one `ident` CONTINUES (`link log <ident>
/// continues=<f> was=<was>`): the same log (the 32 hex digits alike) at a
/// lower generation — the only pair astream's `LogIdentity::continues` ever
/// answers for. Another log, the same generation or a later one never is.
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

/// A `link down reason=` token, kept to the shape every reader prints
/// verbatim: lowercase ASCII letters, digits and `-`, at most
/// [`LINK_REASON_MAX`] bytes, never empty.
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

/// Wake every parked `post --wait` and `await inbox` on every live session of
/// `store`, so each re-reads [`fabric_state`].
///
/// Ctxs are cloned out from under the registry guard first: the fabric lock is
/// a leaf and is never taken while the store is held. Each condvar is signalled
/// UNDER its session's own fabric lock, after the caller's state store. A `post
/// --wait` reads `fabric_state` while holding that lock and releases it only
/// inside `wait_timeout`, so mutual exclusion gives the whole property: either
/// this lock waits until the waiter is parked (and the signal reaches it), or it
/// wins the lock first — in which case the waiter's next read already sees the
/// new state. A `notify_all` outside the lock is the classic lost wakeup, and
/// losing this one costs the caller its entire `--wait`.
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

/// Record that the bridge has governed this session — the membership test
/// [`bridge_lost`] halts on.
///
/// THREE CALL SITES: `deliver` (both forms), the BRIDGE-issued `hold`, and
/// `outbox sent`. `outbox` is deliberately NOT one — a PEEK governs nothing,
/// and a bridge that only ever read a session's queue has not taken
/// responsibility for halting it. An OWNER-issued `hold` is deliberately not one
/// either: the local owner halting its own drivers says nothing about the
/// bridge, and recording it here would let a bridge's later death halt a
/// session that bridge never touched, on the strength of an act it never made.
///
/// PRUNES ITSELF. Past [`TOUCHED_PRUNE_AT`] entries the set is intersected with
/// the live registry, so sids whose sessions have exited stop being carried for
/// the life of the process. The registry snapshot is taken with the `touched`
/// lock RELEASED and re-acquired afterwards: `bridge_lost` reads `touched` and
/// then the store, so taking them in the other order here is the one shape that
/// could deadlock, and it is structurally avoided rather than argued about.
fn note_bridge_touched(store: &Store, sid: &str) {
    let key = registry_key(store);
    let over = {
        let mut touched = LINK.touched.lock().unwrap_or_else(|p| p.into_inner());
        let slot = touched.entry(key).or_insert_with(|| TouchedRegistry {
            registry: Arc::downgrade(store),
            sids: BTreeSet::new(),
        });
        // The slot's own `Weak` pins this address, so an existing slot under
        // this key IS this registry — never a freed one wearing its number.
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
    // Three separate fates, and the middle one is the whole point of the key: a
    // registry that is GONE takes its sids with it; the CALLING registry keeps
    // only what it still holds; every OTHER live registry is left exactly as it
    // was, because this snapshot is no evidence at all about its sessions.
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

/// THE BRIDGE IS GONE. Either bridge fd closed, so the halt this instance was
/// enforcing on the fleet's behalf can no longer be refreshed, lifted, or even
/// observed. Report `fabric=disconnected`, and — by default since the owner's
/// direction of 2026-09-29 ("stop adding blockers and limits") — leave every
/// session the bridge ever touched RUNNING, with an ADVISORY on each
/// (`status hold_advisory=fabric-lost`, a `hold-advisory` timeline record,
/// one Messages record for the loss): nothing is refused on it. A hold that
/// STANDS — a person's, through the fleet or locally — is untouched and stays
/// a real halt: a lost bridge lifts nothing. `[fabric] hold_on_bridge_lost =
/// true` ([`set_hold_on_bridge_lost`]) restores the fail-closed sweep below:
/// hold every session the bridge ever touched with `reason=fabric-lost
/// origin=fleet`. The paragraphs below describe that sweep.
///
/// Fail CLOSED. THE HOLD SWEEP IS UNCONDITIONAL: a halt that a `kill -9` lifts
/// is not a halt, so it does not ask whether a hold was standing, whether the
/// bridge exited cleanly, or whether anything is coming back. It fills the
/// FLEET slot ([`Holds`]) and only that: an owner's local hold stands beside
/// it, so a reconnecting bridge, which lifts what it wants lifted with `hold
/// off origin=fleet`, leaves the owner's stop where it was — one slot, and
/// the sweep's halt used to replace the owner's. NOTHING ELSE LIFTS IT —
/// see the module header; the "a human lifts it at the GUI" this doc used to end
/// with names a path that does not exist.
///
/// THE STATE IS GENERATION-CHECKED, and only the state. `generation` is the
/// incarnation this guard was created under; if a LATER incarnation has attached
/// since, this guard is a ghost of a bridge that is already gone and must not
/// report the live one disconnected. See [`BridgeGeneration`].
#[cfg(any(unix, test, debug_assertions))]
pub fn bridge_lost(store: &impl AsStore, generation: BridgeGeneration) -> usize {
    bridge_lost_waking(store, generation, &|_| {})
}

/// [`bridge_lost`], calling `wake` with each session's id whose session log
/// owes its readers a wake (the halt's `hold` mark), once its fabric lock is
/// released.
#[cfg(any(unix, test, debug_assertions))]
pub fn bridge_lost_waking(
    store: &impl AsStore,
    generation: BridgeGeneration,
    wake: &dyn Fn(&SessionId),
) -> usize {
    let store = &store.as_store();
    {
        // Compare and store under the same lock `bridge_attached` writes under:
        // a read-then-store pair would leave a window in which a newer attach
        // lands between the two and is clobbered anyway.
        let owner = LINK.generation.lock().unwrap_or_else(|p| p.into_inner());
        if *owner == generation.0 {
            if LINK.state.swap(FABRIC_DISCONNECTED, Ordering::Relaxed) != FABRIC_DISCONNECTED {
                link_moved(FABRIC_DISCONNECTED, generation.0, LinkSaid::Lost);
            }
            // The link goes with the bridge. The last rtt and its age are kept
            // — they are history, and `fabric_link_age_ms=` growing past the
            // loss is the honest shape — but nothing about them is `up`.
            let mut link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
            link.up = false;
            link.reason = LINK_BRIDGE_LOST.to_string();
            // Not a stall: the band's fabric slot reads `✕ lost` for a
            // disconnected bridge and prints no age ([`fabric_stalled_ms`]).
            link.stalled_at = None;
        }
    }
    // EVERY registry's sids, not just this store's. Each one is looked up in
    // `store` below and skipped if it is not there, so the answer is identical
    // to filtering by key first — and on a fail-closed sweep the version that
    // cannot be narrowed by a keying mistake is the one to run.
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
        // Clone the ctx Arc out under the store guard; never record a timeline
        // event while holding the registry.
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
                // No bound on a committing update's fence: this halt is the
                // one that must not be dropped, and the fence ends with the
                // Commit — in `_exit`, which ends this sweep too, or in the
                // attempt standing down, which lets it land.
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
    // WAKE EVERY REGISTERED SESSION, not only the governed ones. A session that
    // has only ever POSTED is not in `touched`, so the sweep above never touches
    // it — and its `post --wait` is parked on THIS session's condvar with no
    // other reason to wake. Without this it sits out its whole wait (up to
    // `WAIT_MAX_MS`) and answers `ERR timeout` for a landing nothing could ever
    // have reported, which is the exact outcome `cmd_post`'s entry check
    // (§11.2 deviation 9) exists to avoid. The waiter re-reads `fabric_state`
    // on each wake and answers `ERR fabric disconnected id=<n> queued=1` instead
    // — see [`fabric_wait_refusal`] for why the queued token is not decoration.
    // The same sweep, for the same reason, runs when the bridge reports its
    // link DOWN ([`link_report`]); the lock discipline is on [`wake_parked`].
    wake_parked(store);
    held
}

/// `[fabric] hold_on_bridge_lost`: whether a lost bridge halts the sessions it
/// governed ([`bridge_lost`]). Set once at startup from the config
/// (`fabric_launch::spawn_supervisor`).
#[cfg(any(unix, test, debug_assertions))]
pub fn set_hold_on_bridge_lost(on: bool) {
    LINK.hold_on_loss.store(on, Ordering::Relaxed);
}

/// Whether a lost bridge halts the sessions it governed
/// ([`set_hold_on_bridge_lost`]; `false` by default since 2026-09-29).
#[cfg(any(unix, test, debug_assertions))]
pub fn holds_on_bridge_lost() -> bool {
    LINK.hold_on_loss.load(Ordering::Relaxed)
}

/// Put the ADVISORY of a lost bridge on one session it governed
/// ([`bridge_lost`] by default): `lost` is the hold the fail-closed sweep
/// would have applied, `generation` the incarnation lost. Recorded on the
/// timeline (`hold-advisory 1 reason=… origin=…`) and pushed to presence;
/// NOTHING IS HELD. A session a hold already stands on keeps it and gets no
/// advisory — the standing halt says more. `true` when one was put.
#[cfg(any(unix, test, debug_assertions))]
fn advise(ctx: &dyn FabricSession, lost: Hold, generation: BridgeGeneration) -> bool {
    let mut inbox = ctx.fabric().lock();
    if inbox.holds.is_held() {
        return false;
    }
    let record = format!("1 reason={} origin={}", lost.reason, lost.origin);
    // The session log's `advisory origin= gen=` mark in the row's hold, the
    // reason at its content tier alone (6F-5). The record rings the
    // session's watchers, the log's readers among them, so the wake the hook
    // owes is the record's own.
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

// The hold gate

/// Whether a fleet halt refuses `verb` (§5.3): its verb row's
/// [`Input::PTY`](aterm_types::control_verbs::Input::PTY) flag. Every row that can
/// mutate declares its input class, and the argument for each member and each
/// exemption is written at its row (`aterm_types::control_verbs::VERBS`), so a new
/// writing verb is put inside the halt or argued outside it before it compiles,
/// whatever its target. The `hold` row's help enumerates the set
/// (`the_hold_help_enumerates_exactly_the_halt_set`, beside the table).
///
/// SEVEN MEMBERS ARE NOT `Target::Session` ROWS, and each has its own seam.
/// `askpass` is Owner-only `Meta`, intercepted by the serve loop before dispatch.
/// The other six — `confirm`, `hwkey`, `invoke`, `pane`, `pointer` and `tab` — are
/// `Target::App`: `control::dispatch_app` resolves no session for their bare forms,
/// so the session gate is structurally unreachable for them and
/// [`app_halt_refusal`] is where they are refused instead.
///
/// AN AIMED FORM ASKS BOTH. `spawn`, `tab` and `panes` also have an AIMED form
/// (`@<sid> tab …`, design S3) — the selector is the verb's ARGUMENT, "the window
/// hosting `<sid>`". Gating it on the AIMED session's hold alone is the wrong
/// question: `cmd_tab_aimed` drives `hosting_window(sid)`, so `tab close N`
/// retires whichever session tab N hosts. Under a PARTIAL halt — the ordinary
/// case, since [`bridge_lost`] halts only the sids in `LINK.touched` —
/// `@<unheld> tab close N` retired a HELD window-mate. `control::dispatch_app`
/// therefore asks an aimed App verb [`halt_refusal`] of the aimed session (so a
/// held one answers with its own `reason=`) and then, as it asks every App verb,
/// [`app_halt_refusal`].
///
/// TWO RESIDUALS, argued at their rows: `spawn` MINTS a session the halt does not
/// yet bind, and `update apply` re-execs the instance, which ends every session and
/// every in-memory hold. Neither is refused, so the halt's sentence is scoped to a
/// LIVE session rather than claiming that no verb can end a halted one.
#[must_use]
pub fn is_pty_reaching(verb: &str) -> bool {
    aterm_types::control_verbs::input_of(verb).contains(aterm_types::control_verbs::Input::PTY)
}

/// The refusal a halted session answers `verb` with, or `None` when the verb may
/// proceed ([`is_pty_reaching`], then [`hold_refusal`]): the halt asked of a verb
/// by name; a dispatch asks its request's class (`control::admit`).
#[must_use]
pub fn halt_refusal(ctx: &dyn FabricSession, verb: &str) -> Option<String> {
    if !is_pty_reaching(verb) {
        return None;
    }
    hold_refusal(ctx)
}

/// The refusal `ctx`'s standing hold answers a verb the halt binds, or `None` when
/// no hold stands. Transient class beside `ERR busy`, so a driver's existing back-off
/// already does the right thing.
///
/// SCOPE-BLIND on purpose: the halt binds Owner, Edge and Bridge alike. A halt
/// only the unprivileged obey is decoration.
///
/// A refusal under the PERSON'S hold is counted ([`SessionFabric::person_refusals`]):
/// the window says how many acts aimed at the tab its Take Over kept out. Only
/// this session-grain gate counts: an app verb [`app_halt_refusal`] refuses
/// instance-wide was aimed at no tab, so no tab's words claim it.
#[must_use]
pub fn hold_refusal(ctx: &dyn FabricSession) -> Option<String> {
    let (refusal, origin) = standing_refusal(ctx)?;
    if origin == PERSON_ORIGIN {
        ctx.fabric().person_refusals.fetch_add(1, Ordering::Relaxed);
    }
    Some(refusal)
}

/// [`hold_refusal`]'s words and the standing hold's origin, counting nothing.
/// A remote node's own hold does not refuse the drive connection that node
/// serves on ([`serving_driver`], [`Holds::first_against`]); every other
/// hold does, and its hold refuses everyone else.
fn standing_refusal(ctx: &dyn FabricSession) -> Option<(String, String)> {
    let driver = serving_driver().map(|driver| driver.node);
    let hold = ctx.fabric().holds().first_against(driver.as_deref())?;
    Some((
        format!("ERR halted reason={} origin={}\n", hold.reason, hold.origin),
        hold.origin,
    ))
}

/// [`halt_refusal`] for an APP-TARGET verb, which has no session to check.
///
/// `invoke <action>`, `hwkey <key>`, `pane <dir>`, `pointer <move|leave>`,
/// `tab <sub-form>` and `confirm yes|no` — the `Target::App` members of
/// [`is_pty_reaching`] — are answered by `control::dispatch_app`, which resolves
/// no session for them, and what they touch is whatever window is frontmost at
/// the time — which this thread cannot learn without a main-thread hop, and
/// which can change between the check and the action anyway. So the question asked here is the only one that is both
/// answerable and honest: is ANY session on this instance held?
///
/// FAIL CLOSED, and cheap in the case it exists for. A fleet halt is a fleet-wide
/// gesture — the node's bridge writes `hold <sid> on origin=fleet` to every
/// session it hosts — so when this matters the answer is the same either way.
/// Over-refusing costs an unheld front tab one clipboard paste and one tab or
/// pane switch it can still perform from the physical keyboard, which the halt
/// deliberately never touches. Under-refusing costs the whole property: a
/// standing halt that let `invoke SelectAll` + `copy` + `invoke Paste` put the
/// session's own screen text on its own PTY, that let `hwkey` type the keystroke
/// `send` had just been refused, or that let `tab close` retire the very session
/// `close` had just been refused on.
///
/// The AIMED forms (`@<sid> tab …`) drive the window HOSTING that session rather
/// than the session itself, so the aimed session's own hold is not the whole
/// question for them: `control::dispatch_app` asks this function after it; see
/// [`is_pty_reaching`]'s doc.
pub fn app_halt_refusal(store: &impl AsStore, verb: &str) -> Option<String> {
    if !is_pty_reaching(verb) {
        return None;
    }
    any_hold_refusal(store)
}

/// The refusal of the first session on this instance a hold stands on, or `None`:
/// the App lane's question ([`app_halt_refusal`]), asked once the verb is known to
/// be one the halt binds.
pub fn any_hold_refusal(store: &impl AsStore) -> Option<String> {
    let store = &store.as_store();
    // Clone the ctxs out from under the registry guard: the fabric lock is a leaf
    // and is never taken while the store is held (`cmd_deliver` and
    // [`bridge_lost`] keep the same discipline).
    let ctxs: Vec<_> = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.sessions()
    };
    ctxs.iter()
        .find_map(|ctx| standing_refusal(ctx.as_ref()))
        .map(|(refusal, _)| refusal)
}

/// `open` of a GATEWAY CARD under any hold: [`app_halt_refusal`]'s twin for
/// the palette (`open menu`), a tab's menu (`open tab-menu`), the Overview
/// (`open overview`, its old keyword `connections`) and the Autopilot menu
/// (`open autopilot-menu`) and a fact's menu (`open fact-menu <fact>`), the
/// cards `escalated_op` classes Owner-only as
/// gateways. Each card is a row of acts behind a key: a control client that
/// opened one while a session is held could press with `key` what `invoke`
/// refuses — Close Window over a held background tab, a palette row the
/// person's Take Over keeps out, the Autopilot menu's mode, timers and
/// Harness settings, a fact menu's new tab. So while any session is held, opening one answers
/// `ERR halted` as `invoke` does. Closing one (`open <target> close`) and
/// every other target stay answerable. A card already open when the hold
/// began is still the gesture mark's (`App::person_gesture_refused`): the
/// person's own acts on it refuse a control client's key.
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

// Small wire helpers

/// A principal ADDRESS: a bare principal ([`is_principal`]), or
/// `<owner>@<node>` for a session behind a node, both halves bare. Nothing a
/// sender types reaches this on the INBOUND side — the bridge renders `from=`
/// from the delivered subject — but the endpoint validates it anyway, so a
/// compromised bridge cannot smuggle a sentence into a `from=` field an agent
/// reads as identity. On the OUTBOUND side a sender DOES choose `to=`, and this
/// is the grammar it is held to. A `via=` hop is never this wider form: that is
/// [`via_ok`], which the bridge re-checks on the line this endpoint writes.
pub fn valid_principal(p: &str) -> bool {
    match p.split_once('@') {
        Some((owner, node)) => is_principal(owner) && is_principal(node),
        None => is_principal(p),
    }
}

/// The most bytes a `post key=` may carry.
pub const KEY_MAX: usize = 64;

/// A `post key=` token: one to [`KEY_MAX`] bytes of `[A-Za-z0-9._:-]` — the
/// charset of every id this protocol already prints (sids, `<epoch>:<producer>:
/// <seq>` feed ids, rfc3339 stamps), so a caller can mint one from anything it
/// already holds and it can never split a wire line or a state-dir line.
fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= KEY_MAX
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
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
/// character, and an escape is kept whole or not at all, so it never splits a
/// `%XX` either.
///
/// WELL-FORMED PCT: a `%` that starts no escape (`100%sure`) is the caller's
/// literal percent, kept as its own escape (`100%25sure`), so a strict decoder
/// reads every reason, and every `%` costs one escape. That bounds what the
/// session log's `hold` mark holds of it whole, in the record's 512 bytes: 128
/// bytes of which at most 42 escapes (the review of 6F-6; a flood of `%` was 126
/// of them, past what the record has room for). An escape the input ENDS inside
/// (`a%2`, `a%`) was cut short by whoever bounded it upstream, and goes, as a
/// carried hold row's parse has always dropped it.
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
        // Every byte is ASCII-graphic, so each is one char.
        out.extend(piece.iter().map(|&b| char::from(b)));
        i += step;
    }
    if out.is_empty() { "-".to_string() } else { out }
}

// hold

const HOLD_USAGE: &str = "ERR usage: hold <sid> on|off [reason=<pct>] [origin=fleet|local]\n";

/// The answer to a `hold` that names the PERSON'S slot from the wire
/// (`origin=person`): that slot is the window's own (Take Over / Hand Back),
/// set and lifted by nothing a socket can send. A usage error, because no
/// scope may name it: the grammar is the whole of the refusal.
pub const HOLD_PERSON_USAGE: &str = "ERR usage: origin=person is the window's own hold \
                                            (Agents \u{25b8} Take Over This Tab / Hand Back This \
                                            Tab); hold <sid> on|off [reason=<pct>] \
                                            [origin=fleet|local]\n";

/// The refusal an Owner-issued `hold` gets for a fleet-origin act — naming
/// `origin=fleet`, or an `off` that finds only a fleet hold standing (nothing
/// of the owner's to lift). It says whose hold it is, which tells a caller
/// nothing it could not learn from `sessions status hold_origin=`. Named so
/// the dispatch can audit-log exactly this refusal without matching prose,
/// and so the menu can tell it from the bare `ERR denied` of an edge token or
/// a selector.
pub const HOLD_DENIED: &str = "ERR denied: only the fleet sets or lifts a fleet hold\n";

/// The answer to a `hold` that arrived while a seamless update was committing
/// the session and waited its [`SessionFabric::fence_wait`] without the update
/// either landing or standing down. Nothing moved: `ERR busy` is the transient class
/// a driver already retries, and the retry reaches whichever process then
/// owns the session.
pub const HOLD_BUSY: &str = "ERR busy update-committing\n";

/// Count one block on a committing update's fence: on this thread (a test
/// reads [`fence_blocks_on_this_thread`] to see that a path never waited) and
/// on the session (a test awaits [`SessionFabric::fence_waits`] to know its
/// waiter is parked), without a clock — and the `bound` that waiter waits
/// (`None`: until the fence lifts), which a test reads
/// ([`SessionFabric::fence_bound`]) to see the production bound taken
/// without waiting it out.
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

/// THE NEGATIVE CONTROL OF A FENCED WAIT, WITHOUT A CLOCK: a waiter
/// another thread started on `fabric` under a raised fence is PARKED — it
/// blocked on the fence ([`SessionFabric::fence_waits`] moved past `before`,
/// awaited under the hang detector) — and, woken while the fence still stands,
/// blocks AGAIN rather than answering. `finished` reads the waiter's thread.
/// This crate's tests' own; aterm-gui's tests keep a copy beside their fixtures.
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

/// Who is issuing a `hold` — the whole of what the two owner-class scopes that
/// may run it differ in. The dispatch maps `Scope::Bridge` to [`Self::Bridge`]
/// and `Scope::Owner` to [`Self::Owner`]; an edge token never reaches the
/// handler — `dispatch_hold_verb` refuses it first, audit-logged.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HoldIssuer {
    /// The inherited bridge connection: the fleet's hand. `origin=fleet` by
    /// default and either origin accepted; each act sets or lifts the slot of
    /// the origin it names and no other, so the fleet's lift leaves the
    /// owner's local hold standing.
    Bridge,
    /// The instance Owner token over the local control socket: `origin=local`
    /// only, so its acts set and lift the owner's own slot and never the
    /// fleet's; an `off` that finds only a fleet hold standing is refused.
    Owner,
    /// The person at the window: Take Over and Hand Back (`App::take_over`,
    /// `App::hand_back`, through [`person_hold`]), on the main thread. Never
    /// mapped from a scope: no socket constructs it. It acts on the `person`
    /// slot and no other, and no other issuer acts on that slot
    /// ([`apply_hold`]'s slot discipline) — save that its Take Over takes a
    /// remote node's slot away and its Hand Back may lift it.
    Person,
    /// A remote node over the fleet's drive op: the lines a drive connection
    /// serves, as the node [`DrivingAs`] names on its thread. It acts on the
    /// `remote` slot alone, as that node, and names no origin.
    Remote,
}

/// One act on a session's holds ([`apply_hold`]).
#[derive(Clone, Debug, PartialEq, Eq)]
enum HoldAct<'a> {
    /// Set the slot of `hold.origin`, or move its reason.
    Set(Hold),
    /// Lift the slot of this origin, and only it.
    Lift(&'a str),
    /// Set the remote slot as its node ([`HoldIssuer::Remote`]).
    SetRemote(RemoteHold),
    /// Lift the remote slot: as this node ([`HoldIssuer::Remote`]), or
    /// whichever node holds it (`None`: the person's Hand Back).
    LiftRemote(Option<&'a str>),
    /// Lift every slot: the test door's lift ([`apply_hold_for_test`]), never
    /// a `hold` verb's.
    #[cfg(any(test, debug_assertions))]
    LiftAll,
}

impl HoldAct<'_> {
    /// The one origin this act moves, `None` for [`Self::LiftAll`].
    fn origin(&self) -> Option<&str> {
        match self {
            Self::Set(hold) => Some(hold.origin.as_str()),
            Self::Lift(origin) => Some(origin),
            Self::SetRemote(_) | Self::LiftRemote(_) => Some(REMOTE_ORIGIN),
            #[cfg(any(test, debug_assertions))]
            Self::LiftAll => None,
        }
    }
}

/// What [`apply_hold`] did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Applied {
    /// A slot moved, and the event was recorded. `held`: whether any hold
    /// stands after it — the `OK hold=` the act is answered with.
    Changed { held: bool },
    /// The slot already stood as asked; nothing recorded. `held` as above: a
    /// lift of an origin that was not standing answers `OK hold=1` while the
    /// other origin's hold stands, since the session is still halted.
    Unchanged { held: bool },
    /// An Owner-issued act named the fleet's slot, or an Owner `off` found
    /// only a fleet hold standing: refused without touching it. Never
    /// answered to [`HoldIssuer::Bridge`].
    FleetHeld,
    /// The act named a slot its issuer may not touch: the person's slot from
    /// a socket, or any other slot from the person. Nothing moved.
    NotItsSlot,
    /// A remote node's set while the person's hold stands or another node's
    /// does, or its lift of another node's: nothing moved. The standing
    /// hold is the refusal's ([`Holds::first`]).
    Taken,
    /// A seamless update was committing the session for the whole of the wait
    /// the caller allowed ([`SessionFabric::fence_hold`]); nothing moved.
    Fenced,
}

/// Apply one act to a session's holds and record the transition. Records the
/// timeline event WHILE holding the fabric guard — the sanctioned fabric →
/// timeline nesting — so a watcher's `EVENT hold` order can never invert
/// against the stored slots. Signals the condvar so a parked `await inbox …
/// kinds=hold` wakes at once.
///
/// ONE SLOT PER ORIGIN ([`Holds`]). A set fills the slot of the origin it
/// names, a lift empties it, and neither touches the other origin's: a fleet
/// halt landing on the owner's local hold stands beside it, and the fleet's
/// lift leaves the owner's hold where it was — the session still held. That
/// is what keeps the owner's stop through every lift the bridge writes — an
/// attach that finds no fleet halt writes `hold off origin=fleet` to every
/// session it hosts, so does a person's lift, and so does a reconnect after
/// the `[fabric] hold_on_bridge_lost` sweep — without the bridge having to
/// know, between its read and its write, which sessions the owner has held.
///
/// THE FLEET FENCE IS DECIDED UNDER THE SAME LOCK. An Owner-issued act may
/// name only the local slot, and its `off` is refused when only a fleet hold
/// stands ([`Applied::FleetHeld`]: there was nothing of the owner's to lift,
/// and the hold that stands is not the owner's). The standing slots are read
/// inside the guard the write takes, so a `hold on origin=fleet` from the
/// bridge that lands between an Owner's check and its write is the one that
/// rule sees.
///
/// AND SO IS THE COMMIT FENCE ([`SessionFabric::fence_hold`]): while a
/// seamless update is committing the session, the transition waits on the
/// session's condvar — for at most `fence_wait`, or until the fence lifts when
/// that is `None` — and lands only once the attempt has stood down. An update
/// that lands `_exit`s this process with the waiter still waiting, so no
/// transition is ever acknowledged that the successor did not adopt. Past the
/// wait it answers [`Applied::Fenced`] with nothing moved.
fn apply_hold(
    ctx: &dyn FabricSession,
    act: HoldAct<'_>,
    issuer: HoldIssuer,
    fence_wait: Option<std::time::Duration>,
    wake: &dyn Fn(),
) -> Applied {
    apply_hold_as(ctx, act, issuer, fence_wait, false, wake)
}

/// [`apply_hold`], its timeline row marked ` carried=1` where `carried`: a
/// halt this process did not set, put back from what a restore layout carried
/// ([`carry_hold`]).
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
    // THE PERSON'S SLOT IS THE PERSON'S: only the window's Take Over and
    // Hand Back name it, and they name nothing else (`cmd_hold` refuses a
    // socket's `origin=person` first; this keeps the rule where the slots
    // are) — but the remote slot, which their Hand Back may lift. THE
    // REMOTE SLOT IS ITS NODE'S: only a drive connection's own acts set or
    // lift it, and they act on nothing else.
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
    // THE PERSON WINS: no node takes a tab the person holds, nor one another
    // node holds, nor lifts that node's.
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
        // An `off` with nothing of the owner's standing, against the fleet's
        // halt: the hold it meant is the fleet's.
        if matches!(act, HoldAct::Lift(_)) && !inbox.holds.has("local") && inbox.holds.has("fleet")
        {
            return Applied::FleetHeld;
        }
    }
    let before = inbox.holds.clone();
    match &act {
        HoldAct::Set(hold) => {
            *inbox.holds.slot_mut(&hold.origin) = Some(hold.reason.clone());
            // The person's Take Over takes the tab from the Mac driving it.
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
    // A fresh Take Over counts its own refusals from zero.
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

/// **THE LINK SECTION'S OWN THREAD**, test-only — `None` while no
/// [`with_link_reset`] section is running.
///
/// [`with_link_reset`] serializes every test that MOVES the link, and that was
/// not enough: [`link_evidence`] is reached from `deliver` and `outbox sent`,
/// which sibling tests call OUTSIDE that mutex, and in a test there is no
/// serving lane — so `lane_generation` is `None`, which is exactly the shape
/// `link_evidence`'s own gate accepted ("a bridge from before the report")
/// until it came to require the explicit incarnation that owns the link. Such
/// a call found the SECTION's fresh attach (`stalled reason=starting`, a
/// non-zero owner) and flipped it to `connected` under the test that was
/// measuring it.
///
/// Measured on the 0.86 candidate, 2026-09-14: `-p aterm-gui` went red about
/// one run in three with the victim varying by schedule
/// (`a_ghost_lanes_link_report_is_dropped_and_a_lost_bridge_stays_lost`,
/// `a_parked_waiter_is_woken_by_a_down_report_from_starting_or_up`,
/// `review_an_old_bridge_that_delivers_mail_reads_connected`), green every
/// time under `--test-threads=1`; an `eprintln` in `link_evidence`'s store
/// named the flipping thread as a sibling test's each time. So the link's
/// state is writable only from INSIDE the section, and only by the thread
/// that owns it — the sibling's `deliver` still delivers, it just no longer
/// speaks for a bridge it did not attach.
#[cfg(any(test, debug_assertions))]
static LINK_SECTION: Mutex<Option<std::thread::ThreadId>> = Mutex::new(None);

/// True when a [`with_link_reset`] section is live on ANOTHER thread, so this
/// caller's write to the link would land inside someone else's measurement —
/// and an App's read of it ([`fabric_state_seen`]) would carry that
/// measurement into a window that is not part of it. Test-only; production
/// has one bridge and one instance.
#[cfg(any(test, debug_assertions))]
fn link_section_is_foreign() -> bool {
    LINK_SECTION
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_some_and(|owner| owner != std::thread::current().id())
}

/// What else a [`with_link_reset`] section resets beside the link: the
/// launcher's supervisor record, which aterm-gui keeps ([`also_reset_with_the_link`]).
#[cfg(any(test, debug_assertions))]
static LINK_RESET_ALSO: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// Have every [`with_link_reset`] section also run `reset` at its start and
/// its end, under the section's lock: aterm-gui's launcher record, the other
/// half of `supervised`. The first caller's `reset` stands.
#[cfg(any(test, debug_assertions))]
pub fn also_reset_with_the_link(reset: fn()) {
    let _ = LINK_RESET_ALSO.set(reset);
}

/// Run `f` with the process-wide bridge link RESET to "never launched", and reset
/// it again on the way out — on an unwind too, through the guard's `Drop`.
///
/// The link is per-INSTANCE state in a `static`, which is right for production
/// (one bridge, one instance) and a hazard for a parallel test binary: a test
/// that attaches or loses a bridge is visible to every other test that reads
/// `fabric_state`, `status`'s own record included. Every such test takes THIS,
/// so they serialize and each starts from `absent`.
#[cfg(any(test, debug_assertions))]
pub fn with_link_reset<T>(f: impl FnOnce() -> T) -> T {
    static LINK_TESTS: Mutex<()> = Mutex::new(());

    fn reset_now() {
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
        // The launcher's supervisor record is the other half of `supervised`:
        // aterm-gui's `fabric_launch::arm` sets both under its own lock, so a
        // section that arms one must hand the next section a record that says
        // nothing is ([`also_reset_with_the_link`]).
        if let Some(also) = LINK_RESET_ALSO.get() {
            also();
        }
    }

    /// ONE GUARD, so the order cannot be written wrongly.
    ///
    /// Rust runs a struct's own `Drop::drop` BEFORE dropping its fields, so the
    /// reset below happens while `_lock` — the field that serializes the section
    /// — is still held, and the mutex is released only afterwards. The two-binding
    /// form this replaced (`let guard = …; let _reset_on_exit = Reset; let out =
    /// f; drop(guard); out`) released the lock FIRST on the success path,
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
            // Reset FIRST, then released, both while the lock is still held,
            // so no window exists in which the state is live and unowned: the
            // other order let a sibling's read ([`fabric_state_seen`]) or write
            // pass the foreign check between the release and the reset and
            // meet this section's attached bridge.
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

/// Whether [`FabricLink::touched`] currently holds `sid`, under ANY registry.
/// Test-only.
///
/// Membership of a sid a test itself minted is schedule-proof in a way an exact
/// global count is not — 27 tests in `inbox_hold` call `deliver` outside
/// [`with_link_reset`]'s mutex, so a count raced them (2 of 3 full-suite
/// samples, 2026-09-01). Membership alone was NOT enough, and saying so is the
/// point of this paragraph: those siblings do not merely insert, they can also
/// trip the prune, and while `touched` was one flat set a sibling's prune
/// evicted the caller's live sids as readily as its own dead ones. That is
/// closed at the root by [`TouchedRegistry`], not here.
#[cfg(any(test, debug_assertions))]
pub fn touched_contains(sid: &str) -> bool {
    LINK.touched
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .any(|r| r.sids.contains(sid))
}

/// THE OWNER'S LOCAL HOLD A RESTORE LAYOUT CARRIED, put back on a session
/// that is REGISTERED already — a window's bootstrap shell a cold restore
/// grafts onto the leaf (`App::carry_restored_identity`;
/// `restore::TerminalLeafRestore::holds`, the owner's slot) — through the
/// hold's own path ([`apply_hold`]), as the owner's: it fills the owner's own
/// slot, and a standing FLEET hold stands beside it untouched ([`Holds`]), so
/// the fleet's lift leaves the owner's stop where it was; the move is
/// recorded `carried=1` and wakes the band (`FabricChanged`); and while a
/// seamless update is committing the session nothing moves. Never waits: the
/// caller is the event loop, which alone raises and lifts that fence (as
/// `App::take_over_session` does). `true` where the owner's hold stands on the
/// session after it; `false` where the commit's fence kept it off, nothing
/// moved. A session not yet registered takes [`SessionFabric::seed_holds`]
/// (the review of LC-8: seeded, a registered shell skipped the fence and the
/// band's wake).
pub fn carry_hold(ctx: &dyn FabricSession, hold: Hold) -> bool {
    carry_hold_waking(ctx, hold, &|| {})
}

/// [`carry_hold`], calling `wake` when the session log's readers are owed a
/// wake (its `hold` mark), once the fabric lock is released: the event
/// loop's.
pub fn carry_hold_waking(ctx: &dyn FabricSession, hold: Hold, wake: &dyn Fn()) -> bool {
    // Only the owner's slot is carried back (`Holds::of_origin("local")`);
    // naming the slot here keeps an Owner act off the fleet's.
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

/// [`apply_hold`] for a caller that already holds the ctx — the `status` test's
/// door into the same state the bridge writes, without a registry round trip.
/// Bridge-issued, so it is never refused: `Some` sets that hold's origin's
/// slot, `None` lifts every slot. `true` when the state moved.
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

/// `hold <sid> on|off [reason=<pct>] [origin=fleet|local]` — the drive halt,
/// local or fleet. `Access::OwnerOnly` gates the scope (Owner-class in, edge
/// out, selector rejected); this is the handler, and [`HoldIssuer`] is the one
/// thing it needs from the dispatch that the table cannot say.
///
/// A HOLD IS A SET OF ORIGINS ([`Holds`]): `on` sets the slot of the origin
/// the act names, `off` lifts that slot and no other, and the session is held
/// while either stands. The answer is `OK hold=<0|1>` for the session AFTER
/// the act — a lift that leaves the other origin's hold standing answers
/// `OK hold=1`, because the session is still halted.
///
/// THE ORIGIN RULE, ALL OF IT:
///
/// * A bridge-issued act is `origin=fleet` unless the line says otherwise. The
///   bridge is the fleet's hand, and the fleet's halt and lift are no act on
///   the owner's stop: every attach that finds no fleet halt writes `hold off
///   origin=fleet` to every session, and that used to free each one the owner
///   had held; a fleet halt landing on a local hold used to REPLACE it, so its
///   lift freed the session too.
/// * An Owner-issued act is `origin=local` — the default, and the only value
///   accepted: `origin=fleet` from Owner is [`HOLD_DENIED`] before any session
///   is looked up. It sets and lifts the owner's own slot, beside a fleet hold
///   as well as without one, and never touches the fleet's: lifting or
///   replacing a fleet halt is the "lift a fleet halt locally" a
///   prompt-injected agent holding Owner must never be able to do. An `off`
///   that finds nothing of the owner's and a fleet hold standing is
///   [`HOLD_DENIED`] ([`Applied::FleetHeld`], under the lock): the hold it
///   meant is the fleet's. `off` with nothing standing is `OK hold=0`, as for
///   the bridge — there is nothing to refuse.
///
/// An Owner-issued act does NOT mark the session bridge-governed
/// ([`note_bridge_touched`]): see that function for why.
///
/// A SEAMLESS UPDATE COMMITTING THE SESSION holds the answer back
/// ([`SessionFabric::fence_hold`]) for up to its [`SessionFabric::fence_wait`]: an attempt
/// that stands down lets the act land and answers it as usual; one that lands
/// takes this process with it, so the caller sees its connection close with
/// no answer and its retry reaches the successor — never an `OK` for a halt
/// the successor did not adopt. Past the wait it is [`HOLD_BUSY`].
pub fn cmd_hold(store: &impl AsStore, rest: &str, issuer: HoldIssuer) -> String {
    let store = &store.as_store();
    cmd_hold_within(store, rest, issuer, None)
}

/// [`cmd_hold`], calling `wake` with the session's id when the session log's
/// readers are owed a wake (its `hold` mark), once the fabric lock is
/// released: the control worker's.
pub fn cmd_hold_waking(
    store: &impl AsStore,
    rest: &str,
    issuer: HoldIssuer,
    wake: &dyn Fn(&SessionId),
) -> String {
    let store = &store.as_store();
    cmd_hold_within_waking(store, rest, issuer, None, wake)
}

/// [`cmd_hold`] with the wait for a committing update as an argument — `None`
/// is the session's own [`SessionFabric::fence_wait`] — so the menu can
/// answer at once and a test can reach the [`HOLD_BUSY`] answer without
/// sitting out the real bound.
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
        // The window's own acts go through [`person_hold`], never this
        // grammar: a line that claims to be the person's is refused whole.
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
            // A remote node names no origin: its slot is its own.
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
    // The session is now governed by the bridge whichever way this went: a `hold
    // off` is as much an act of governance as a `hold on`, and losing the bridge
    // right after one must fail closed too. The Owner's local act is NOT
    // governance by the bridge and records nothing here.
    if issuer == HoldIssuer::Bridge {
        note_bridge_touched(store, sid);
    }
    // A remote act is its connection's node's, which only a drive
    // connection names: anywhere else it is refused.
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
        // The session's state after the act: a lift that left the other
        // origin's hold standing is still a halt, which the answer says.
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
    /// The slot already stood as asked: nothing moved.
    Unchanged,
    /// A seamless update was committing the session: nothing moved, and the
    /// press is answered at once (the Commit's fence is this thread's own).
    Busy,
}

/// THE PERSON'S TAKE OVER (`on`) OR HAND BACK (`off`) of one session: the
/// `person` slot, reason [`TAKEN_OVER`], under the same fabric guard, the
/// same timeline record and the same wakes a wire `hold` takes. The only
/// constructor of [`HoldIssuer::Person`] outside this module's tests: the
/// window's `App::take_over` / `App::hand_back` call it on the main thread.
///
/// HAND BACK OF A CLIENT'S HOLD: where no person's hold stands, Hand Back
/// lifts an Owner client's (`local`) as that client's own `hold off` would
/// ([`Holds::hand_back_origin`]), issued as [`HoldIssuer::Owner`] so the
/// slot discipline stays [`apply_hold`]'s. Only the main thread sets the
/// person's slot, so the look and the lift cannot cross a Take Over.
///
/// NEVER A WAIT ON A COMMITTING UPDATE: the Commit's fence is raised and
/// lifted by the main thread, which is this caller, so a wait could end only
/// at its bound. The press is answered at once ([`PersonHoldOutcome::Busy`]).
///
/// `wake` is called when the session log's readers are owed a wake (its
/// `hold` mark), once the fabric lock is released: the event loop's.
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
        // `FleetHeld` answers only an Owner lift that found the fleet's
        // hold alone (a client lifted its own meanwhile), and `NotItsSlot`
        // and `Taken` cannot follow from either issuer naming its own slot:
        // nothing moved either way.
        Applied::Unchanged { .. } | Applied::FleetHeld | Applied::NotItsSlot | Applied::Taken => {
            PersonHoldOutcome::Unchanged
        }
    }
}

// deliver

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
pub fn cmd_deliver(store: &impl AsStore, rest: &str) -> String {
    cmd_deliver_waking(store, rest, &|_| {})
}

/// [`cmd_deliver`], calling `wake` with the session's id when the session
/// log's readers are owed a wake (a `mail` or `mail-landed` mark), once the
/// fabric lock is released: the control worker's (see [`FabricFact`]).
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
        // THE OTHER RETIREMENT PATH. `deliver landed=` retires a post exactly as
        // `outbox sent` does, so the owed clock is reconciled here for the same
        // reason and in the same order — after the retirement, so the post that
        // just landed is off the list before the list is read.
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

/// `deliver <sid> receipt=<rid> off=<n|->` — the bridge retiring a receipt this
/// session OWED (R8, [`OwedReceipt`]): `off=<n>` is where the `ack` landed on
/// the sender's lane, `off=-` that the bridge decided there was none to send (it
/// serves no `--receipts`, the sender has no lane, the kind does not wait).
/// Either way the receipt leaves the `outbox` peek.
///
/// IDEMPOTENT, and a rid already retired answers `OK` as well: the bridge
/// forgets the producer sequence it pinned to the receipt only on an `OK`, so a
/// retirement retried after a lost reply must not strand that pin.
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
    // Held back by a committing update like a post's retirement (see
    // [`retire_post`]): the successor carries the receipt as owed.
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

/// `deliver <sid> fetched=<off> …` — the bridge answering an `inbox get @<off>`
/// this session parked (E3, "nothing lost"): the record as the bridge read it
/// off the broker's log, classified by the same code that classified it at
/// delivery, or `err=<token>` for why there is none. It TOUCHES NO RING — no
/// row, no id, no watermark, no quota, no event: the parked read takes the
/// answer and that is the whole effect. Idempotent: a complete answer for an
/// offset nothing waits on is kept as an answered slot (bounded), which is
/// exactly what a read that timed out a moment ago will ask for next.
///
/// IN CHUNKS WHEN THE BODY DOES NOT FIT ONE LINE. A body the delivery had to cut
/// (`truncated=1`) is the case this verb exists for, and the control line that
/// cut it is the same line this answer travels on — one line could only ever
/// carry it cut again. So the bridge sends consecutive lines, each a whole
/// `fetched=` line with the same fields: `at=<n>` is the byte offset in the
/// DECODED body where its `text=` starts (0 when absent) and `more=1` marks
/// every line but the last. A chunk extends the partial answer at exactly its
/// end; `at=0` starts afresh, which is also what a bridge relaunched mid-answer
/// does. `len=` is the record's true body length and may exceed [`BODY_MAX`] —
/// a record over it arrives cut at [`BODY_MAX`] and reads `truncated=1 len=` —
/// while the body carried is bounded by [`BODY_MAX`] like every other.
///
/// A REFUSED ANSWER ENDS THE READ WAITING ON IT (`ERR fetch failed
/// reason=malformed`) rather than leaving it parked: a pending slot is listed on
/// every `outbox` peek, so a refusal the bridge could only log used to be
/// fetched again, and refused again, on every drain until the read timed out.
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
            // `err=retained`'s floor: what the broker keeps of this session's
            // lane starts there.
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
    // A partial chunk cannot answer a parked `inbox get`: its wait loop treats
    // `Partial` exactly like `Pending`. Keep assembling it, but leave the
    // waiter and the GUI asleep until the final chunk or a failure arrives.
    let answer_ready = !matches!(slot, FetchSlot::Partial(_));
    let mut inbox = ctx.fabric().lock();
    if matches!(slot, FetchSlot::Done(_)) {
        inbox.oldest_on_bus = Some(inbox.oldest_on_bus.map_or(off, |o| o.min(off)));
    }
    match inbox.fetches.iter().position(|(o, _)| *o == off) {
        Some(i) => inbox.fetches[i].1 = slot,
        // NOTHING WAITS ON THIS OFFSET. A complete answer is kept (bounded) for
        // the read that asks next; a chunk or a failure is not — a partial
        // answer nobody asked for would be listed for the bridge to fetch
        // again, and a failure is only ever consumed by the read it answers.
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

/// Refuse one `fetched=` line, and FAIL the read parked on its offset if one
/// is: the reply is the refusal the bridge logs, and the reader is told now —
/// `ERR fetch failed reason=malformed` — instead of at its timeout.
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

/// Drop the oldest ANSWERED fetch slot once the table is full. A pending one
/// is never dropped: a parked read must find its answer or its timeout.
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

/// The parsed fields of a `deliver` row line — shared by the ring form and the
/// `fetched=` form so the two cannot admit different grammars.
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
    /// Body length in bytes before any bridge-side cut (`len=`), else the
    /// body's own.
    len: usize,
    /// The `len=` token itself, when the line carried one.
    declared_len: Option<usize>,
    /// `topic=<t>` — the broadcast face this record came in on.
    topic: Option<String>,
    body: String,
}

/// Parse the `k=v` tokens of a `deliver` row: `Err` is the reply to give.
/// `len_max` bounds `len=`: [`BODY_MAX`] for a row the ring will hold, and
/// nothing for a `fetched=` answer, whose `len=` is the size of a record on
/// the log and whose carried body is bounded where it is assembled.
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
    // `trust=` is the RECEIVER's verdict, computed by the bridge from (face,
    // sender class, relay), and the endpoint refuses a label it does not know
    // rather than passing an uninterpreted token to an agent as if it meant
    // something. The default is the conservative one, not the trusting one.
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

/// `deliver <sid> landed=<post-id> off=<n>` — the §11.2 spelling of a post's
/// retirement. ONE implementation with `outbox sent`, which is the same act with
/// one more case (`off=-`, permanently undeliverable): two names for one
/// transition would be two chances to disagree about the watermark.
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
    ctx: &dyn FabricSession,
    post_id: u64,
    off: Option<u64>,
    reason: &str,
    dup: bool,
    wake: &dyn Fn(),
) -> Result<(), String> {
    // A RETIREMENT A COMMITTING UPDATE WOULD NOT CARRY WAITS: answered here
    // it would let the bridge forget the sequence it pinned while the
    // successor carries the post as still queued — and publishes it again at
    // a FRESH sequence. Unanswered, the pin stays, and the successor's bridge
    // publishes the carried copy at it (the broker's dedup) — see
    // [`FabricCarry`]. Only for a successor that reads the carry
    // ([`Inbox::retirements_fenced`]): an older one restarts the post ids
    // under the same sid, and a pin left standing would swallow its post.
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
            // STICKY, like the offset: a retirement retried without the flag
            // (a bridge that died between `outbox sent` and its reply) must
            // not turn a duplicate back into an original.
            row.dup |= dup;
        }
        None => {
            row.dead = true;
            row.dead_reason = reason.to_string();
        }
    }
    // EVERYTHING THE ROW WAS KEEPING FOR THE BRIDGE GOES WITH THE BODY. `via` is
    // caller-chosen and rides the same `post` line; it survived retirement while
    // both of the docs that describe a retired row — [`RETIRED_KEEP`] and
    // [`Inbox::trim_retired_posts`] — said in as many words that such a row
    // carries "only (id, to, kind, off)". `RETIRED_KEEP` of them are held for the
    // life of the session, so the one field the bridge no longer needs is the one
    // field that stayed. The row's remaining use is resolving an old answer's
    // `re-id=` from (id, off), and neither the `outbox` drain (queued rows only)
    // nor the `inbox` post rows read `via`.
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

// outbox / outbox sent

const OUTBOX_USAGE: &str = "ERR usage: outbox [<max>] [earlier-receipts=1] | outbox sent <sid> <post-id> off=<n|-> \
                            [dup=1] [reason=<word>]\n";

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

/// `outbox [<max>] [earlier-receipts=1]` — drain every session's queued outbound posts WITH their
/// bodies, as one length-prefixed frame. BRIDGE-ONLY (the dispatch gates it).
///
/// A PEEK, on purpose. It moves no watermark and removes nothing: a bridge that
/// dies between reading a post and getting its `PublishAck` re-reads the same
/// post on restart and republishes it under the same producer sequence, which the
/// broker's `(producer_id, producer_seq)` dedup then collapses. Retirement is a
/// separate, explicit act (`outbox sent`), which is what makes the pair the
/// outbound mirror of `deliver` / `inbox seen`.
///
/// FRAMING. `OK <nbytes>` then that many bytes, holding a `post … len=<n>` line
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
/// still queued in the same order, and the bridge calls `outbox` again. A walk
/// the bound stopped with a post still unlisted ends the frame with a bare
/// `more` line, so the bridge does not take the listing for the whole queue: it
/// keeps the `post` events queued for what it was not shown, each of which runs
/// its next drain at once, and with none queued its two-second roster backstop
/// does. The bound is enforced HERE, where the reply is built, rather than by
/// asking every caller to remember a number — the two sides of a bound must meet
/// in one place.
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
    // EACH SESSION'S POSTS IN QUEUE ORDER, the sessions TAKEN IN TURN: one post
    // from every session, then the next one from each, until
    // the budget binds. Per sender, the drain a bridge sees is the order the
    // endpoint accepted the posts in, which is the order the bus must see them in
    // for a per-sender stream to mean anything; ACROSS senders nothing is
    // promised, and taking them in turn is what keeps one session from spending
    // every drain. The bridge holds a sender whose post it could not publish
    // and moves on to the others (`Bridge::drain_outbox`), and a session ahead
    // in `local_id` order whose queue alone filled the budget — a full 4 MiB of
    // posts the broker cannot take now — used to be the whole of every drain,
    // so no later session's post was ever listed. Now each session's head post
    // is listed before any session's second, as far as the budget reaches.
    // Across drains, the least recently listed sessions go first (`local_id`
    // breaks ties). Otherwise sixteen refused maximum-sized heads fill every
    // drain and a seventeenth sender is never even offered to the bridge.
    //
    // A session is read under its own lock once per turn, so between turns a
    // post may be retired or queued: each turn resumes at the first queued post
    // AFTER the id last listed for that session, never at a position, so the
    // per-sender order holds whatever moved in between.
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
    // Stable, so sessions on one turn keep the registry's local-id order.
    turns.sort_by_key(|(turn, _)| *turn);
    let handles: Vec<_> = turns.into_iter().map(|(_, h)| h).collect();
    let mut payload = String::new();
    let mut sent = 0usize;
    let mut listed: Vec<Option<u64>> = vec![None; handles.len()];
    let mut exhausted = vec![false; handles.len()];
    // Whether the walk stopped at `<max>` or the byte budget rather than at the
    // end of every queue.
    let mut stopped = false;
    'turns: loop {
        let mut took = false;
        for (i, h) in handles.iter().enumerate() {
            if exhausted[i] {
                continue;
            }
            // AFTER the first post, never before it: a single body may legally be
            // as large as `BODY_MAX`, and a budget that could refuse the post at
            // the head of the queue would strand it forever behind a bound it can
            // never fit under.
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
    // MORE: a walk the bound stopped with a post still unlisted says so, as the
    // frame's last line. A drain event is the bridge's cue to drain, and the
    // bridge forgets the ones queued before a peek that showed it everything
    // (aterm-link's `Mailbox::forget_queued_drains`): told nothing, it took a
    // budget-cut peek for the whole queue and left the rest for its two-second
    // roster backstop. After the walk, under each session's lock again, so a
    // post queued since counts too: it is unlisted either way.
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
    // THE OWED RECEIPTS AND THE PARKED READS, AFTER EVERY POST: one bodiless
    // `receipt sid=<sid> rid=<n> off=<n> verdict=<v> kind=<k> from=<p>
    // [log=earlier]` line per receipt this session owes (R8; the bridge
    // publishes the `ack` and retires it with `deliver <sid> receipt=<rid>`;
    // `log=earlier` is a row of a log before the one it reads now,
    // [`OwedReceipt::earlier_log`], which it retires unsent). These old-log
    // receipts are listed only for `earlier-receipts=1`, an explicit promise
    // that the requester understands their mark; older bridges ignore unknown
    // receipt columns. Also one `fetch sid=<sid>
    // off=<n>` line per `inbox get @<off>` waiting on the bridge. After, so a
    // bridge from before this rung — whose parser stops at the first line
    // without a `len=` — still drains every post. Not counted against `<max>`
    // or the byte budget: each is one short line, and both are bounded per
    // session ([`RECEIPTS_OWED_MAX`], [`FETCH_SLOTS`]).
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
    // LAST, so a bridge from before it — whose parser stops at the first line
    // without a `len=` — has read every post, receipt and fetch by then.
    if more {
        payload.push_str("more\n");
    }
    format!("OK {}\n{payload}", payload.len())
}

/// `outbox sent <sid> <post-id> off=<n|->` — retire one queued post.
/// BRIDGE-ONLY (the dispatch gates it).
pub fn cmd_outbox_sent(store: &impl AsStore, rest: &str) -> String {
    cmd_outbox_sent_waking(store, rest, &|_| {})
}

/// [`cmd_outbox_sent`], calling `wake` with the session's id when the
/// session log's readers are owed a wake (a `mail-landed` mark), once the
/// fabric lock is released: the control worker's (see [`FabricFact`]).
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
    // A duplicate LANDED, by definition: `dup=1` names the original record's
    // offset, and a dead post has none to name.
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
    // A RETIREMENT SETTLES THE DEBT WHATEVER THE BRIDGE'S VINTAGE IS, and it
    // settles it against WHAT IS LEFT. `link_evidence` above is the legacy arm —
    // it returns at once for any bridge that reports its own link — but `outbox
    // sent` is the endpoint watching the exact work it handed over come back
    // finished, which is stronger evidence than an ack on some other lane.
    //
    // AFTER `retire_post`, NEVER BEFORE, and that ordering is the fix: this post
    // has to be off the outstanding list before the list is read, or the
    // reconcile re-stamps a debt for work that has just been settled. It also
    // has to be after because `retire_post` holds the session's fabric lock and
    // [`posts_outstanding`] takes every session's in turn.
    settle_work_owed(store);
    match retired {
        Ok(()) => "OK\n".to_string(),
        Err(e) => e,
    }
}

/// The ordinary `deliver`: one message row.
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

    // A ROW IN THE UPDATE'S OVERLAP IS NUMBERED BELOW THE SUCCESSOR'S IDS:
    // the manifest carried the last
    // id plus [`INBOX_ID_RESERVE`], so a delivery between the park's draw and
    // the `_exit` lands here as ever. An ADDRESSED row reaches the successor
    // again — its bridge refills the session's addressed lane from the floor
    // `inbox seen` reported ([`addressed_floor`]) — under an id of its own,
    // and so does an unhandled BROADCAST from a bridge that refills them (its
    // topic's refill floor, see [`INBOX_ID_RESERVE`]). While the Commit's
    // fence stands, a row that would pass that ceiling, and every broadcast,
    // waits for the attempt to land or stand down
    // ([`SessionFabric::wait_fenced_delivery`]); neither is ever answered an
    // `ERR` the bridge would book as a verdict (the review's item 1). Before
    // the fence, passing the ceiling stands the Commit down
    // ([`SessionFabric::past_handoff_ceiling`]).
    //
    // Whether the bridge refills a broadcast is read BEFORE the inbox guard:
    // it takes the link's lock, and nothing here needs the two held together.
    let refill = topic.is_some() && bridge_refills_broadcasts();
    let mut inbox = ctx.fabric().wait_fenced_delivery(
        ctx.fabric().lock(),
        ctx.self_id().as_str(),
        topic.as_deref(),
    );
    // IDEMPOTENCY, before anything else: the offset decides, and a duplicate
    // answers the id it first got without touching the ring, the quota or the
    // events digest. A row the RING still holds is a duplicate too, after the
    // dedup window ([`DEDUP_CAP`]) has forgotten its offset: a bridge that
    // starts re-offers every broadcast its session had not handled
    // ([`InboxRow::refill`]), however long ago the ring took it.
    if inbox.dedup_set.contains(&off) || inbox.row_at(off).is_some() {
        let existing = inbox.row_at(off).map(|r| r.id);
        return match existing {
            Some(id) => format!("OK {id}\n"),
            // Delivered before, then evicted or pruned: the row is gone but the
            // offset is not deliverable twice. `OK 0` names no row on purpose —
            // a bridge that re-reads it learns the endpoint kept nothing.
            None => "OK 0\n".to_string(),
        };
    }
    // CONSENT IS CHECKED WHERE IT IS HELD. Receiver-side opt-in is the whole
    // safety story of broadcast, and this endpoint holds the set; the bridge's
    // copy of it is a routing hint that lags a `topic drop` — the rest of a
    // catch-up page already in flight, a session past the watch cap read once
    // per roster round, a `topic ls` that failed. Read under the inbox guard
    // (inbox → topics, the order `topic add|drop` take under the same guard), so
    // once `drop` answers OK no row on that topic is appended here. After the
    // dedup, so the offset still decides a redelivery; before the quota, so a
    // declined topic is not counted in `dropped=` — the session said no, it lost
    // nothing. The bridge books the refusal as accounted and moves its cursor on.
    if let Some(t) = topic.as_deref()
        && !ctx.fabric().opted_into(t)
    {
        return "ERR not-subscribed\n".to_string();
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
        // A BROADCAST HAS NOBODY TO TELL BUT THE RECIPIENT — see
        // [`Inbox::dropped`]. The addressed lane's refusal rides back to the
        // sender on their own lane; this one is counted here or it is nowhere.
        //
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
    // Threaded by offset only to a post whose landing THIS log still holds
    // ([`PostRow::landed_on_log`]): an earlier log's post at the same number is
    // another record, a moved generation's below its fork the same one.
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

// topic — the receiver-side broadcast opt-in

/// `topic add <t> [since=head|@<off>] | topic drop <t> | topic ls` — the
/// RECEIVER-SIDE OPT-IN for broadcast.
///
/// A record on `say/<topic>` reaches this session only because this session
/// asked for the topic. That is the whole safety story: a fleet-wide shout
/// cannot put a word in front of an agent that did not want it, and an empty
/// set — the default — receives nothing at all.
///
/// `since=` is the LATE SUBSCRIBER's choice and is answered by the bridge, not
/// here: `head` (the default) takes only records published after the add, and
/// `@<off>` replays the topic from that broker offset so a session joining a
/// conversation can read what it missed. It is echoed on the `OK` line so the
/// caller can see which it got.
///
/// Owner scope: adding a topic changes what reaches an agent's inbox, which is
/// the same authority class as the halt, not a read.
///
/// A change records its `topic` row on the session's timeline, which wakes
/// the session's `events` watchers.
pub fn cmd_topic(ctx: &dyn FabricSession, rest: &str) -> String {
    let mut words = rest.split_whitespace();
    match words.next() {
        Some("ls") | None => {
            if words.next().is_some() {
                return TOPIC_USAGE.to_string();
            }
            let topics = ctx.fabric().topics();
            // `seen=` is the handled watermark ([`cmd_inbox_seen`]'s
            // event carries it too): a bridge reads it here, on every read of
            // the set, as the backstop for an `inbox-seen` it never heard, so
            // a broadcast the agent handled is not offered again.
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
                // ONE GRAMMAR FOR THE TOKEN, shared with the handoff carry's
                // validator, so the two cannot disagree about what `@+5` is.
                Some(tok) => match tok.strip_prefix("since=") {
                    Some(v) if valid_since(v) => v.to_string(),
                    _ => return TOPIC_USAGE.to_string(),
                },
            };
            if words.next().is_some() {
                return TOPIC_USAGE.to_string();
            }
            // A COMMITTING UPDATE HOLDS THE CHANGE BACK:
            // the successor carries the set the
            // park drew, and the Commit compares it again under this fence
            // (`App::carried_claims_held`), so a change that took the inbox
            // guard first stands that Commit down, and one after it waits.
            let Some(unfenced) = ctx
                .fabric()
                .wait_unfenced(ctx.fabric().lock(), ctx.fabric().fence_wait())
            else {
                return HOLD_BUSY.to_string();
            };
            // THE BRIDGE HEARS IT NOW, on the event lane it already holds:
            // nothing on the bus announces the change — the set is this
            // endpoint's, and only its bridge needs it.
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
            // Held back by a committing update, as `add` is.
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
            // THE BRIDGE FORGETS THIS TOPIC'S REFILL FLOOR WITH THE DROP
            // (aterm-link's `Bridge::forget_unhandled_topic`), so no successor
            // is offered the rows the ring still holds on it — not even after
            // a `topic add` of the same name, which resumes from a new cursor.
            // Each of them is therefore one no bridge refills
            // ([`InboxRow::refill`]): an update counts it lost in `dropped=`
            // rather than as carried. Under the guard the drop was made under,
            // so no draw reads the set without the rows.
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

/// The topic set as the HANDOFF MANIFEST carries it: one `"<topic> <since>"`
/// row per opt-in, in topic order.
#[cfg(any(unix, test, debug_assertions))]
pub fn render_topics(topics: &[(String, TopicEntry)]) -> Vec<String> {
    topics
        .iter()
        .map(|(t, e)| format!("{t} {}", e.since))
        .collect()
}

/// The inverse of [`render_topics`], VALIDATING every row.
///
/// The manifest is a file on disk that an older build, a rollback, or anything
/// that can write the state directory may have produced, and a topic set is
/// what decides which broadcasts reach an agent. A row whose topic is not
/// `[a-z0-9][a-z0-9._-]{0,31}` or whose `since` is not `head`/`@<n>` is
/// DROPPED, not repaired: the carry re-seeds consent, so a malformed row must
/// not be able to invent one.
pub fn parse_topics(rows: &[String]) -> Vec<(String, TopicEntry)> {
    rows.iter()
        .filter_map(|row| {
            let mut words = row.split(' ');
            let t = words.next()?;
            let since = words.next()?;
            // A 0.90.x manifest carried a third field (the add serial that
            // release keyed on); it is read past and dropped.
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

/// The reason a keeper META gives a carried hold whose own reason it had no
/// room for ([`KeeperCarry::rungs`]): the halt crossed, the words did not.
#[cfg(unix)]
pub const REASON_NOT_CARRIED: &str = "not-carried";

/// Every standing hold as the carries write them ([`render_hold`] rows), in
/// [`Holds::standing`]'s order: the person's first, then the fleet's. The
/// seamless handoff carries the person's Take Over as it carries the other
/// two, and so does a crash the PTY keeper recovers ([`KeeperCarry`]): a
/// crash or a Force Quit is not a Hand Back. A cold restore puts back only
/// the owner's local hold ([`carry_hold`]), so a deliberate quit and
/// relaunch hands the tab back. A handoff record's `hold` and a keeper
/// META's `hold` take the first row, `more_holds` the rest: a reader from
/// before the slots reads the stronger halt where it always read the one,
/// and reads past the others. The durable layouts carry them all, on the
/// leaf of a held tab (`restore::TerminalLeafRestore::holds`, LC-8), on
/// every platform.
///
/// A remote node's hold is never carried: it is the driving Mac's to take
/// again, and a row a successor read as a local hold would halt the tab for
/// no one.
pub fn render_holds(holds: &Holds) -> Vec<String> {
    holds
        .standing()
        .filter(|hold| hold.origin != REMOTE_ORIGIN)
        .map(|hold| render_hold(&hold))
        .collect()
}

/// The inverse of [`render_holds`], FAIL-CLOSED as [`parse_hold`] is: every
/// row is a hold of the origin it reads as, and the first row of an origin is
/// the one that stands. One row — every carry from before the two slots — is
/// exactly that one hold, of that origin.
pub fn parse_holds<'a>(rows: impl IntoIterator<Item = &'a str>) -> Holds {
    let mut holds = Holds::default();
    for row in rows {
        holds.seed(parse_hold(row));
    }
    holds
}

/// The standing hold as the HANDOFF MANIFEST carries it
/// (`SessionRecord::hold`): `"<origin> <reason>"`, both already wire tokens —
/// the origin is `person`, `fleet` or `local` and the reason
/// [`reason_token`]'s output, which holds no space. The durable layouts carry
/// its rows too ([`render_holds`]), on the leaf of a held tab
/// (`restore::TerminalLeafRestore::holds`, LC-8).
pub fn render_hold(hold: &Hold) -> String {
    format!("{} {}", hold.origin, hold.reason)
}

/// The inverse of [`render_hold`], FAIL-CLOSED: every input is a hold.
///
/// The row comes from a file on disk that an older build, a rollback, or
/// anything that can write the private state directory may have produced — and
/// unlike a topic row ([`parse_topics`]), a hold row that does not read cannot
/// be DROPPED, because dropping it lifts a halt. So a row outside the grammar
/// halts the session anyway, `reason=carried-unreadable`, under the STRONGER
/// origin the row could have meant: `person` or `fleet` when it says so first,
/// else `local`. The reason is rebuilt by [`reason_token`] on the way in, as a
/// bridge's would be. The worst a hostile row can do is halt a session, which
/// the owner's `hold off` undoes for a local hold, a reconnecting bridge for a
/// fleet one and the person's Hand Back for the person's. A build from before
/// the person's slot reads a carried `person` row as a local hold it cannot
/// read: still a halt.
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

/// Whether `s` is a `since=` token: `head`, or `@<decimal offset>`.
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
/// Topics are named channels a session asks for, and nothing else
/// bounded how many: every opt-in rides the session's handoff record
/// (`SessionRecord::topics`), which the writer may not shed — a topic is
/// consent, and dropping one silently would stop broadcasts the session asked
/// for — so a script that subscribed one topic per task and never dropped
/// them grew the manifest past the 4 MiB cap every successor reads it under,
/// and the old build refused every update on content. At this bound the
/// longest rows (a 32-character topic and a 20-digit `since=@<offset>`) of
/// every session a handoff carries cost the manifest less than half of the
/// quarter its screens leave.
///
/// Enforced where a topic is ADDED ([`SessionFabric::topic_add`]): an `add` past
/// it answers `ERR topics full`. A set carried across an update is taken whole
/// (`set_topics`; the consumer is only ever more lenient), so a session an
/// older build handed with more keeps them, and adds again once it drops below.
pub const MAX_TOPICS: usize = 32;

/// [`SessionFabric::topic_add`]'s refusal: the set already holds [`MAX_TOPICS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopicsFull;

// inbox / inbox get / inbox seen

const INBOX_USAGE: &str = "ERR usage: inbox [<n>] [since=<id>] [--peek] [--meta] \
                           | inbox get <id> | inbox get @<off> \
                           | inbox seen <id> [handled|refused|deferred]\n";

/// `inbox` — the whole keyword, sub-forms included. The three forms split HERE
/// rather than in the router's `match verb`, so the router keeps one arm per
/// keyword and the sub-keywords never look like verbs of their own.
///
/// The two forms that record a row the BRIDGE acts on — `inbox seen` (its
/// `inbox-seen` event persists the refill floor and prompts a receipt's
/// drain) and a parked `inbox get @<off>` (its `fetch` event prompts the
/// bridge's read) — wake the session's watchers with that record, so the
/// push lane carries the event at once. The listing and `inbox get <id>`
/// record nothing.
pub fn cmd_inbox_verb(ctx: &dyn FabricSession, rest: &str) -> String {
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
fn holder_token(ctx: &dyn FabricSession) -> String {
    ctx.holder_token().unwrap_or_else(|| "-".to_string())
}

/// `inbox [<n>] [since=<id>] [--peek] [--meta]` — the drain a turn-based agent
/// runs at turn start. One header, one row per message, one row per un-landed
/// post; `Lines`-framed, so `OK <n>` counts EVERY row that follows.
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
    // `<n>` keeps the NEWEST n, the `history`/`timeline` grammar: ids rise in
    // ring order, one row each, so that is every selected row past the first
    // `selected - n`.
    let selected = inbox.rows.iter().filter(|r| r.id > since).count();
    let mut skip = limit.map_or(0, |n| selected.saturating_sub(n));

    // `pending=` is what the ring still holds that this reply did NOT carry:
    // rows waiting for an explicit drain. Zero after a bare `inbox` that listed
    // everything.
    //
    // COUNTED OVER THE PER-ROW FLAG, not over a watermark. A non-peek `inbox`
    // marks only the rows it KEPT, so a bounded (`inbox <n>`) or `since=`-filtered
    // reply leaves older rows unlisted BELOW the newest one it carried. A
    // watermark test called every one of those "not pending" and reported
    // `pending=0` with mail still sitting in the ring — the verb table promises
    // "the unlisted delivered rows this reply did not carry", with no watermark qualifier,
    // and this is that number. (The watermark field is gone: one value with two
    // copies is how the two came to disagree.) A row this reply does not carry
    // is never marked by it, so its flag is read as the call found it.
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

/// One `msg` row, appended to `out`. `trust=` is printed before the text ON
/// PURPOSE (§4.3): what a message IS must reach the reader ahead of what it says.
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
    // `truncated=1` means the ENDPOINT NEVER RECEIVED the rest: the delivering
    // bridge cut the body to fit one control request line and named the true size
    // in `len=`. `more=1` means only "this row does not carry the whole of what
    // the endpoint holds" — so a cut body sets BOTH, including the case where
    // what survived is under `TEXT_PREVIEW_MAX` and the row would otherwise have
    // read as a complete short message beside a much larger `len=`.
    let truncated = row.len > row.body.len();
    if row.body.len() > TEXT_PREVIEW_MAX || truncated {
        out.push_str(" more=1");
    }
    if truncated {
        out.push_str(" truncated=1");
    }
    if !meta_only {
        // Cut on a CHARACTER boundary at or below the byte bound: a preview that
        // halved a multi-byte character would render as a replacement glyph in
        // the one field an agent actually reads.
        let mut end = TEXT_PREVIEW_MAX.min(row.body.len());
        while end > 0 && !row.body.is_char_boundary(end) {
            end -= 1;
        }
        out.push_str(" text=");
        aterm_control::wire::pct_encode_into(out, &row.body[..end]);
    }
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
///
/// A read by offset that PARKS records its `fetch` row first, which wakes the
/// session's watchers before the park ([`inbox_get_at`]).
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

/// `inbox get @<off>` — one record BY BROKER OFFSET, from the ring when it
/// still holds it and from the broker's log through the bridge when it does
/// not (E3, "nothing lost"): a row the bounded ring evicted (`dropped=`) or a
/// listing cut (`inbox <n>`, `since=`) is gone from the endpoint, not from the
/// bus, and the offset in the header's `oldest_on_bus=` … `bus_head=` span is
/// still fetchable while the broker holds it.
///
/// THE ENDPOINT HAS NO BUS ACCESS — that is its point — so the read is
/// PARKED: a `fetch sid=<sid> off=<n>` line rides the bridge's next `outbox`
/// peek (prompted by the `fetch` event, with the roster round as a backstop), the bridge
/// answers with `deliver <sid> fetched=<off> …` after one bounded `Fetch` on
/// THIS session's own lane, and the wait wakes on the condvar every other
/// parked verb uses. The bridge's filter is the authority on "yours": another
/// session's record, another face's, or nothing at all is `ERR no such
/// record`, and nothing says which. Bounded by [`FETCH_WAIT_MS`], refused at
/// once when no bridge can answer (`ERR fabric <state>`), and at most
/// [`FETCH_SLOTS`] parked per session. The answer is the WHOLE body — the
/// bridge carries one that does not fit a control line in chunks — up to
/// [`BODY_MAX`], and `truncated=1 len=` for a record larger than that, exactly
/// as `inbox get <id>` says it. NOTHING IS RE-APPENDED: the record is answered,
/// not redelivered, so no id, no watermark, no quota and no event move.
///
/// A ROW THE DELIVERY CUT IS NOT ANSWERED FROM THE RING. `truncated=1` is the
/// other half of what this verb is for (E3: "`truncated=1` becomes
/// fetchable"), and the ring holds only the cut prefix — answering from it
/// first returned the same cut body the reader came here to get past. Such a
/// row parks like an evicted one; only when no bridge can answer is the cut
/// row the answer, still marked `truncated=1`, because it is then the most
/// this endpoint holds.
///
/// A NEWLY PARKED READ WAKES THE PUSH LANE BEFORE IT PARKS. The `fetch` row is
/// what becomes `EVENT <local> fetch`, the bridge's prompt to drain the
/// `outbox` peek, and the bridge has no idle poll: the record wakes the
/// session's watchers as it lands, before the park. A flag returned to the
/// caller could not do it — the call parks until the very answer that event
/// starts.
fn inbox_get_at(ctx: &dyn FabricSession, off: u64) -> String {
    inbox_get_at_within(ctx, off, std::time::Duration::from_millis(FETCH_WAIT_MS))
}

/// [`inbox_get_at`], parked at most `wait`.
fn inbox_get_at_within(ctx: &dyn FabricSession, off: u64, wait: std::time::Duration) -> String {
    let mut inbox = ctx.fabric().lock();
    if let Some(row) = inbox.row_at(off)
        && (row.len <= row.body.len() || wait_is_futile())
    {
        return Fetched::from_row(row).render();
    }
    match inbox.fetches.iter().position(|(o, _)| *o == off) {
        // Answered already: a record is immutable, so the answer stands. A
        // failure is consumed — the next read asks the bus again, because
        // `unreadable` is transient even though `no-record` is not.
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
            // The bridge's idle outbox poll is gone. A parked fetch is work
            // owed now: the record is the push lane's `fetch` event, and it
            // rings the session's watchers as it lands, before the park. The
            // roster round still finds a lost event.
            // The session log's `mail-fetch off=` mark rides the same
            // record (the bridge drains these rows).
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
        // THE ENTRY CHECK ON EVERY WAKE, as `post --wait` does: a bridge can
        // die while a read is parked, and `bridge_lost` wakes every session.
        if wait_is_futile() {
            inbox.fetches.remove(i);
            let why = format!("ERR fabric {} off={off}\n", fabric_state());
            return unfetched(&inbox, off, why);
        }
        if std::time::Instant::now() >= deadline || aterm_control::hangup::caller_hung_up() {
            inbox.fetches.remove(i);
            return unfetched(&inbox, off, format!("ERR timeout off={off}\n"));
        }
        // PARK NO LONGER THAN THE STALE HORIZON ([`park_until`]): the check
        // above answers `ERR fabric stale` there instead of at the read's bound.
        inbox = aterm_control::park::park_guarded(
            &ctx.fabric().changed,
            inbox,
            park_until(deadline),
            aterm_control::park::Lane::SERVED,
        );
    }
}

/// A read the bus could not complete: the ring's CUT row when it holds the
/// record — still marked `truncated=1`, because it is then the most this
/// endpoint has — and `why` otherwise. Only a cut row ever reaches the bus
/// path while the ring holds it, so this is never a whole row passed off as
/// a failure's answer.
fn unfetched(inbox: &Inbox, off: u64, why: String) -> String {
    match inbox.row_at(off) {
        Some(row) => Fetched::from_row(row).render(),
        None => why,
    }
}

/// The reply a failed fetch gives: `no-record` is the one answer that is the
/// same for "another session's" and "nothing there", on purpose. Below what
/// the broker's retention keeps of this session's lane it is that floor,
/// `ERR retained-from=<floor>`: the floor is
/// this lane's own, so it says nothing of whose record `off` was.
fn fetch_failure(why: &str, off: u64) -> String {
    if why == "no-record" {
        format!("ERR no such record off={off}\n")
    } else if let Some(floor) = why.strip_prefix("retained-from=") {
        format!("ERR retained-from={floor} off={off}\n")
    } else {
        format!("ERR fetch failed reason={why} off={off}\n")
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
/// only through `--peek` listings and never runs a bare `inbox` could
/// acknowledge all of its mail and still be refused the next message with
/// `ERR quota` forever. The `inbox seen` verb row
/// says so; a change here that stopped touching `listed` would silently reinstate
/// that deadlock, so `an_ack_releases_the_senders_quota` pins it.
///
/// AND A VERDICT IS A RECEIPT (R8). `inbox seen <id> handled|refused|deferred`
/// records the word on THAT row — the one the id names, not every row the
/// watermark passes, which is why a bare `inbox seen <id>` records no verdict
/// at all — and the `inbox-seen` event then carries `verdict=<v> kind=<k>
/// from=<p>`, which is everything a bridge running `--receipts` needs to
/// publish `kind=ack re=<off> verdict=<v>` onto the sender's lane. Once per
/// row per CHANGE: the same word again records the plain event (the watermark
/// still moves), a different word (`deferred`, later `handled`) is a new
/// receipt. A `--peek` records nothing and so acks nothing.
///
/// An answered `OK` records the `inbox-seen` row, which wakes the session's
/// watchers: that event is what the bridge persists the refill floor from and
/// drains a receipt on, so a `--wait-ack` sender hears it at once. A bare
/// `inbox seen` records it too — it moves the floor.
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
    // A VERDICT MAY OWE A RECEIPT, which a committing update carries as the
    // park drew it: held back until the attempt lands or stands down, like a
    // post ([`SessionFabric::wait_unfenced`]).
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
    // A row delivered before the bridge said the log changed, at an offset the
    // log it reads now does not hold: its `off` is an earlier log's
    // ([`InboxRow::on_log`]), and so is its receipt's `re=`.
    let earlier_log = !inbox.rows[at].on_log;
    let decided = verdict.filter(|v| inbox.rows[at].decision.as_deref() != Some(v));
    let mut verdict_tail = String::new();
    if let Some(v) = decided {
        let row = &mut inbox.rows[at];
        row.decision = Some(v.to_string());
        verdict_tail = format!(" verdict={v} kind={} from={}", row.kind, row.from);
        // OWED, NOT MERELY ANNOUNCED. The event is the bridge's prompt trigger;
        // the queue is what survives a broker outage or a bridge relaunch
        // between this moment and the publish (see [`OwedReceipt`]). Only the
        // kinds that wait earn one — a demoted task is a `note` row here.
        if matches!(row.kind.as_str(), "ask" | "task") {
            let (kind, from) = (row.kind.clone(), row.from.clone());
            owe_receipt(&mut inbox, off, v, &kind, &from, earlier_log);
        }
    }
    // MONOTONE: the watermark never goes backwards, so acknowledging an older row
    // out of order cannot re-open rows already handled.
    let seen_before = inbox.seen;
    inbox.seen = inbox.seen.max(id);
    for row in &mut inbox.rows {
        if row.id <= id {
            row.listed = true;
        }
    }
    let seen = inbox.seen;
    // `off=` is the NAMED row's offset (a broadcast's included); `floor=` is
    // what the bridge refills this session's addressed lane from
    // ([`addressed_floor`]), and the only one of the two it persists. `seen=`
    // is the WATERMARK — every row at or below it is handled, whichever lane
    // it came on — which is what releases a broadcast's refill floor
    // (`aterm-link`'s `Bridge::unhandled`). It is not `<id>`: acknowledging
    // an older row out of order leaves the watermark where it was.
    let floor_at = addressed_floor(&inbox);
    let floor = floor_at.map_or_else(|| "-".to_string(), |f| f.to_string());
    let payload = format!("{id} off={off} floor={floor} seen={seen}{verdict_tail}");
    // The session log's `mail-seen` mark, every one (6F-6): `moved=` says
    // whether the act moved something (the watermark, or a verdict
    // decided), and the fold folds only the moves. The record rings the
    // session's watchers, the log's readers among them.
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

/// THE ADDRESSED LANE'S REFILL FLOOR, as `inbox seen` reports it on its
/// `inbox-seen` event (`floor=`): the offset the bridge refills this session's
/// ring from when a relaunch keeps the sid but not the ring — a seamless
/// update (its bridge persists `floor - 1` as `seen/<sid>`). `None` (`floor=-`)
/// when this ring was never delivered an addressed row, so there is nothing to
/// say and the bridge persists nothing.
///
/// THE LOWEST OFFSET OF AN UNHANDLED ADDRESSED ROW (`topic` none, id past
/// `seen`), else one past the highest addressed offset ever delivered here.
/// So it never passes an addressed row the agent has not handled while the
/// ring holds it (one the ring evicted unhandled is counted in `dropped=` and
/// still fetchable by `inbox get @<off>`), and a BROADCAST never moves it.
///
/// IT USED TO BE THE NAMED ROW'S OWN OFFSET, which was a safe floor only while
/// ids and offsets rose together — true on the addressed lane alone, which
/// one subscription delivers in offset order and a relaunch refills before
/// that subscription opens. Broadcasts share the id counter but not the
/// order: they arrive on a second subscription, which nothing orders against
/// the first, and through the topic catch-up, which delivers OLDER offsets
/// later. So acknowledging a broadcast could lift the floor past an unhandled
/// addressed row of a lower offset (lost at the next update, uncounted) or
/// leave it below a handled one of a higher offset (offered again as fresh
/// unread mail, and decided twice).
fn addressed_floor(inbox: &Inbox) -> Option<u64> {
    // Only THIS log's rows: an earlier log's offset is no floor here
    // ([`InboxRow::on_log`]), and its rows cannot be refilled from this log.
    inbox
        .rows
        .iter()
        .filter(|r| r.topic.is_none() && r.id > inbox.seen && r.on_log)
        .map(|r| r.off)
        .min()
        .or_else(|| inbox.addressed_head.map(|h| h.saturating_add(1)))
}

// post

const POST_USAGE: &str = "ERR usage: post to=<@<sid>[@<node>]|<principal>|say[:<topic>]> \
                          kind=<ask|answer|task|report|note|ack|control> [re=<n>] [dl=<ms>] \
                          [via=<p>] [key=<token>] [--wait[=<ms>]] [--wait-ack[=<ms>]] \
                          ([--] <text> | len=<n> + <n> raw bytes)\n";

/// The answer to a `post … len=` value that is not a byte count.
const POST_LEN_USAGE: &str = "ERR usage: post len=<n> must be a byte count\n";

/// Parse a `post` `len=` value: a byte count up to [`BODY_MAX`], or the whole
/// refusal line. A count over the cap and a value that is no count at all answer
/// differently, so a typo never reads as a size problem. Shared by the framed
/// detector (`control::post_frame_len`) and [`cmd_post`], so the two cannot
/// answer the same `len=` two ways.
pub fn post_len(v: &str) -> Result<usize, String> {
    match v.parse::<usize>() {
        Ok(n) if n <= BODY_MAX => Ok(n),
        _ if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => Err(post_body_too_large()),
        _ => Err(POST_LEN_USAGE.to_string()),
    }
}

/// The refusal for a post body over [`BODY_MAX`].
fn post_body_too_large() -> String {
    format!("ERR too large: a post body is capped at {BODY_MAX} bytes\n")
}

/// `post`'s END OF OPTIONS: every token after it is the body, whatever it
/// spells. A caller posting text it did not write (a peer's mail, an issue, a
/// model's words) puts it after this, or a body opening `to=@s-x kind=answer
/// re=<n>` picks its own recipient, kind and correlation id under the sender's
/// identity — and `len=<n>` makes the serve loop wait for a frame nobody sends.
pub const POST_OPTIONS_END: &str = "--";

/// `post`'s leading OPTION tokens out of `toks`: every token up to the first
/// that is not one, or up to [`POST_OPTIONS_END`]. The ONE reading of where the
/// options end, shared by the handler, the inline body's offset and the serve
/// loop's length-prefixed-frame detector, so they cannot disagree about where
/// the options end and the body begins — a detector that saw a frame the handler
/// did not would leave the announced bytes to be read as control verbs.
pub fn post_options<'a>(toks: impl Iterator<Item = &'a str>) -> impl Iterator<Item = &'a str> {
    toks.take_while(|tok| *tok != POST_OPTIONS_END && post_option_token(tok))
}

/// Whether `tok` is one of `post`'s leading OPTION tokens (read through
/// [`post_options`], which also stops at [`POST_OPTIONS_END`]).
fn post_option_token(tok: &str) -> bool {
    tok == "--wait"
        || tok.starts_with("--wait=")
        || tok == "--wait-ack"
        || tok.starts_with("--wait-ack=")
        || ["to=", "kind=", "re=", "dl=", "via=", "key=", "len="]
            .iter()
            .any(|k| tok.starts_with(k))
}

/// The byte offset in `rest` where the inline body begins — the first token that
/// is not an option, or the first after [`POST_OPTIONS_END`] — or `None` when the
/// line is options all the way down.
///
/// Scans FORWARD from the end of the previous token rather than searching the
/// whole string for each one, so a body whose first word also appears inside an
/// earlier option value cannot make the body start in the wrong place.
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

/// How long `post --wait` parks by default, and the ceiling on an explicit wait.
const WAIT_DEFAULT_MS: u64 = 30_000;
const WAIT_MAX_MS: u64 = 600_000;

/// `post` — queue one outbound message and, for the kinds that need a
/// correlation id, wait for the offset it landed at.
///
/// `body` is `Some` only for the length-prefixed frame form, whose bytes the
/// serve loop has already read off the stream.
///
/// An accepted post's `post` timeline record wakes the session's watchers,
/// BEFORE any `--wait` parks, so the push lane's `EVENT <sid> post` — the
/// bridge's prompt trigger to drain the outbox — leaves at once. After the
/// wait would be too late: a `--wait` post is waiting for exactly the drain
/// that trigger starts.
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
    // Options lead; the first token that is not one (or the first after `--`)
    // begins the inline text, so a body may contain `=` and spaces without
    // quoting.
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
    // `--wait-ack` IS FOR THE KINDS THAT WAIT: a receipt is published for an
    // `ask` or a `task` and nothing else (R8), so waiting for one on a `note`
    // would be a guaranteed timeout.
    if wait_ack.is_some() && !matches!(kind.as_str(), "ask" | "task") {
        return POST_USAGE.to_string();
    }
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
    // ONE OPTIONAL `@`, NOT A RUN OF THEM. `trim_start_matches` strips EVERY
    // leading `@`, so `@@@…@s-b` — one whitespace-free token, and the request
    // line runs to `control::MAX_REQUEST_LINE` — validated as `s-b` and was then
    // kept VERBATIM as the row's `to`. That is the field
    // [`PostRow::caller_sized_bytes`] declines to fold on the stated ground that
    // it is "grammar-bounded to tens of bytes", and the field [`retire_post`]
    // does not clear, so [`RETIRED_KEEP`] held 512 of them for the life of the
    // session and [`RETIRED_KEEP`]'s own doc called the survivor "a principal".
    // Neither sentence was true while the padding was accepted, and the padding
    // is no part of the address grammar the `post` row publishes
    // (`to=<@<sid>[@<node>]|<principal>|say>`) — so it is REFUSED here rather
    // than trimmed here and refused by `resolve_to` a publish later.
    // `say` and `say:<topic>` are the BROADCAST addresses. The topic is one
    // subject segment, so it is validated HERE — a bad one is a usage error the
    // sender sees, not a post the bridge retires as unroutable a publish later.
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
    // LOSSY FOR NON-UTF-8, and said out loud: the control reply plane is
    // `String`-typed end to end, so a body that is not UTF-8 is replaced here
    // exactly as `inbox get`'s is on the way in. `outbox`'s length prefix buys
    // framing (a body may hold newlines), not byte-exactness; a byte-exact
    // outbound path needs a bytes-carrying `ControlReply`, which is a wider
    // change than one verb pair.
    //
    // CONVERTED BEFORE THE DOOR, not at the push, because the replacement can
    // TREBLE the length (every invalid byte becomes a three-byte U+FFFD) and it
    // is the converted string the row keeps and [`Inbox::queued_load`] folds.
    // Weighing the frame's raw length against a bound denominated in stored
    // bytes admitted a 128 KiB frame as if it were not a 384 KiB row. The
    // declared-size refusal above stays on the RAW bytes: [`BODY_MAX`] is the
    // wire's "up to 256 KiB", which is a statement about the frame.
    let body = String::from_utf8_lossy(&body).into_owned();

    // `--wait` is ON by default for the two kinds whose whole point is a reply:
    // an `ask`/`task` sender that does not learn its offset cannot recognize the
    // answer when it comes back as `re=`. And `--wait-ack` implies it: the
    // receipt names the offset, so the offset has to be known first.
    let wait = wait.or_else(|| {
        (matches!(kind.as_str(), "ask" | "task") || wait_ack.is_some()).then_some(None)
    });

    // A COMMITTING UPDATE HOLDS THE POST BACK ([`SessionFabric::wait_unfenced`]):
    // the successor carries the outbox the park drew, so a post queued here
    // after the Commit's reading would be answered `OK` and lost with this
    // process.
    let Some(mut inbox) = ctx
        .fabric()
        .wait_unfenced(ctx.fabric().lock(), ctx.fabric().fence_wait())
    else {
        return HOLD_BUSY.to_string();
    };
    // REFUSED AT THE DOOR. A full outbox is answered, not absorbed: see
    // [`OUTBOX_CAP`]. Counted over the posts still WAITING for the bridge —
    // retired ones hold no body and cost nothing.
    //
    // THE INCOMING ROW IS MEASURED THE WAY THE QUEUED ONES ARE, by the same
    // function: [`caller_sized_bytes`]. This line used to add `body.len`
    // alone, so the one row whose admission was being decided was the one row
    // whose `via` went uncounted.
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
    // THE BRIDGE NOW OWES AN ANSWER. The record below is what becomes the push
    // lane's `EVENT <sid> post <id>`, so from here the endpoint has asked and
    // the bridge has not yet moved it. A bridge that is working answers in
    // milliseconds and [`settle_work_owed`] runs; one that is wedged says
    // nothing, and [`STALE_AFTER`] later `fabric=` reads `stale`.
    //
    // ARMED BEFORE THE RECORD, under the guard that admitted the row: the
    // record rings the bridge's push lane, and a bridge that drained, landed
    // and settled the post before this ran would leave the clock armed with
    // nothing owed, so `fabric=` would read `stale` and cut the next `--wait`
    // short. `ctx.fabric` → `LINK.link` is this file's one order.
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
    // The record rings the session's watchers, the session log's readers
    // among them, so the wake the hook owes is the record's own.
    let _ = ctx.record_fact("post", payload, fact);
    drop(inbox);
    ctx.fabric().changed.notify_all();
    ctx.fabric_changed();

    let Some(wait) = wait else {
        return format!("OK {id}\n");
    };
    // Waiting for a landing that nothing can report is a guaranteed timeout, so
    // say so at once and name the id: the post IS queued, and `inbox` lists it.
    // `stalled` is refused here exactly like `disconnected`: the bridge has
    // said its broker link is down, and a wait on it would burn its whole
    // timeout for an ack the bridge itself is not getting. `stalled` with
    // NOTHING said yet is not that — see [`wait_is_futile`].
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
    // THE RECEIPT'S BOUND IS THE DEADLINE, unless the caller said otherwise:
    // `dl=` is the time the sender gave the recipient, so it is the time the
    // sender waits for the recipient's word. The asker's own bridge records
    // `expired` at that moment, and that verdict releases the wait — so the
    // bound is `dl=` plus the time the verdict takes to come back
    // ([`EXPIRY_GRACE_MS`]), or the wait would end first and answer `ERR
    // timeout` for a deadline the bus says `expired`. An explicit
    // `--wait-ack=<ms>` is the caller's own bound and is kept exactly.
    let bound = match wait_ack {
        Some(ms) => ms,
        None => dl.map_or(WAIT_DEFAULT_MS, |d| d.saturating_add(expiry_grace_ms())),
    }
    .min(WAIT_MAX_MS);
    wait_receipt_on_log(ctx, id, dup, &watch, bound)
}

/// Park until the post `id` LANDS: its offset, duplicate flag and a watch on
/// that offset armed under the same guard ([`OffsetWatch`], for the receipt
/// wait that may follow, so no move between the two goes unseen), or the
/// reply to give instead — a verdict, a futile link, or the timeout.
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
            // `dup=1` IS THE BRIDGE'S REPORT of the broker's verdict: this post
            // re-used a sequence an earlier `key=` reserved, the broker deduped
            // it, and `off` is the ORIGINAL record's. One record on the bus.
            let dup = if row.dup { " dup=1" } else { "" };
            let on_log = row.landed_on_log;
            return Ok((off, dup, guard.watch(off, on_log)));
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
            return Err(format!("ERR {reason} id={id}\n"));
        }
        // THE ENTRY CHECK, ON EVERY WAKE — checked AFTER the landing and the
        // verdict, so a post that landed just before the bridge died still
        // answers `OK … off=`. Waiting for a landing that nothing can report is a
        // guaranteed timeout (§11.2 deviation 9), and a bridge can die while a
        // waiter is parked exactly as easily as before it parked; the entry check
        // alone left that caller sitting out its whole `--wait` (up to
        // `WAIT_MAX_MS` = 600 s) for the `ERR timeout` the check exists to
        // replace. `bridge_lost` wakes every registered session so this runs.
        if wait_is_futile() {
            return Err(fabric_wait_refusal(id));
        }
        // The caller hung up: nobody can read the verdict, so the lane goes
        // back now. The post itself is untouched — it lands or not as it would.
        if aterm_control::hangup::caller_hung_up() {
            return Err(aterm_control::hangup::HUNG_UP_REPLY.to_string());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("ERR timeout id={id}\n"));
        }
        // PARK NO LONGER THAN THE STALE HORIZON ([`park_until`]): the next
        // turn of the loop answers `ERR fabric stale id=<n> queued=1` instead
        // of sitting out the remaining minutes.
        guard = aterm_control::park::park_guarded(
            &ctx.fabric().changed,
            guard,
            park_until(deadline),
            aterm_control::park::Lane::SERVED,
        );
    }
}

/// `post --wait-ack`'s second wait: the post has landed at `off`, and the
/// caller wants the RECIPIENT's word (R8). Released by an `ack` row carrying
/// `re=<off>` — `OK <id> off=<n> ack=<verdict> msg=<inbox id>` — or by the
/// asker's own bridge recording the deadline passed, `ERR expired id=<n>
/// off=<n>`. A timeout and a futile link both name the offset, because the
/// post DID land: `await inbox re=<off>` picks the wait up later, and nothing
/// here is a reason to re-post.
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

/// [`wait_receipt`]'s wait, on the post's landing offset `post.off` as the
/// watch [`wait_landing`] armed holds it.
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
        // The NEWEST word wins: `deferred` then `handled` is two receipts, and
        // the later one is the recipient's current answer.
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
        // PARK NO LONGER THAN THE STALE HORIZON ([`park_until`]). THIS WAIT
        // IS THE LONGER OF THE TWO — its bound is the sender's `dl=` plus a
        // grace, up to `WAIT_MAX_MS` — so sitting it out is the more expensive
        // mistake, and shipped the cap on the landing wait alone.
        guard = aterm_control::park::park_guarded(
            &ctx.fabric().changed,
            guard,
            park_until(deadline),
            aterm_control::park::Lane::SERVED,
        );
    }
}

/// The instant a fabric wait that consults [`wait_is_futile`] parks to: its
/// `deadline`, cut to the moment an owed answer goes stale ([`stale_in`]).
/// Nothing signals the condvar when a bridge simply stops answering — that is
/// what makes `stale` worth having — so the wait looks again exactly when the
/// futility check starts returning true. A slice of a served lane asks only
/// after the caller ([`aterm_control::park`]), so this bound is what brings the look.
///
/// WITH NOTHING OWED, THE BOUND IS [`stale_after`] FROM NOW. Arming the owed
/// clock wakes no parked waiter: another session's `post` arms it under that
/// session's own fabric lock and notifies only its own condvar, and a
/// bridge's first `link up` with posts queued arms it with a presence wake.
/// A debt armed after this instant goes stale no sooner than `stale_after`
/// from now, so a wait that parked debt-free still looks by its horizon, at
/// one look per `stale_after`.
fn park_until(deadline: std::time::Instant) -> std::time::Instant {
    let horizon = stale_in()
        .filter(|d| !d.is_zero())
        .unwrap_or_else(stale_after);
    deadline.min(std::time::Instant::now() + horizon)
}

/// The answer a `post --wait` gives when the link cannot report a landing.
///
/// `queued=1`, AND IT IS THE LOAD-BEARING TOKEN. A bridge EXIT is the ordinary
/// relaunch path, not only the terminal one: `fabric_launch::supervise` brings a
/// replacement up after `RELAUNCH_MIN`, and the post survives it untouched —
/// `outbox` is a PEEK that removes nothing, so the new bridge drains the same
/// queue and `outbox sent` retires it seconds later. Answering a bare `ERR fabric
/// disconnected` for a message that is still queued reads as "not sent", and the
/// documented remedy for "not sent" is to send again: a `post` without `key=`
/// carries no idempotency key, so the peer's inbox ends up holding the same
/// `ask` twice (and one WITH `key=` should still not be re-posted on this
/// answer — it is queued, and a re-post only costs a deduped publish).
/// This refusal says the only two things the endpoint actually knows — nothing
/// can report the landing, and the message is still in the outbox.
///
/// …EXCEPT WHEN NOTHING IS COMING, WHICH IS THE STATE THAT ARGUMENT DOES NOT
/// REACH. `queued=1`'s whole meaning is "a replacement bridge will publish it",
/// and on an instance with no `[fabric] command` there is no first bridge, let
/// alone a replacement: aterm-gui's `fabric_launch::spawn_supervisor` starts nothing
/// and `outbox` is never called by anyone. Told `queued=1`, an agent following
/// the verb row LITERALLY does not re-post and waits for an answer no process
/// will ever fetch — 128 times, until `ERR outbox full` becomes the first hint
/// anything is wrong. So that state answers `no-bridge=1` INSTEAD of `queued=1`:
/// the message is in the outbox and nothing will ever take it out, which is a
/// different instruction (stop waiting; this instance has no fabric) and is
/// spelled differently.
///
/// `absent` alone would be the wrong test — it is also the state during the
/// seconds before the FIRST bridge finishes attaching, where `queued=1` is
/// exactly right. [`bridge_reachable`] asks the question that actually
/// distinguishes them: has a supervisor started, or has a bridge ever been up.
///
/// `stalled` is always `queued=1`: a bridge IS attached, it is redialing the
/// broker with back-off, and it drains this same outbox the moment its link
/// comes up — so `ERR fabric stalled id=<n> queued=1` is "wait, do not re-post",
/// answered at once rather than after the wait a landing could not end.
fn fabric_wait_refusal(id: u64) -> String {
    let state = fabric_state();
    if bridge_reachable() {
        format!("ERR fabric {state} id={id} queued=1\n")
    } else {
        format!("ERR fabric {state} id={id} no-bridge=1\n")
    }
}

/// Whether a `post --wait` parked now could only time out: no bridge, a
/// bridge that is gone, or a bridge that has SAID its link is down. A bridge
/// that is attached and has said nothing yet (`stalled reason=starting`) is
/// none of those: a bridge reports within milliseconds either way
/// and a `down` wakes the waiter ([`link_report`]); a bridge from before the
/// report never says anything and lands the post all the same, and the
/// landing itself is what moves the state ([`link_evidence`]). Refusing the
/// wait on `starting` failed every `ask`/`task` from such a bridge's
/// sessions at once (measured 2026-09-14).
fn wait_is_futile() -> bool {
    match LINK.state.load(Ordering::Relaxed) {
        // `stale` IS ALWAYS `queued=1`, for the same reason `stalled` is: the
        // post is in the outbox and the bridge drains it the moment it moves
        // again. The difference from `stalled` is only in what to go and fix
        // — a bridge that SAID its link is down against one that has said
        // nothing at all while owing an answer ([`STALE_AFTER`]).
        FABRIC_CONNECTED if link_is_stale() => true,
        FABRIC_CONNECTED => false,
        FABRIC_STALLED => {
            let link = LINK.link.lock().unwrap_or_else(|p| p.into_inner());
            link.reported || link.reason != LINK_STARTING
        }
        _ => true,
    }
}

// await inbox

/// `await inbox since=<id> [kinds=<k,...>] [topic=<t>] | await inbox re=<off>
/// [since=<id>] [kinds=<k,...>]` — the fabric wake predicate.
///
/// MONOTONE: it latches only on a row whose id is strictly greater than `since`,
/// so a row the agent read and chose to ignore cannot latch the same wait twice.
/// A `hold` transition latches only when `hold` is one of the listed kinds — a
/// halt reaches an agent through the events digest, never by pretending to be
/// mail. `OK timeout` on expiry, which the client exits 124 on.
///
/// `re=<off>` NARROWS IT TO THE REPLIES TO ONE POST: a row carrying that
/// `re=` of any kind — an `answer`, a `report`, a receipt (`ack … verdict=`),
/// the asker's own bridge's `expired` — and `since=` may then be omitted (0),
/// because the offset is already a thing that happens once. `kinds=` still
/// narrows it further when given.
///
/// `topic=<t>` NARROWS IT TO ONE CHANNEL: a BROADCAST row that arrived on that
/// topic (the session ran `topic add <t>`, and the row carries `topic=<t>`).
/// It is how a session chooses to be pushed to — nothing aterm types wakes
/// an agent for mail, so the wait it runs in the background is the push. A
/// broadcast is usually a `note`, and naming the topic accepts every kind on
/// it, so the default skip does not apply; `kinds=` still narrows it when
/// given. It sleeps through addressed mail: a row without the topic never
/// latches it.
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
        // A reply to a named post is wanted whatever its kind, and so is a
        // broadcast on a named topic (usually a `note`); an unnamed wait skips
        // the class that must never wake anybody by itself.
        None => re.is_some() || topic.is_some() || kind != AWAIT_DEFAULT_SKIP,
    };
    // A reply named by `re=` is one to a post of this log: an earlier log's
    // rows ([`InboxRow::on_log`] false) share its offsets. One whose own
    // offset the log still holds names the same record by its `re=`, which
    // is always below it, whatever moves it was delivered across.
    let latches = |r: &InboxRow| {
        r.id > since
            && accepts(&r.kind)
            && re.is_none_or(|n| r.on_log && r.re == Some(n))
            && (topic.is_none() || r.topic == topic)
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let mut guard = ctx.fabric().lock();
    // AN ANCHOR ABOVE EVERY ID THIS SESSION HAS HANDED OUT names no row of
    // this process's: nothing
    // here can have told a caller an id it has not minted, so it was taken
    // from another process — a producer before the id carry, whose successor
    // counts from 1 again. Parked, it waited for a count the ring may never
    // reach and answered `OK timeout` over unread mail; refused, the caller
    // re-reads `inbox` and takes the new high, as `timeline` and `offscreen`
    // refuse an index past their newest.
    if since > guard.last_msg_id {
        return format!("ERR bad since high={}\n", guard.last_msg_id);
    }
    let hold_at_arm = guard.holds.is_held();
    // The offset `re=` names, as the log the bridge reads now holds it, for
    // as long as this wait is parked ([`OffsetWatch`]).
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
        // Mail that came first is still answered; after it, the end.
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

/// THE TESTS' SESSIONS AND REGISTRY: the least of aterm-gui's `SessionCtx`
/// and `SessionStore` the fabric reaches through [`FabricSession`] and
/// [`Registry`], so these tests drive the same handlers the dispatch does.
#[cfg(test)]
mod test_support {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Condvar, Mutex, RwLock};
    use std::time::{Duration, Instant};

    use aterm_session::SessionId;

    use super::{FabricFact, FabricSession, Registry, SessionFabric, Timeline};

    /// What every record rings, as aterm-gui's timeline rings its session's
    /// watchers on each append: the wake the push lane and the session log's
    /// readers wait on. A count of rings under a condvar.
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

    /// One watcher of a session's [`Watchers`]: its waits answer whether the
    /// session rang since the last one answered (or since it subscribed), every
    /// ring between two waits coalesced into one wake.
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

    /// A session's timeline: every record, in order, and every typed fact
    /// the session log's hook was handed ([`FabricSession::record_fact`]).
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

        /// The fact as its `Debug`, and a wake owed for every one (the
        /// host's log always moves on a mark).
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

    /// A session: its id, its fabric endpoint, its timeline and the watchers
    /// every record rings.
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

    /// What `test_handle` hands a test: the session, before it is registered.
    pub(crate) struct TestHandle {
        pub(crate) sid: SessionId,
        pub(crate) ctx: Arc<SessionCtx>,
        local_id: u64,
    }

    /// A fresh, unregistered session with process-local id `local_id`, on the
    /// test timing value (the fence and notice waits at their test lengths).
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
        /// Every `link set-aside` count handed over
        /// ([`Registry::torn_state_set_aside`]), in order.
        pub(crate) set_aside: std::sync::Mutex<Vec<u64>>,
        /// Whether each of those came from a bridge that says when its
        /// quarantine is empty, in the same order.
        pub(crate) told_when_empty: std::sync::Mutex<Vec<bool>>,
        /// Every `link quarantine` count handed over
        /// ([`Registry::torn_state_held`]), in order.
        pub(crate) held: std::sync::Mutex<Vec<u64>>,
    }

    impl TestStore {
        /// Register a session; a first registration records `spawned`, as the
        /// real store's does.
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

    /// **THE HANDOFF CARRY RE-VALIDATES EVERY ROW IT READS.**
    ///
    /// The manifest is a file on disk. An older build, a rollback, or anything
    /// that can write the state directory may have produced it, and what it
    /// carries here is CONSENT — the only reason a broadcast reaches a session
    /// at all. So a row that is not a topic and a `since` that is not a token
    /// are dropped rather than repaired: a malformed row must not be able to
    /// invent an opt-in, and a repaired one would be an opt-in nobody asked
    /// for under a name nobody typed.
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
        // A 0.90.x manifest's third field (that release's add serial) is read
        // past.
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

    /// The instance bridge link is process-global (one bridge per instance), so
    /// every test that reads or moves it goes through [`with_link_reset`] and
    /// starts from `absent`. Everything else here is per-session and parallel.
    use super::with_link_reset as with_link;

    /// A registered session: the real `SessionHandle` the store builds, so these
    /// tests drive the same `ctx.fabric` the dispatch reaches.
    fn registered(store: &Store) -> (String, std::sync::Arc<SessionCtx>) {
        registered_as(store, 1)
    }

    /// Attach one incarnation AND bring its link up — what a bridge whose
    /// broker answers does within milliseconds of being served. An attach on
    /// its own is `stalled`: the link is the bridge's to report.
    fn attach_up(store: &Store, generation: BridgeGeneration) {
        bridge_attached(generation);
        assert!(link_report(store, Some(generation), true, "", Some(1)));
    }

    /// Mirror serve_bridge's thread-local identity for one fixture operation.
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

    /// [`registered`] with an explicit `local_id`, for the tests that need two
    /// sessions in one store — the store lists by `local_id`, so this is also
    /// what fixes the order `outbox` drains them in.
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

    /// A broadcast delivery, as the bridge sends one to a subscribed session.
    /// The session must hold `topic` ([`SessionFabric::topic_add`]), or the
    /// endpoint refuses it `ERR not-subscribed`.
    pub(super) fn broadcast(store: &Store, sid: &str, off: u64, topic: &str, text: &str) -> String {
        cmd_deliver(
            store,
            &format!(
                "{sid} off={off} from=s-peer@n-lab kind=note trust=agent topic={topic} text={text}"
            ),
        )
    }

    /// **THE PUSH FACE OF THE RING: METADATA, A CURSOR, AND A GAP.**
    /// `mail_since` answers the rows above a cursor that the filter wants,
    /// never their bodies; the cursor it hands back is the ring's HIGH, so a
    /// row the filter rejected is stepped over once instead of re-walked on
    /// every wake; and the oldest id it reports is what tells a subscriber the
    /// ring dropped rows it was never shown.
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

        // EVERYTHING, from the beginning: three rows, metadata only.
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

        // A CURSOR: only what is newer.
        let after = ctx.fabric.mail_since(2, &MailFilter::default());
        assert_eq!(after.rows.iter().map(|r| r.id).collect::<Vec<_>>(), [3]);

        // `kinds=` and `from=` narrow, and `from=` takes a CLASS as well as a
        // principal — but the cursor still advances past what they rejected.
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

    /// **A GAP COUNTS WHAT WAS EVICTED, NOT WHAT IS BELOW THE FRONT ROW.**
    /// Eviction is by CLASS — an agent's row goes first, wherever it sits — so
    /// a human row pinned at the front keeps the ring's oldest id still while
    /// agent rows behind it vanish. `present` is the honest input: how many
    /// rows above the cursor the ring actually holds, so
    /// `(high - cursor) - present` is what was lost, whether or not the front
    /// moved.
    #[test]
    fn mail_since_reports_what_was_evicted_even_when_the_front_row_is_pinned() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        // One human row first — eviction will never choose it — then enough
        // agent rows to overflow the ring.
        assert!(deliver(&store, &sid, 100, "h-andrew", "task", "x").starts_with("OK"));
        // A DISTINCT SENDER PER ROW: `SENDER_QUOTA` caps one sender at 64
        // unread rows, and a quota refusal would end the fixture long before
        // the ring overflowed.
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
        // The pinned human row is still there, so a front-anchored gap would
        // have reported nothing at all.
        assert_eq!(
            ctx.fabric.lock().rows.front().map(|r| r.id),
            Some(1),
            "the human row is pinned at the front, which is the trap"
        );
    }

    /// `cmd_hold` as the BRIDGE issues it — the form every pre-local-halt test
    /// here drives, and the one whose rule the local halt left untouched.
    fn bridge_hold(store: &Store, rest: &str) -> String {
        cmd_hold(store, rest, HoldIssuer::Bridge)
    }

    /// `cmd_hold` as the local OWNER token issues it.
    fn owner_hold(store: &Store, rest: &str) -> String {
        cmd_hold(store, rest, HoldIssuer::Owner)
    }

    /// A COMMITTING UPDATE HOLDS EVERY HOLD BACK AND DROPS NONE (the
    /// round-four review). While a session is fenced
    /// ([`SessionFabric::fence_hold`]) a verb's act waits and, past its bound,
    /// answers [`HOLD_BUSY`] with the serial unmoved; the bridge-lost sweep's
    /// act (no bound) waits and LANDS the moment the attempt stands down —
    /// the fail-closed halt is delayed by a Commit, never dropped by one.
    ///
    /// FAILS WITHOUT THE FIX: there was no fence, so the verb answered
    /// `OK hold=1` and moved the serial the Commit had already compared.
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
        // Unfenced, the verb is answered at once again.
        assert_eq!(owner_hold(&store, &format!("{sid} off")), HOLD_DENIED);
    }

    /// `post` AND `deliver` HOLD THEIR WORDS TO THE FABRIC'S ONE GRAMMAR.
    ///
    /// The grammar is `aterm_types::fabric_grammar`'s, walked at its edges
    /// there. What is pinned here is that this endpoint APPLIES it at both
    /// seams, because each is one field on both: the outbound `post` line the
    /// endpoint WRITES and the inbound `deliver` line it READS. This end once
    /// kept a spelling of its own that admitted a 40-byte principal name against
    /// §3.2's 32 and an unbounded `via=` — lines its own bridge then refused.
    #[test]
    fn post_and_deliver_hold_their_words_to_the_fabric_grammar() {
        let name = |n: usize| "a".repeat(n);
        let store = new_store();
        let (sid, ctx) = registered(&store);

        // A principal one byte past §3.2's name bound, as `to=` and as `from=`.
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
        // The `<owner>@<node>` address — this endpoint's own wider form — bounds
        // BOTH halves by the same number.
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

        // A chain one hop past the bound, and a hop in the address form: a
        // `via=` hop is a BARE principal, and the bridge re-checks the line.
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

    /// THE OUTBOX'S TWO BOUNDS COUNT EVERY CALLER-SIZED FIELD, AND A RETIRED ROW
    /// KEEPS NONE OF THEM.
    ///
    /// `queued_load` folded `p.body.len` alone, so `via` — the other
    /// caller-chosen string on the same `post` line — was outside the 4 MiB
    /// bound; and `retire_post` cleared the body and left `via` on the row, which
    /// [`RETIRED_KEEP`] then held 512 of for the life of the session, while both
    /// of the docs describing a retired row said it carries "only (id, to, kind,
    /// off)".
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

        // The bridge takes it and retires it.
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

    /// AN ADDRESS CARRIES ONE `@`, NOT A RUN OF THEM.
    ///
    /// The grammar check began `to.trim_start_matches('@')`, which strips EVERY
    /// leading `@` — so `@@@…@s-b` passed as `s-b` and the row then kept the
    /// whole token. `to` is one whitespace-free token on a request line that
    /// runs to 64 KiB, it is the field `caller_sized_bytes` declines to fold
    /// because it is "grammar-bounded to tens of bytes", and it is the ONE
    /// caller-typed field `retire_post` does not clear — so `RETIRED_KEEP` held
    /// 512 of them for the life of the session, against a `RETIRED_KEEP` doc
    /// that calls the survivor "a principal". Same shape as the `via` wound one
    /// field over: a bound stated in a doc and enforced by a grammar that did
    /// not enforce it.
    #[test]
    fn an_address_carries_one_at_sign_and_not_a_run_of_them() {
        let store = new_store();
        let (_sid, ctx) = registered(&store);
        // The address forms the `post` verb row publishes.
        assert_eq!(cmd_post(&ctx, "to=@s-abc kind=note hi", None), "OK 1\n");
        assert_eq!(
            cmd_post(&ctx, "to=@s-abc@n-lab kind=note hi", None),
            "OK 2\n"
        );
        assert_eq!(cmd_post(&ctx, "to=h-andrew kind=note hi", None), "OK 3\n");
        assert_eq!(cmd_post(&ctx, "to=say kind=note hi", None), "OK 4\n");
        // And nothing else. Padding is refused rather than trimmed: it is not
        // in the grammar, and trimming is what let it be stored.
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

    /// THE DOOR MEASURES THE ROW THE FOLD WILL MEASURE.
    ///
    /// [`OUTBOX_BYTES_MAX`] has two ends and they computed different numbers.
    /// `queued_load` folds `body + via` over the STORED rows; the admission test
    /// added the raw frame's `body.len` and nothing else. So the one row whose
    /// admission was being decided was the one row whose `via` went uncounted,
    /// and a frame of non-UTF-8 was weighed at its wire length while the row
    /// keeps `String::from_utf8_lossy`'s — three bytes for every invalid one.
    /// Both ends call [`caller_sized_bytes`] now.
    #[test]
    fn the_door_measures_the_row_the_fold_will_measure() {
        let store = new_store();
        let (_sid, ctx) = registered(&store);
        // Fill to exactly one BODY_MAX short of the bound.
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

        // `via` is part of what the row will keep, so it is part of what the
        // door weighs: body + via crosses the remaining BODY_MAX by ten bytes.
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

        // And the row keeps the LOSSY body: 100 KiB of invalid bytes is a
        // 300 KiB row, which does not fit the remaining 256 KiB.
        assert!(
            cmd_post(
                &ctx,
                &format!("to=@s-abc kind=note len={}", 100 * 1024),
                Some(vec![0xff; 100 * 1024])
            )
            .starts_with("ERR outbox full"),
            "a frame of non-UTF-8 is weighed as the row it becomes"
        );

        // The bound is not merely closed: the same body without the chain fits.
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

    /// **A ROW THE RING HOLDS IS A DUPLICATE PAST THE DEDUP WINDOW, AND A
    /// BROADCAST THE QUOTA REFUSED IS COUNTED ONCE.** A bridge that starts
    /// rewinds each topic to the oldest broadcast its session has not handled
    /// and walks it again (aterm-link's `Bridge::load_persisted_topics`) — on a
    /// ring that, after a restart of the bridge alone, still holds those rows,
    /// however many deliveries ago it took them. So a row the ring holds
    /// answers the id it got even once [`DEDUP_CAP`] newer offsets have pushed
    /// its offset out of the window, and a broadcast the quota refused answers
    /// `OK 0` (taken once, held no more) instead of being refused and counted
    /// again. NEGATIVE CONTROLS: an offset neither the ring nor the window
    /// holds is a new row, and an ADDRESSED refusal is still re-judged — its
    /// sender was told, and the addressed refill exists to offer it again.
    ///
    /// FAILS WITHOUT THE FIX: the old human row comes back as a second row,
    /// and the re-offered refusal moves `dropped=` to 2.
    #[test]
    fn a_walk_again_neither_duplicates_a_held_row_nor_counts_a_refusal_twice() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        assert_eq!(
            ctx.fabric.topic_add("news", "head", &ctx.timeline),
            Ok(true)
        );
        assert_eq!(deliver(&store, &sid, 1, "h-a", "task", "keep"), "OK 1\n");
        // A person's row outlives every agent's in the ring ([`evict_for`]),
        // so DEDUP_CAP agent rows from distinct peers (one quota each) push
        // its offset out of the window while the ring keeps it.
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

        // SENDER_QUOTA broadcasts from one peer, then one more: refused.
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
        // An addressed refusal is re-judged, as the addressed refill needs.
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

    /// **A BRIDGE SAYS IN ITS `hello` WHETHER IT REFILLS BROADCASTS, AND EACH
    /// BROADCAST ROW CARRIES WHAT ITS DELIVERING BRIDGE SAID.** A seamless
    /// update counts an unhandled broadcast as lost only when no bridge offers
    /// it again ([`FabricCarry::broadcasts_refilled`]), and only the bridge
    /// knows whether it will. NEGATIVE CONTROLS: before any `hello`, after a
    /// `hello` with no `refill=`, and with a `refill=` that names only the
    /// addressed lane, a broadcast row is not marked; an addressed row never is.
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

    /// THE BUS MIRROR IS ACKNOWLEDGED ONLY TO A RECORDED HELLO:
    /// `log=turns` is echoed to the lane that owns the
    /// link and recorded there; a ghost lane's hello is answered `OK stale=1`
    /// without it and records nothing; `fabric status`'s tail is empty until
    /// the owning hello asked, and `link mirror` is the owner's alone. Its
    /// `budget` state (lane 6) is said as `mirror=budget`; NEGATIVE CONTROL,
    /// run: an endpoint without the word answers it `ERR usage`.
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
            // Lane 5: the node at its window budget on a retaining broker, a
            // wait the next `on` ends.
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
            // A new incarnation has said nothing about the mirror yet.
            let next = next_bridge_generation();
            attach_up(&store, next);
            assert!(!bridge_mirrors_logs());
            assert_eq!(fabric_mirror_facts(), "");
        });
    }

    /// AN `auto` HELLO ASKS FOR NOTHING AND IS SAID:
    /// `log=auto` is recorded, never acknowledged with `log=turns`, so
    /// nothing serves `log-take` to it; `fabric status` says `bridge_log=auto`
    /// and, once the bridge reports a broker that retains nothing,
    /// `mirror=unretained`; the second hello an attach to a retaining broker
    /// sends (`log=turns`) is the mirror the default runs. NEGATIVE CONTROL,
    /// run: `auto` read as `turns` fails it.
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

    /// TORN STATE FILES SET ASIDE ARE THE OWNING BRIDGE'S TO SAY (lane 4): the
    /// lane that owns the link hands its count to the registry, once per line;
    /// a ghost lane's and an unowned line's are `OK stale=1` and hand nothing
    /// over; a count that is not a positive number, or a word after it, is a
    /// usage error.
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

            // Attempt the real replacement at the registry boundary. The
            // ownership check must remain protected through publication.
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

    /// A SET-ASIDE SAYS WHETHER ITS BRIDGE WILL TELL WHEN THE QUARANTINE IS
    /// EMPTY (sim item S6, the review's finding on the entry's words): the
    /// lane the owning bridge's recorded `hello` named, at
    /// [`aterm_types::control_verbs::LINK_QUARANTINE_LANE`] or later, is one
    /// that says `link quarantine`; a lane-4 bridge (a `[fabric] command` that
    /// names an older aterm) never does, and neither does a bridge that said
    /// no `hello` — so the entry it raises must not promise to end on its own.
    /// A new attach forgets the last bridge's lane.
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

    /// WHAT THE QUARANTINE STILL HOLDS IS THE OWNING BRIDGE'S TO SAY (lane 5):
    /// the lane that owns the link hands every count to the registry, 0
    /// included — the count that ends the owner's entry; a ghost lane's and
    /// an unowned line's are `OK stale=1` and hand nothing over; a count that
    /// is not a number, a word for it other than `torn=`, or a word after it,
    /// is a usage error. The two lines are apart: a count held is never one
    /// set aside.
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

    /// **A BROADCAST REACHES ONLY A SESSION THAT HOLDS ITS TOPIC, AND A `drop`
    /// TAKES EFFECT HERE THE MOMENT IT ANSWERS.** The endpoint holds the
    /// opt-in set; the bridge's copy lags a drop (the rest of a catch-up page,
    /// a session past the watch cap, a failed `topic ls`), so `deliver`
    /// checks the set itself. A refusal is not a loss — the session said no —
    /// so `dropped=` does not move, and the offset is not remembered: a later
    /// `topic add since=@<off>` still replays it. NEGATIVE CONTROLS: the same
    /// row is taken while the topic is held, a redelivery of a taken offset
    /// still answers its id after the drop (the offset decides first), and an
    /// addressed row is untouched by the set.
    ///
    /// FAILS WITHOUT THE FIX: the first and the post-drop broadcast both
    /// answer `OK` and land in the ring.
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
        // The refused offset was never remembered: re-adding from it replays it.
        assert_eq!(ctx.fabric.topic_add("news", "@13", &ctx.timeline), Ok(true));
        assert_eq!(
            broadcast(&store, &sid, 13, "news", "after-the-drop"),
            "OK 3\n"
        );
    }

    /// TWO WATERMARKS, and the difference is the whole ergonomics of the drain: a
    /// bare `inbox` moves the LISTED mark (the agent has now had its chance to
    /// read the row, so it stops holding the ring hostage), `inbox seen` moves the
    /// HANDLED mark (`seen=`, the durable one, which `topic ls` reports too), and
    /// `--peek` moves neither — which is what lets a hook print the header into a
    /// prompt without consuming mail.
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
        // So does every read of the topic set: the bridge's backstop for an
        // `inbox seen` it never heard.
        assert!(cmd_topic(&ctx, "ls").starts_with("OK 0 seen=2\n"));
        // `since=` selects, `<n>` keeps the newest, and what a bounded reply did
        // not carry is `pending=`.
        assert_eq!(rows(&cmd_inbox(&ctx, "since=2 --peek")).len(), 1);
        let one = cmd_inbox(&ctx, "1 --peek");
        assert!(one.starts_with("OK 1 "), "{one}");
        assert!(rows(&one)[0].starts_with("msg 3 "), "newest kept: {one}");
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

    /// THE SESSION LOG'S ONE TYPED HOOK:
    /// each fabric site hands its record's facts — ids, words and offsets,
    /// never a body — to `record_fact` in the record's own call. A bridge
    /// verb's wake follows exactly when the hook owes one, once per record;
    /// the session's own verbs (`post`, `inbox seen`) ring its watchers with
    /// the record itself. Every field of a record's row is its fact's (6F-6):
    /// a repeated `inbox seen` is a fact too, `moved: false`. NEGATIVE
    /// CONTROLS: a redelivered offset, a landing reported again and a hold
    /// that moves nothing hand no fact and wake nobody.
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
        // A topic's moves (6F-5), a repeat add and a drop of nothing moving
        // nothing.
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
        // One bridge-verb wake per fact it owes: the delivery's, the landing's
        // and the two holds'.
        assert_eq!(woke.get(), 4, "{facts:?}");
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
    /// decoration, and the point of keeping the FLEET hold behind the bridge is
    /// that the halted party is not the one who lifts it.
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
            // `pointer` sits here with `mouse`, and once spent a rung outside the
            // halt on the false premise that it "synthesizes no mouse event".
            // `pointer move` drives `App::on_cursor_moved` — winit's own
            // `CursorMoved` entry point — and under DEC 1000/1002/1003 that motion
            // is reported straight to the program on the PTY. A halted driver could
            // steer a mouse-reporting TUI with it.
            //
            // The verb table's check passes with `pointer` on EITHER side: it proves
            // every mutating row DECLARED an input class, not that the class is true.
            // Whether a verb's effect is observable by the program on the far side of
            // the PTY is no fact the table can check, so this row is the assertion.
            "pointer",
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
        // (`tab`, which retires a session, is refused on the App lane too:
        // `control_tests`' aimed-App-verb test.) A halt is still no App
        // freeze: `hover` types and retires nothing.
        assert!(app_halt_refusal(&store, "hover").is_none());
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
            // residual it leaves is argued at its verb row.
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
        // A `%` that starts no escape is a literal percent, kept as its own
        // escape (the review of 6F-6), so every `%` of a reason starts one.
        assert_eq!(reason_token("100%sure"), "100%25sure");
        assert_eq!(reason_token("a%g"), "a%25g");
        // An escape the input ends inside was cut short where the reason was
        // bounded, and goes, as a carried row's parse always dropped it.
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

    /// THE DEFAULT SINCE 2026-09-29 — A LOST BRIDGE HALTS NOTHING (the owner:
    /// "stop adding blockers and limits"): every session it governed keeps
    /// running — no verb refused — with an ADVISORY on it
    /// ([`SessionFabric::hold_advisory`], `status hold_advisory=fabric-lost`)
    /// and a `hold-advisory` timeline record, while the instance reports
    /// `fabric=disconnected`. A HOLD THAT STANDS — a person's, here through
    /// the fleet — is untouched and stays a real halt, with no advisory over
    /// it. The advisory speaks for the lost incarnation only: once a newer
    /// bridge attaches it is gone. NEGATIVE CONTROLS: a session the bridge
    /// never governed gets nothing; the knob's sweep (`[fabric]
    /// hold_on_bridge_lost = true`) is `control_tests`'
    /// `dropping_the_bridge_guard_halts_the_governed_sessions_inbox_hold`'s.
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
            // A person's halt, through the fleet, before the loss.
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
            // The person's halt stands, and says more than an advisory would.
            assert_eq!(
                halt_refusal(&held_ctx, "turn").as_deref(),
                Some("ERR halted reason=person origin=fleet\n")
            );
            assert!(held_ctx.fabric.hold_advisory().is_none());
            // Never governed: nothing at all.
            assert!(halt_refusal(&untouched_ctx, "turn").is_none());
            assert!(untouched_ctx.fabric.hold_advisory().is_none());
            // A newer bridge governs it again: the advisory is gone.
            let next = next_bridge_generation();
            attach_up(&store, next);
            assert!(ctx.fabric.hold_advisory().is_none());
            assert!(halt_refusal(&ctx, "turn").is_none());
        });
    }

    /// `link bye <word>` IS COUNTED FOR THE BRIDGE THAT OWNS THE LINK ALONE —
    /// a ghost lane's or an unscoped one's ack speaks for no bridge that can
    /// still withdraw anything, so it releases no waiting quit — and only a
    /// word in the grammar is one.
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

    /// THE LINK IS THE BRIDGE'S TO REPORT. An attach is `stalled`
    /// with `reason=starting` and no numbers; `link up rtt=` is the ONLY way to
    /// `connected` and stamps the ack; `link down reason=` is `stalled` with
    /// that reason, keeps the last rtt as history, and a `post --wait` under it
    /// answers `ERR fabric stalled id=<n> queued=1` AT ONCE — measured here
    /// against a ten-minute wait — rather than after the wait a landing could
    /// not end.
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
            // A fresh ack, and the minute only a hang detector's: an age read
            // off a wrong origin is far older, and a stall of this thread is not.
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
            // Connected and nothing lands: the wait runs its course.
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
            // The longest `--wait` there is (`WAIT_MAX_MS`): a wait that is not
            // refused sits it out, so the minute's hang detector tells the two
            // apart however loaded the machine.
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

            // The grammar, and the reason token's shape.
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

    /// A WEDGED HELPER IS `stale`, AND `stale` IS NOT THE ACK'S AGE.
    ///
    /// The third failure of a bridge, and the one neither `stalled` nor
    /// `disconnected` can see: the helper process holds both inherited fds open
    /// — so no `BridgeLostGuard` fires — and publishes nothing — so it never
    /// reaches its own 5 s ack deadline and never reports `link down`. Before
    /// its instance read `connected` for as long as it hung, and a
    /// `post --wait` from its sessions sat out the whole `--wait` (up to
    /// `WAIT_MAX_MS` = 600 s) for a landing nothing was going to report.
    ///
    /// AND THE HALF THAT IS EASY TO GET WRONG: a quiet link is NOT stale. The
    /// ack's age grows without bound on a healthy idle fleet by construction
    /// ([`LinkReport`]), and the owner's live instance was measured at
    /// `fabric_link_age_ms=297547` — 297 s — against a broker answering a head
    /// query in 20 ms (2026-09-20). Reading THAT as `stale` would refuse the
    /// wait on every `ask` and `task` on the fleet at once, which is the
    /// regression [`link_evidence`] exists to undo. So the clock runs from the
    /// work, not from the ack: [`STALE_AFTER`].
    #[test]
    fn a_bridge_that_owes_an_answer_reads_stale_and_a_quiet_one_does_not() {
        with_link(|| {
            let store = new_store();
            let (_sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            bridge_attached(generation);
            assert_eq!(cmd_link(&store, "up rtt=3", Some(generation)), "OK\n");
            assert_eq!(fabric_state(), "connected");

            // A QUIET LINK IS NOT STALE, however old its ack gets. Nothing has
            // been asked of this bridge, so there is nothing it owes.
            //
            // THE PRECONDITION IS ESTABLISHED, NOT ASSUMED, and that is about
            // the harness rather than the rule. `LINK` is process-global while
            // each test owns its own `Store`, and `cmd_post` arms the debt for
            // whatever instance is connected — so a test running OUTSIDE this
            // section, posting while this section has the link up, can arm it.
            // `settle_work_owed` reconciles against THIS store, which has no
            // posts, so the debt is genuinely clear; and because
            // `backdate_work_owed` only ages a stamp that already exists, an arm
            // that lands after this line is a FRESH one and still reads
            // `connected`. Without it this assertion was flaky, which is a
            // harness fact and not a defect in the rule it states.
            settle_work_owed(&store.as_store());
            backdate_work_owed(stale_after() * 100);
            assert_eq!(
                fabric_state(),
                "connected",
                "an idle bridge owes nothing: no work, no debt, no `stale`"
            );
            assert!(!wait_is_futile(), "a quiet link must still take a wait");

            // NOW HAND IT WORK. The debt is armed but fresh, so the state does
            // not move and a wait is still worth taking.
            assert_eq!(cmd_post(&ctx, "to=h-andrew kind=note one", None), "OK 1\n");
            assert_eq!(
                fabric_state(),
                "connected",
                "a fresh debt is not a stale one"
            );
            assert!(!wait_is_futile());

            // AND LET IT GO UNANSWERED. No report, no landing, no ack — only
            // time passing with the post still owed: the production horizon,
            // which `stale_after` keeps with no seam set.
            backdate_work_owed(STALE_AFTER);
            assert_eq!(fabric_state(), "stale");
            assert!(wait_is_futile());
            // The longest `--wait` there is (`WAIT_MAX_MS`), against a minute's
            // hang detector, as for `stalled` above.
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

            // THE DEBT IS SETTLED BY EVIDENCE, NOT BY TIME. One `link up` and
            // the instance is connected again with both posts still queued.
            assert_eq!(cmd_link(&store, "up rtt=4", Some(generation)), "OK\n");
            assert_eq!(fabric_state(), "connected");
            assert!(!wait_is_futile());
            // (A landing settles it too, for a bridge that sends no `link`
            // record: `a_landing_re_arms_the_debt_…`.)

            // AND A FRESH BRIDGE INHERITS NO DEBT.
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

    /// **THE DEBT BELONGS TO THE OLDEST OUTSTANDING POST, NOT TO THE LAST EVENT.**
    ///
    /// the prior first cut of `stale` cleared the owed clock on ANY evidence the
    /// bridge was moving. That reintroduced the freeze it exists to cut short, one
    /// post further along: queue two, let the bridge land the first and then wedge,
    /// and the landing clears a debt the SECOND post is still owed. The instance
    /// reads `connected` for ever, and a `post --wait` already parked on the second
    /// sleeps its whole wait — up to `WAIT_MAX_MS`, 600 s — for a landing nothing
    /// is going to report.
    ///
    /// Evidence now RECONCILES against what is actually outstanding
    /// ([`settle_work_owed`]): it clears only when the outbox is empty of
    /// un-landed posts, and otherwise re-stamps, so the remaining work gets a
    /// fresh [`STALE_AFTER`] rather than an exemption.
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

            // THE FIRST LANDS. The second is still queued, so the debt must not
            // be cleared — only re-stamped onto what is left.
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

            // AND THE SECOND LANDING CLEARS IT, because nothing is left.
            assert_eq!(cmd_outbox_sent(&store, &format!("{sid} 2 off=8")), "OK\n");
            assert_eq!(fabric_state(), "connected");
            backdate_work_owed(stale_after() * 100);
            assert_eq!(
                fabric_state(),
                "connected",
                "an empty outbox owes nothing, however long ago the last ack was"
            );

            // A DEAD VERDICT SETTLES ITS POST TOO: `off=-` is a retirement, so a
            // post the bridge gave up on is not owed for ever.
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

    /// **`post --wait-ack`'s SECOND WAIT IS CUT TO THE STALE HORIZON TOO.**
    ///
    /// [`wait_landing`] caps its park so a waiter is not left asleep when the
    /// bridge stops answering; shipped that cap on the landing wait
    /// alone and claimed both. The receipt wait is the LONGER of the two — its
    /// bound is the sender's `dl=` plus a grace, up to `WAIT_MAX_MS` — so sitting
    /// it out is the more expensive half of the bug.
    #[test]
    fn the_receipt_wait_wakes_at_the_stale_horizon_rather_than_at_its_bound() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            let generation = next_bridge_generation();
            bridge_attached(generation);
            assert_eq!(cmd_link(&store, "up rtt=3", Some(generation)), "OK\n");

            // One post lands — it is the one whose receipt is being waited on —
            // and a second is queued behind it, so the bridge is in debt. Both
            // are `note`s: `ask` defaults to `--wait`, and a post that parks
            // inside the arrange step would be measuring the landing wait's cap
            // rather than the receipt wait's.
            assert_eq!(cmd_post(&ctx, "to=h-andrew kind=note one", None), "OK 1\n");
            assert_eq!(cmd_post(&ctx, "to=h-andrew kind=note two", None), "OK 2\n");
            assert_eq!(cmd_outbox_sent(&store, &format!("{sid} 1 off=7")), "OK\n");
            backdate_work_owed(stale_after() - std::time::Duration::from_millis(300));

            // The longest bound there is (`WAIT_MAX_MS`): a wait that sat it out
            // takes ten minutes, so the minute's hang detector tells it from the
            // stale horizon 300 ms out however loaded the machine.
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

    /// A PARKED FETCH LOOKS AGAIN AT THE STALE HORIZON, as the post waits do
    /// ([`park_until`]). Nothing signals its condvar when a bridge in debt
    /// simply stops answering, and a served lane's slices ask only after the
    /// caller, so without the bound the fetch sat out its whole read. Parked
    /// for the longest bound there is (`WAIT_MAX_MS`), the minute's hang
    /// detector tells the horizon 300 ms out from the bound however loaded
    /// the machine. NEGATIVE CONTROL: with nothing owed the horizon is a whole
    /// [`STALE_AFTER`] out, and a fetch with a shorter bound sits it out and
    /// answers `ERR timeout`.
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

            // A queued post puts the bridge in debt.
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

    /// A WAIT PARKED WITH NOTHING OWED LOOKS AGAIN BY THE HORIZON OF A DEBT
    /// ARMED AFTER IT PARKED ([`park_until`]). Arming the owed clock wakes no
    /// parked waiter — another session's `post` notifies only its own condvar
    /// — so a park that ran to its bound sat out a `WAIT_MAX_MS` receipt wait
    /// for an `ERR timeout` the link had made `ERR fabric stale` six seconds
    /// in. The fetch is the waiter here because it lists itself and rings its
    /// watchers under the guard it parks with: once the test holds that guard,
    /// the fetch is parked, and every look it took saw nothing owed. It takes
    /// one [`STALE_AFTER`]: no event ends the park, which is the case under
    /// test, and a backdate cannot shorten a sleep already taken. Parked for
    /// `WAIT_MAX_MS`, the minute's hang detector tells the horizon from the
    /// bound. NEGATIVE CONTROL: with no debt armed, a short read answers `ERR
    /// timeout`.
    #[test]
    fn a_wait_parked_with_nothing_owed_answers_stale_when_a_later_debt_goes_stale() {
        use std::time::Duration;
        with_link(|| {
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

            // Another session hands the bridge work, and the bridge says
            // nothing: the link goes stale with the fetch still parked.
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

    /// A REPORT FROM A GHOST LANE IS DROPPED, and a lost bridge cannot be talked
    /// back to life by a report still in flight. The generation gate that keeps a
    /// dead incarnation's guard from reporting the live one disconnected keeps
    /// its `link up` from reporting the live one's link, too — and once either
    /// lane's guard has fired, `disconnected` outranks anything the other lane
    /// still says (§11.2: either fd closing is the link lost).
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

            // A lane with no generation at all (nothing served) reports nothing.
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
                            // Historical is_some_and(mismatch) treated None as authority.
                            assert!(!generation.is_some_and(|g| g.0 != current.0));
                            state.insert("accepted", 1);
                            assert!(!model.check_invariant("ExplicitLiveOwnerOnly", &state));
                        }
                    }
                }
            }
        });
    }

    /// THE NODE LOG HEARS EVERY STORED LINK TRANSITION, ONCE, IN ORDER:
    /// an attach (`stalled`, a new incarnation), the
    /// bridge's report moving the state, its evidence moving it, and the
    /// owner's loss. A report that stores the state it found, a ghost's report
    /// and a second loss move nothing, and nothing is heard of them.
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
            // A bridge that never reports, read by its first delivery.
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

    /// A BRIDGE THAT NEVER REPORTS ITS LINK IS READ BY WHAT IT DOES.
    /// Both lanes of a 0.85 bridge attach and
    /// no `link` record ever comes; its first delivery moves the endpoint to
    /// `connected` with no round trip and the delivery as the ack, and a
    /// `post --wait` under `stalled reason=starting` PARKS (here: runs its
    /// zero-length wait to `ERR timeout`) rather than failing fast. A bridge
    /// that HAS reported `down` is not moved by a delivery — its word stands —
    /// and a wait under it fails fast as before.
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
            // Fresh; the minute is a hang detector's, as in the link-report test.
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

            // A REPORTED link is the bridge's word, whatever it delivers.
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
            // And a fresh attach (a relaunched bridge) starts over: unreported.
            let next = next_bridge_generation();
            bridge_attached(next);
            assert_eq!(fabric_state(), "stalled");
            assert_eq!(fabric_link_facts().0, LINK_STARTING);
            let delivered = on_lane(next, || deliver(&store, &sid, 5, "s-peer", "note", "third"));
            assert!(delivered.starts_with("OK"), "{delivered}");
            assert_eq!(fabric_state(), "connected");
        });
    }

    /// A PARKED WAITER HEARS A `down` REPORT, whether the link was `starting`
    /// (the dial failed) or `up` (the link stalled): the wait would otherwise
    /// sit out its whole timeout under a state that was never going to land
    /// its post.
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

    /// A LOST BRIDGE'S ADVISORY IS ONE FACT (6F-5): by default nothing is
    /// held, and the session the bridge governed is handed one
    /// `FabricFact::Advisory` naming the origin, the reason and the
    /// generation lost, beside its `hold-advisory` row.
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

    /// `hold` is governance whichever way it goes, so a session the bridge only
    /// ever LIFTED a halt on is still one it governed — and is halted again when
    /// the bridge dies. The alternative would let a bridge free a session and then
    /// be killed, leaving it running with nobody watching.
    #[test]
    fn a_session_the_bridge_only_unheld_is_still_governed() {
        with_link(|| {
            // The fail-closed sweep (`hold_on_bridge_lost = true`); the
            // default leaves it running with an advisory.
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

    /// THE FLEET HOLD STAYS THE BRIDGE'S, AND THE OWNER'S STAYS THE OWNER'S.
    /// The Owner and Bridge rules are `control_tests`'
    /// `the_local_owner_may_hold_its_session_and_never_a_fleet_hold_inbox_hold`'s,
    /// through the dispatch, and a fleet halt over the owner's stop is
    /// [`a_fleet_lift_leaves_the_owners_stop_standing`]'s. What that leaves: the
    /// Owner's explicit `origin=local` is its default spelling and its `off`
    /// with nothing standing is no refusal; the bridge's lifts move only the
    /// slot of the origin they name — its fleet lift, the default and the
    /// spelled-out one, leaves the owner's local hold standing and says so
    /// (`OK hold=1`, nothing recorded), and a lift naming `origin=local` lifts
    /// that and leaves a fleet hold standing; and a bridge `on` over a standing
    /// LOCAL hold stands beside it, the one a refused verb names and the
    /// owner's `off` cannot lift.
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

            // The FLEET's lift — the default and the spelled-out one — leaves
            // the owner's local hold standing, and says so: it is the owner's
            // stop, not the fleet's.
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
            // A lift that names the local origin is that origin's…
            assert_eq!(
                bridge_hold(&store, &format!("{sid} off origin=local")),
                "OK hold=0\n"
            );
            assert!(halt_refusal(&ctx, "turn").is_none());
            // …and leaves a FLEET hold standing.
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

            // A bridge `on` over a standing LOCAL hold stands BESIDE it: the
            // fleet's is the one named, and the owner's `off` lifts only its
            // own.
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

    /// The single-slot rule the endpoint kept until the two slots: an `on`
    /// replaced whatever stood, and an `off` lifted what stood unless it was
    /// of another origin. The NEGATIVE CONTROL for
    /// [`a_fleet_lift_leaves_the_owners_stop_standing`]: the same acts,
    /// replayed through this, end with the owner's stop gone.
    fn single_slot(standing: Option<Hold>, act: &HoldAct<'_>) -> Option<Hold> {
        match act {
            HoldAct::Set(hold) => Some(hold.clone()),
            HoldAct::Lift(origin) => standing.filter(|hold| hold.origin != *origin),
            HoldAct::LiftAll => None,
            // The old rule knew no remote slot.
            HoldAct::SetRemote(_) | HoldAct::LiftRemote(_) => standing,
        }
    }

    /// **A FLEET HALT NEVER COSTS THE OWNER'S STOP.** The owner holds a
    /// session (`origin=local`), the fleet halts it, the fleet lifts its halt
    /// — and the session is STILL HELD, by the owner's hold, with the owner's
    /// reason, on every surface: the dispatch gate, the `inbox` header,
    /// `status`'s fabric leaf, and the last `hold` event a reader of the
    /// timeline alone (`aterm fabric`, the bridge) reads. In between, both
    /// stand, and the events say so (`hold_origin=fleet,local`).
    ///
    /// NEGATIVE CONTROL, on the old rule: the same three acts replayed through
    /// the single slot ([`single_slot`]) end UNHELD — this sequence is exactly
    /// the one that silently dropped the owner's stop.
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
            // The owner lifts its own: free, and the event says the origin.
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

    /// **THE BRIDGE-LOST SWEEP NEVER COSTS THE OWNER'S STOP EITHER**
    /// (`[fabric] hold_on_bridge_lost = true`). The sweep fills the FLEET slot
    /// of every session the bridge governed; a session the owner had held
    /// keeps the owner's hold beside it, and the reconnecting bridge's lift —
    /// `hold off origin=fleet`, which `apply_fleet_halts` writes to every
    /// session it hosts — leaves it held, the owner's reason intact.
    /// NEGATIVE CONTROL: the session beside it, with no hold of the owner's,
    /// is freed by the same lift.
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
            // Both governed by the bridge; one held by its owner.
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

            // The reconnect: a bridge that finds no fleet halt lifts the
            // sweep's from every session it hosts.
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
        // FLIP UNTIL IT LATCHES, rather than flipping once and hoping the waiter
        // armed first: whenever it armed, the next flip is a change from the value
        // it captured, so this terminates without a sleep and without assuming an
        // interleaving. (`apply_hold` signals only on a REAL change, which is what
        // makes a re-flip necessary and a spin harmless.)
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
        // Usage.
        assert!(cmd_await_inbox(&ctx, &[], 5).starts_with("ERR usage"));
        assert!(cmd_await_inbox(&ctx, &["since=1", "kinds=nope"], 5).starts_with("ERR usage"));
    }

    /// **`await inbox topic=<t>` LATCHES ON THAT CHANNEL'S BROADCASTS AND
    /// NOTHING ELSE.** Naming the topic accepts every kind on it, so the wait
    /// latches on a broadcast `note` without `kinds=note`; a broadcast on
    /// another topic and an addressed row — of the default kinds, or a `note` —
    /// do not; an explicit `kinds=` still narrows it; and a topic outside the
    /// `post to=say:<t>` grammar is the usage line. NEGATIVE CONTROL: the
    /// addressed row a topic wait sleeps through latches a bare wait.
    #[test]
    fn await_inbox_topic_latches_only_on_that_topics_broadcasts() {
        let store = new_store();
        let (sid, ctx) = registered(&store);
        // The session holds both topics: a broadcast reaches only a session
        // that opted into it (`deliver` refuses one it does not hold).
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

    /// **AN ACCEPTED POST WAKES ITS WATCHERS, BEFORE ITS `--wait` PARKS; A
    /// REFUSED ONE NEVER.** The post's `post` record rings the session's
    /// watchers, and the bridge's push lane is one of them: `EVENT <sid> post`
    /// is what makes it drain the outbox. A wake that came after the wait
    /// would come after the landing the wait is waiting for, so the parked
    /// post here wakes its watcher while it is still parked, and only then is
    /// its landing reported.
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
            // Inside the wait's own life: an unanswered post turns the link
            // `stale` at `stale_after`, which ends a parked wait — so a wake that
            // came only after the wait would come only after that.
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

    /// **`inbox seen` REPORTS THE ADDRESSED LANE'S REFILL FLOOR, AND NO
    /// BROADCAST EVER MOVES IT.** The bridge persists `floor - 1` and refills
    /// the session's addressed lane from it when a seamless update keeps the
    /// sid but not the ring, so the floor must never pass an addressed row the
    /// agent has not handled, nor stay below one it has. Broadcasts share the
    /// id counter but arrive on another subscription and through the topic
    /// catch-up, so their ids are out of offset order with the addressed
    /// lane's:
    ///
    /// * LOSS: a live broadcast at 1100 is numbered before an addressed task
    ///   at 900 still in flight; acknowledging the broadcast must keep the
    ///   floor at 900, not lift it to 1100 past the unhandled task.
    /// * DUPLICATE: a catch-up broadcast at 1200 is numbered after a handled
    ///   task at 2000; acknowledging it must put the floor past 2000, not at
    ///   1201, which would offer the handled task again after the update.
    ///
    /// FAILS WITHOUT THE FIX: the event carried only the named row's `off=`
    /// (1100 and 1200 here), which the bridge persisted as the floor.
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

    /// **A DELIVERY WAKES A `mail` WATCH.** The row's `inbox` record rings the
    /// session's watchers under the inbox guard, so the push lane drains the
    /// row on the wake its delivery causes, never on a liveness tick.
    /// NEGATIVE CONTROL: a redelivery of the same offset is answered from the
    /// dedup and records nothing, so it wakes nobody.
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

    /// **`inbox seen` WAKES ITS WATCHERS WITH ITS `inbox-seen` ROW; A FORM
    /// THAT RECORDS NOTHING NEVER DOES.** The bridge persists the refill floor
    /// and drains a receipt on that event, and the push lane is the only road
    /// it takes — the bridge has no idle poll. The record rings the session's
    /// watchers, so a woken pass that reads the timeline finds the row.
    ///
    /// NEGATIVE CONTROL: the refused forms and the reads record nothing and
    /// wake nobody.
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

    /// **A PARKED `inbox get @<off>` WAKES ITS WATCHERS WITH ITS `fetch` ROW,
    /// BEFORE IT PARKS; ONE ANSWERED FROM THE RING NEVER DOES.** The `fetch`
    /// row is the bridge's prompt to read the bus; the call parks until that
    /// read is answered, so a wake that came after the park would come only
    /// after the very answer it was to start — here, after the read's own 10 s
    /// bound, as `ERR timeout`. The test answers the read only once the wake
    /// has arrived and the outbox lists the fetch.
    ///
    /// NEGATIVE CONTROL: the ring read records nothing and wakes nobody.
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

    /// **`post key=` IS BOUNDED AT THE DOOR, RIDES EVERY `outbox` DRAIN, AND A
    /// `dup=1` RETIREMENT REACHES THE PARKED WAIT AS `OK <id> off=<n> dup=1`.**
    ///
    /// The endpoint's half of exactly-once (R7): it never dedups itself — the
    /// bridge maps the key to a durably reserved producer sequence and the
    /// BROKER answers `deduped` with the original offset — so what the endpoint
    /// owes is to carry the key to the bridge on every peek (a relaunched bridge
    /// reads the same key from the same row), to refuse a key that could split a
    /// wire line or a state-dir line, and to report the bridge's verdict without
    /// losing it to a retried retirement.
    #[test]
    fn post_key_is_bounded_rides_the_outbox_and_a_dup_retirement_reaches_the_wait() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            // THE GRAMMAR: ids' charset, one to KEY_MAX bytes.
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

            // CARRIED: on the outbox line (a peek, so on every drain) and on the
            // in-flight `post` row an agent reads at turn start.
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
            // A post without a key carries none.
            assert_eq!(cmd_post(&ctx, "to=@s-b kind=note plain", None), "OK 2\n");
            let (_, both) = parse_outbox(&cmd_outbox(&store, ""));
            assert!(!both[1].0.contains("key="), "{:?}", both[1].0);

            // THE VERDICT, AND ITS GRAMMAR. `dup=1` names the original record's
            // offset, so it cannot ride the dead form.
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
            // STICKY: a retirement retried without the flag does not un-dup it.
            assert_eq!(cmd_outbox_sent(&store, &format!("{sid} 1 off=42")), "OK\n");
            assert!(ctx.fabric.lock().posts.iter().any(|p| p.id == 1 && p.dup));

            // THE PARKED WAIT hears it. `--wait` is on by default for a task.
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
        assert!(cmd_post(&ctx, "to=fleet kind=control halt", None).starts_with("ERR denied"));
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
        // `--` ENDS THE OPTIONS: text the sender did not write cannot pick its own
        // recipient, kind, correlation id, key or frame — after `--` it is body.
        assert_eq!(post_body_offset("to=h-a kind=note -- to=@s-x"), Some(20));
        assert_eq!(post_body_offset("to=h-a kind=note --"), None);
        assert!(cmd_post(&ctx, "to=h-a kind=note --", None).starts_with("ERR usage"));
        // Over the inline cap, and the frame form.
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
            bridge_hold(&store, &format!("{sid} on reason=main%20broken"));

            let events: Vec<(String, String)> = ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .since(None)
                .map(|e| (e.kind.to_string(), e.payload.to_string()))
                .collect();
            // The lifecycle kinds a registered session already records (`spawned`)
            // are not this test's subject; the FABRIC kinds this scenario writes
            // are, in order (`topic` is written by `cmd_topic`, not here).
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
            // A DECIDED row's event carries the verdict and the two tokens a
            // receipt is addressed by — the same `from=`/`kind=` the `inbox`
            // event already put on the digest, never a body.
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

    /// **A VERDICT IS RECORDED ONCE PER ROW PER CHANGE, ON THE ROW THE ID
    /// NAMES, AND THE BARE FORM RECORDS NONE (R8).** The `inbox-seen` event is
    /// what a `--receipts` bridge acks from, so what it carries is the whole
    /// receipt policy: a bare `inbox seen 3` moves the watermark over rows 1–3
    /// and names no verdict for any of them; `inbox seen 3 handled` names one
    /// for row 3 alone; the same word again records the plain event; a
    /// changed word is a new one; and a `--peek` records nothing at all.
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
                    // An older row acknowledged out of order: its own id and
                    // offset, and the watermark where it was.
                    "2 off=11 floor=13 seen=3 verdict=deferred kind=ask from=s-peer@n-lab",
                    "2 off=11 floor=13 seen=3 verdict=handled kind=ask from=s-peer@n-lab",
                ],
                "one verdict per row per CHANGE; the bare form and a repeat carry none"
            );
        });
    }

    /// **AN `ack` ROW CARRIES THE RECIPIENT'S VERDICT, AND `await inbox re=<off>`
    /// LATCHES ON ANY REPLY TO THAT OFFSET.** The receipt arrives as an ordinary
    /// row, resolved to `re-id=` against this session's own post; `verdict=` is
    /// a closed word (`deliver` refuses a fourth); and a wait named by offset
    /// wakes on it — and on an `answer` or an `expired` naming the same offset —
    /// without a `since=`, while a wait on another offset sleeps through it.
    #[test]
    fn an_ack_row_carries_its_verdict_and_await_inbox_re_latches_on_it() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            // A note queues without waiting; landing it gives post 1 an offset,
            // which is what a later `re=70` resolves to `re-id=1`. (The kind of
            // the sent post does not change how a reply's `re=` is read.)
            assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note do it", None), "OK 1\n");
            assert_eq!(
                cmd_deliver(&store, &format!("{sid} landed=1 off=70")),
                "OK\n"
            );
            // A verdict outside the three words is refused at the door.
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
            // `await inbox re=` is monotone like the rest: the row that latched it
            // does not latch a wait that starts above it.
            assert_eq!(
                cmd_await_inbox(&ctx, &["re=70", "since=1"], 100),
                "OK timeout\n"
            );
            assert!(cmd_await_inbox(&ctx, &["kinds=ack"], 100).starts_with("ERR usage"));
        });
    }

    /// **`post --wait-ack` RETURNS THE RECIPIENT'S VERDICT, OR THE EXPIRY, AND
    /// IS BOUNDED BY `dl=` WHEN GIVEN BARE.** The landing is waited for first
    /// (the receipt names the offset); the parked wait is released by an `ack`
    /// naming it — `OK <id> off=<n> ack=<verdict> msg=<row>` — or by the
    /// asker's own bridge's `expired`; a timeout names the offset, because the
    /// post DID land; and a `note` may not ask for one.
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
            // Acked.
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
            // Expired before any ack: the verdict, not a timeout.
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
            // Bare `--wait-ack` is bounded by `dl=` PLUS THE GRACE the asker's
            // bridge needs to notice, check the bus and deliver its `expired`:
            // nobody answers here, and the wait ends then, naming the offset
            // the post landed at. The poster's thread waits a 1 ms grace in
            // place of the real one.
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
            // THE CASE THE GRACE EXISTS FOR: the asker's bridge records `expired`
            // AFTER `dl=` has passed — its sweep arms that deadline and the
            // verdict still has to be delivered back — and the wait answers the
            // verdict, not `ERR timeout`. (Reviewed: with the bound at `dl=`
            // alone this answered `ERR timeout` every time.)
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
            // The scenario is the delay: the verdict lands well past `dl=100`
            // (twice it), inside the shipped grace.
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
            // An explicit `--wait-ack=<ms>` is the caller's own bound, kept
            // exactly: no grace is added to it. The poster's grace is the longest
            // bound there is (`WAIT_MAX_MS`), so a bound that took it would run
            // ten minutes and the minute's hang detector below tells it from the
            // 200 ms the caller asked for, however loaded the machine.
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

    /// **A DECIDED `ask`/`task` OWES A RECEIPT, AND THE ENDPOINT HOLDS IT ON THE
    /// `outbox` PEEK UNTIL THE BRIDGE RETIRES IT (R8).** Reviewed defect: the
    /// receipt rode the `inbox-seen` event alone, so a verdict given while the
    /// broker was down or the bridge was being replaced never reached the
    /// sender, and — the endpoint recording a word once per row per change —
    /// the recipient could not resend it. Now it is QUEUED: listed on every
    /// drain (a peek), in order, with a per-session `rid`; a note, a bare
    /// `inbox seen` and a repeated word owe nothing; `deliver <sid>
    /// receipt=<rid> off=<n|->` retires one, idempotently; and the queue is
    /// bounded, oldest out.
    #[test]
    fn a_decided_ask_owes_a_receipt_on_the_outbox_until_the_bridge_retires_it() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            deliver(&store, &sid, 10, "h-andrew", "task", "one");
            deliver(&store, &sid, 11, "s-peer@n-lab", "ask", "two");
            deliver(&store, &sid, 12, "s-peer@n-lab", "note", "three");
            // The `receipt` lines of one drain, read by the FRAMING — a post's
            // body is `len=` raw bytes with no newline of its own, so a line
            // scan would glue the next line onto it.
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
            // RETIRED by the bridge, idempotently, and only by a well-formed line.
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
            // THE RECEIPT LINES RIDE AFTER EVERY POST, like the `fetch` lines, so
            // a parser from before them still drains every post.
            assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note after", None), "OK 1\n");
            let reply = cmd_outbox(&store, "");
            let post_at = reply.find("post sid=").expect("the post");
            let receipt_at = reply.find("receipt sid=").expect("the receipt");
            assert!(post_at < receipt_at, "{reply}");
            // BOUNDED, oldest out.
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

    /// **`inbox get @<off>` ANSWERS FROM THE RING WHILE IT CAN, PARKS FOR THE
    /// BRIDGE WHEN THE RING LET THE RECORD GO, AND THE HEADER NAMES THE OLDEST
    /// OFFSET THE LOG STILL HOLDS FOR THIS SESSION (E3).** 513 deliveries evict
    /// the first row (`dropped=1`); its offset is still `oldest_on_bus=`, the
    /// read for it rides the `outbox` peek as `fetch sid= off=`, the bridge's
    /// `deliver … fetched=` fills it with the full body and touches no ring,
    /// and a record the bridge cannot find is `ERR no such record`.
    #[test]
    fn inbox_get_at_an_offset_answers_the_ring_or_the_bus_and_the_header_names_the_oldest() {
        with_link(|| {
            let store = new_store();
            let (sid, ctx) = registered(&store);
            attach_up(&store, next_bridge_generation());
            // Park a fetch, then wait — bounded — for it to ride the `outbox`
            // peek, so a record that answers from the ring instead of parking
            // fails here loudly rather than spinning this poll forever.
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
            // Fill past the 512 cap by eight, so the eight oldest offsets
            // (1000..1007) are evicted from the ring while the bus keeps them.
            // List every 60 so the per-peer quota (64 unlisted) never refuses one.
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
            // From the ring: the newest row, by offset, with its fields on the tail.
            assert_eq!(
                cmd_inbox_get(&ctx, &format!("@{newest}")),
                format!("OK 9 off={newest} from=s-peer@n-lab kind=note trust=agent\nbody-{newest}")
            );
            // Evicted: parks, and the peek lists the read for the bridge.
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
            // Answered once, answered again without a second parked read; and no
            // row was appended for it.
            assert_eq!(
                cmd_inbox_get(&ctx, "@1000"),
                "OK 9 off=1000 from=s-peer@n-lab kind=note trust=agent\nbody-1000"
            );
            assert!(!cmd_outbox(&store, "").contains("fetch "));
            assert_eq!(cmd_inbox_get(&ctx, "1"), "ERR no such message\n");
            // The bridge found nothing at that offset on this session's lane.
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
            // Below what the broker's retention keeps of this session's lane:
            // its floor, never `no such
            // record`. NEGATIVE CONTROL, run: `floor=` read past.
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

            // IN CHUNKS: a body that does not fit one control line comes back
            // whole, `at=` placing each piece and `more=1` on all but the last —
            // and a chunk out of place is refused AND fails the read at once.
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
            // A BRIDGE-SIDE CUT is said, as `inbox get <id>` says it — here a
            // record over BODY_MAX: `len=` is the log's size and may exceed the
            // bound on this form (the carried body may not), so it reads
            // `truncated=1 len=` instead of a refusal nothing ever answers.
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
            // AND A ROW THE DELIVERY CUT IS NOT ANSWERED FROM THE RING (E3:
            // "`truncated=1` becomes fetchable"): it parks for the bridge, which
            // answers it whole.
            // List everything first, so the per-peer quota has room.
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
            // A cut row the bus CANNOT complete still answers what the ring
            // holds, marked cut — never an error in place of the prefix.
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

    /// **AFTER A LOG SWITCH THE REFILL FLOOR IS THE NEW LOG'S** (r2-01).
    /// `inbox seen`'s `floor=` is what the bridge persists as the session's
    /// refill watermark, so an earlier log's unhandled row must not set it:
    /// its offset names another record here. On log A two rows land (7, 900)
    /// and the first is handled; on log B one lands at 950. The floor is B's
    /// row, not A's unhandled 900, and once B's row is handled it is one past
    /// it. Negative control: counted with A's rows, the floor reads 900.
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

    /// **A NEW BUS LOG IS NOT DEDUPED INTO THE OLD ONE** (r2-01). astream gives
    /// another log's records the same offsets, and the endpoint's `off=`
    /// dedup, `inbox get @<off>` and `re=` threading key on bare offsets. Told
    /// log A, a row lands at 7; a second delivery at 7 on A is the same record.
    /// Told log B, every session forgets A's offsets (`OK forgot=1`): B's
    /// record at 7 is a row of its own, `inbox get @7` answers B's, and a
    /// read parked for A's offset fails rather than being answered from B.
    /// The same identity again forgets nothing; a bridge that no longer owns
    /// the link changes nothing; anything but an identity is a usage error.
    /// Negative control: without the switch, B's record at 7 is answered with
    /// A's row id and never appended.
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
            // A read of an A offset parked for the bridge.
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
            // The process-wide link starts fresh; the real outbound carry is
            // seeded before that process's first bridge attaches.
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
                // No second `link log` runs before this first real drain.
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

    /// **A POST QUEUED ACROSS A LOG SWITCH LANDS ON THE NEW LOG, AND A REPLY TO
    /// IT STILL THREADS** (r2-01). A post's offset is fixed when it LANDS, not
    /// when it is queued: post 1 lands on log A at 70; post 2 is still queued
    /// when the bridge says the log changed, and lands on the new one — at 70
    /// too, since a new log numbers its records again. The new log's reply
    /// `re=70` answers post 2 (`re-id=2`), never post 1, whose 70 named
    /// another log's record. Negative control, run: marked by an id floor taken
    /// at the switch, post 2 counts as an earlier log's and the reply carries
    /// no `re-id=`.
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

    /// Wait, at most a minute (a hang detector), for the outbox peek to list
    /// `sid`'s parked `inbox get @<off>`.
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

    /// The newest `inbox-seen` event's payload on `ctx`'s timeline.
    fn last_inbox_seen(ctx: &SessionCtx) -> String {
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .since(None)
            .rfind(|e| e.kind == "inbox-seen")
            .map(|e| e.payload.clone())
            .unwrap_or_default()
    }

    /// **A MOVED GENERATION KEEPS WHAT NAMES A RECORD BELOW THE FORK, AND
    /// ONLY THAT.** The bridge clamps at the fork `f` below which a moved
    /// generation continues its log (a power loss under a weak tier, a
    /// poisoned log's reopen), and a lane-3 bridge tells the endpoint so:
    /// `link log <A.1> continues=20 was=<A.0>`. On A.0 the session was handed
    /// two asks, at 7 (below the fork) and 40 (above it), decided both (a
    /// receipt owed for each) and queued a reply to each while the broker was
    /// down; two of its posts landed, at 9 and 41, and a receipt wait is armed
    /// on the one at 9; two reads were answered off the bus (@3, @44) and two
    /// more are parked (@5, @45). Told the move, everything that names 3, 5,
    /// 7 and 9 STAYS — the reply `re=7` queued, receipt 1 owed on this log,
    /// `@3` answered from what the bridge said, `@5` still parked for the
    /// bridge, the wait on post 3 released by the ack the new generation
    /// carries, and the dedup at 7 (a refill handing it again is the same
    /// row) — and everything that names 40, 41, 44 and 45 GOES: the reply
    /// `re=40` retired `log-replaced`, receipt 2 marked `log=earlier`, `@44`
    /// read again, `@45` failed, post 4's wait `log-replaced`, and the new
    /// generation's record at 40 a row of its own. The span is what is kept
    /// and the refill floor resumes at the fork, where the bridge's own
    /// `seen/` does.
    ///
    /// NEGATIVE CONTROL, run: the same move told BARE — all a lane-2 endpoint
    /// is told — forgets everything, the reply, the receipt, the read and the
    /// wait below the fork included, which is what every endpoint did before
    /// lane 3.
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

                // GENERATION 0 of log A.
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
                // Armed for as long as the hang detector: only an ack or the
                // move ends it.
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

                // THE MOVE to generation 1, continuing generation 0 below 20.
                let moved = if lineage {
                    format!("{a1} continues=20 was={a0}")
                } else {
                    a1.clone()
                };
                assert_eq!(tell(&moved), "OK forgot=1\n", "{case}");
                assert_eq!(tell(&a1), "OK\n", "{case}: the move is applied once");

                // THE SPAN AND THE REFILL FLOOR, before any read below moves the
                // span: what is kept of it, and the floor at the fork.
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

                // THE READS.
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

                // THE QUEUED REPLIES AND THE OWED RECEIPTS.
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

                // THE LANDED POSTS: an ack on the new generation threads to the
                // one below the fork and releases its wait; the one above it
                // is another record's number now.
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

                // THE DEDUP: 7 is the same record on both generations, 40 is not.
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

    /// **ACROSS TWO MOVES, AN OFFSET IS KEPT ONLY BELOW BOTH FORKS** — the
    /// LOWEST of them, as astream's `LogIdentity::continues` takes the least
    /// common over every step: each move is applied once, and what one took
    /// stays taken. On A.0 post 1 lands at 40, a row is delivered at 40, and
    /// an ask at 35 is decided (a receipt owed). A.1 continues A.0 below 30:
    /// all three are another record's number now (the receipt `log=earlier`),
    /// and on A.1 post 2 lands at 40 and a new row is delivered there. A.2
    /// continues A.1 below 50, so A.1's 40 is A.2's 40 — but A.0's 40 and 35
    /// are not, because A.0 and A.2 agree only below 30. A reply at `re=40` on
    /// A.2 threads to post 2, `inbox get @40` answers A.1's row, A.1's record
    /// at 40 handed again is the row it was, and the receipt for 35 is still
    /// an earlier log's: never offered for A.2, where 35 is another record.
    /// So with two waits parked on A.0's offsets ([`OffsetWatch`]): the one
    /// on 35 ends, the one on 25 goes on.
    ///
    /// NEGATIVE CONTROL, run: a move that ASSIGNS what it keeps instead of
    /// clearing only (`r.on_log = kept(r.off)`, `p.landed_on_log = …`,
    /// `r.earlier_log = !kept(r.off)`, a watch stored `kept(w.off)` in
    /// [`Inbox::forget_bus_offsets`]) judges by the NEWEST fork alone — the
    /// mutant `Buggy == 2` of `aterm-models-session`'s
    /// `fabric_reader_resume_model` — and keeps A.0's 40 and 35 (both below
    /// 50): this test fails at the reply threading to A.0's post 1 (the
    /// posts), the row at 40 answered from A.0 (the rows), the receipt
    /// offered again at 35 (the receipts) and the wait on 35 going on (the
    /// watches), whichever one of the four is the mutant.
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
            // What two parked waits hold ([`OffsetWatch`]): A.0's 35 and 25.
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
            // A wait that ended drops its watch, and the next registration
            // prunes it: the list holds the parked waits, never the history.
            drop((watch_35, watch_25));
            let _armed = ctx.fabric.lock().watch(1, true);
            assert_eq!(ctx.fabric.lock().watches.len(), 1);
        });
    }

    /// **A SESSION AHEAD OF THE BRIDGE'S WITNESS KEEPS WHAT ITS CHAIN KEEPS**
    /// ([`LogClock::keep_below`]). A bridge told the endpoint of A.1 (fork
    /// 50) and died before it recorded the move; the log moved again, to A.2,
    /// and the next bridge — its witness still A.0 — names `was=A.0` and the
    /// fork of the whole chain, 30. The session holds A.1: its row at 10 is
    /// below 30 and is the row it was (a redelivery of 10 dedups), its row at
    /// 40 is not (a record at 40 is a new row), and a receipt at 35 is an
    /// earlier log's. NEGATIVE CONTROL, in the same test: a session carried
    /// on ANOTHER log (`bb….0`) forgets everything under the same line.
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
            // The move the dead bridge told: A.1, below 50 the same.
            assert_eq!(
                tell(format!("{} continues=50 was={}", a(1), a(0))),
                "OK forgot=1\n"
            );
            // The next bridge's line, from its own (older) witness.
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

            // The control: a session whose clock names another log.
            let other = new_store();
            let (other_sid, _other_ctx) = registered(&other);
            let other_link =
                |line: String| cmd_link(&other, &format!("log {line}"), Some(generation));
            let b0 = format!("{}.0", "bb".repeat(16));
            // The link's own clock is the instance's, one per process: moving it
            // to another log is a change whatever this store held.
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

    /// **`link log … continues=` TAKES ONLY AN EARLIER GENERATION OF THE SAME
    /// LOG**, both tokens or neither, each once, in either order — what
    /// astream's `LogIdentity::continues` ever answers for. Every other form
    /// is a usage error that changes nothing: the offset at 7 on A.0 still
    /// dedups after each. From a bridge that no longer owns the link, `OK
    /// stale=1` and nothing changes either. The control is the well-formed
    /// line, tokens reversed: it is applied (`forgot=1`), and 7 is still the
    /// row it was, below the fork.
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

    /// **A SESSION CARRIED ON THE GENERATION A MOVE CONTINUES KEEPS WHAT IS
    /// BELOW THE FORK** ([`register_on_log`], [`LogClock`]). A seamless
    /// update carries a session's clock (A.0), its owed receipts (for asks at
    /// 7 and 40) and its queued replies (`re=7`, `re=40`). The successor's
    /// bridge has already said `A.1 continues=20 was=A.0` when the adopted
    /// session registers — and has since re-told A.1 bare, as a bridge that
    /// re-attaches to the same generation does, which keeps what was said of
    /// it: the receipt at 7 and the reply `re=7` are kept for the first
    /// drain, and the ones at 40 are retired as an earlier log's.
    /// NEGATIVE CONTROLS, run: carried on another log (B.0), or carried from
    /// a build before the clock was carried (no `bus_log`), nothing is kept —
    /// the move continues A.0, not those. A bare re-tell that dropped the
    /// lineage ([`cmd_link_log`]'s `or_else(previous …)` removed) fails the
    /// A.0 case: the receipt at 7 is an earlier log's.
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
                // A bridge that re-attaches to the same generation tells it
                // BARE: that A.1 continues A.0 is still true of the pair.
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

    /// **WHAT A SESSION DECIDES AFTER THE MOVE GOES BY WHAT THE MOVE KEPT.**
    /// On A.0 a note is delivered at 3 and read (`inbox seen 1`), and asks at
    /// 7 and 40 wait unread. The bridge says `A.1 continues=20 was=A.0`, and
    /// only then does the agent look again and decide. The refill floor its
    /// bare `inbox seen` reports is the unread ask at 7, below the fork — not
    /// the fork, which would start a successor's refill past it and never
    /// offer it again. The verdict on 7 owes its receipt on A.1 (`receipt
    /// rid=1 off=7`, which settles the deadline the asker's bridge kept below
    /// the fork); the one on 40 is an earlier log's (`log=earlier`, retired by
    /// the bridge rather than put on A.1, where 40 is another record).
    ///
    /// NEGATIVE CONTROL, run: the same move told BARE (all a lane-2 endpoint
    /// is told) reports no floor and marks both receipts earlier. By hand,
    /// each failing this test: the verdict rule before lineage (any move
    /// makes a row's receipt an earlier log's), and a floor that counts only
    /// the rows delivered after the move.
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

    /// How many waits of `ctx` hold a live [`OffsetWatch`]: what says a
    /// spawned `await inbox re=` has armed before the test moves the log.
    fn armed_watches(ctx: &SessionCtx) -> usize {
        ctx.fabric
            .lock()
            .watches
            .iter()
            .filter(|w| w.strong_count() > 0)
            .count()
    }

    /// **`await inbox re=` FOLLOWS WHAT THE MOVE KEPT**, armed before it or
    /// after. On A.0 post 1 lands at 9 and post 2 at 41, a reply to post 1
    /// arrives at 15 (`re=9`, row 1), and two waits are parked: one on
    /// `re=9` past that reply (`since=1`) and one on `re=41`. The bridge says
    /// `A.1 continues=20 was=A.0`. The wait on 41 ends `ERR log-replaced
    /// re=41`: its record is gone. The wait on 9 goes on, and a wait armed
    /// AFTER the move on `re=9` latches the reply that came at 15 on A.0,
    /// whose own offset A.1 still holds — and so the record its `re=` names.
    /// An ack to post 1 then arrives on A.1, at 21, and releases the parked
    /// wait on 9.
    ///
    /// NEGATIVE CONTROL, run: the move told BARE (a lane-2 endpoint) ends
    /// both parked waits `log-replaced`, and the reply at 15 is an earlier
    /// log's, which the wait armed after the move does not latch (`OK
    /// timeout`). By hand, each failing this test: a wait ended by any move
    /// (the rule before lineage), and a latch on the rows delivered after
    /// the move alone.
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

    /// **A CHAIN OF MOVES HAS NO WINDOW.** Each move is applied once, as the
    /// bridge clamps its own state at each attach, so an offset below every
    /// fork survives however many there are, and one a move took stays taken
    /// however many keep it after. Nine moves of A, each continuing the last
    /// above 20 (a failing disk's poisoned reopens can make as many within
    /// one aterm's life): the ask at 7 delivered on A.0 is still A.9's record
    /// 7 — a verdict on it after the ninth owes its receipt there (no
    /// `log=earlier`, so no `expired` at the asker for an ask that was
    /// answered), its redelivery is the row it was, and `inbox get @7`
    /// answers it. And the note at 5 delivered on B.0 before a BARE move to
    /// A.0 stays B's: nine moves of A that keep 5 do not make it A's, so A.9's
    /// record at 5 is a row of its own, not a duplicate of it, lost.
    ///
    /// NEGATIVE CONTROL, run: the same nine moves told BARE (a lane-2
    /// endpoint) make the row at 7 an earlier log's — the receipt
    /// `log=earlier`, the redelivery a new row. By hand: a move that assigns
    /// what it keeps (the mutant
    /// [`across_two_moves_an_offset_is_kept_only_below_both_forks`] names)
    /// makes B.0's 5 A.9's, and A.9's record at 5 is answered `OK 1`.
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

    // outbox / outbox sent — the outbound mirror of deliver / inbox seen

    /// Parse one `outbox` reply into `(header, [(fields, body)])`. Written as a
    /// real parser rather than a substring check because the framing IS the
    /// claim: a length prefix, then a `len=`-terminated line and exactly that
    /// many body bytes, repeated. A reader that got the arithmetic wrong here
    /// would silently read the next post's header as body.
    fn parse_outbox(reply: &str) -> (usize, Vec<(String, String)>) {
        let (declared, posts, _) = parse_outbox_frame(reply);
        (declared, posts)
    }

    /// Whether an `outbox` reply ends with a cut walk's `more` line, read as
    /// the bridge reads it: after every post's `len=` body bytes (a body need
    /// not end in a newline, so the line may follow it directly).
    fn says_more(reply: &str) -> bool {
        parse_outbox_frame(reply).2
    }

    /// [`parse_outbox`], and whether the frame's last line is `more`.
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
            // The bodiless trailer lines (owed receipts, parked reads) ride after
            // every post and carry no `len=`.
            if line.starts_with("receipt ") || line.starts_with("fetch ") {
                rest = tail;
                continue;
            }
            // A cut walk's `more`, the frame's last line.
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
        // `re=`/`dl=`/`via=` ride the row too: the published record's body
        // carries them, and only the endpoint knows them.
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
    }

    /// `post --wait` PARKS ON EITHER VERDICT: an `off=-` retirement answers
    /// `ERR undeliverable` rather than letting the caller sit out the full
    /// timeout and learn nothing. (A landing's `OK <id> off=<n>` is
    /// `post_key_is_bounded_rides_the_outbox_and_a_dup_retirement_reaches_the_wait`'s.)
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
            // Retire it until the waiter has observed it: the retirement is
            // idempotent, so re-notifying closes the window between the row
            // existing and the waiter parking. A hang detector, not a budget.
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

    // R1 audit regressions

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
            // deterministically no bridge AND no supervisor, so those two are
            // answered `ERR fabric absent id=<n> no-bridge=1`. Either way the post
            // was ACCEPTED and queued — which is what the id in both replies names
            // — while the refusal under test is the usage line, which names no id
            // at all.
            let expected = if matches!(*kind, "ask" | "task") {
                format!("ERR fabric absent id={id} no-bridge=1\n")
            } else {
                format!("OK {id}\n")
            };
            assert_eq!(reply, expected, "`post kind={kind}` is one of the seven");
        }

        // `deliver` must still ACCEPT both verdict kinds: the receiving bridge
        // writes them.
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
    /// turn is told `pending=0` over mail it has never seen.)
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
    /// The same holds for the dead launch's own late GUARD (the R1 audit): the
    /// verb lane sees EOF at once, the push lane only on its 250 ms peer probe,
    /// and the supervisor relaunches after 200 ms, so the replacement can be
    /// CONNECTED before the old second guard unwinds — which used to store
    /// `disconnected` over the live bridge, for good.
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
            attach_up(&store, second);
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

    /// A PRUNE SPEAKS ONLY FOR THE REGISTRY THAT DROVE IT.
    ///
    /// `touched` is process-global and its prune predicate is "still in the
    /// registry" — but the only registry a bridge verb can ask is the one it was
    /// handed. While the set was one flat `BTreeSet`, a verb on registry A
    /// intersected the WHOLE set with A's live handles, so every sid registry B
    /// still owed a `bridge_lost` halt to was dropped on the floor by a message
    /// that had nothing to do with B. Nothing failed loudly: `bridge_lost` just
    /// held fewer sessions than it had promised, on the path whose entire
    /// contract is to fail closed.
    ///
    /// Production runs one registry, which is why this never showed there. The
    /// test suite runs one per test, which is why it showed here — as a prune
    /// test losing its own live sids whenever a sibling's `deliver` happened to
    /// land while the set was over [`TOUCHED_PRUNE_AT`]. The driver's own
    /// departed sids still go: the set is bounded by the live registry, not by
    /// uptime (`touched` once had no removal path at all).
    #[test]
    fn one_registrys_prune_does_not_evict_another_registrys_governed_sids() {
        with_link(|| {
            // The bystander: one live, governed session in a registry that takes
            // no further part in anything below.
            let bystander = new_store();
            let (b_sid, _b_ctx) = registered_as(&bystander, 1);
            assert_eq!(deliver(&bystander, &b_sid, 1, "h-a", "note", "x"), "OK 1\n");
            assert!(touched_contains(&b_sid), "the bystander is governed");

            // A second registry, driven past the threshold so its verbs prune.
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

            // ...and the prune is not merely inert: the driver's OWN departed
            // sessions still go, which is the property the keying must not cost.
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

    /// THE QUOTA COUNTS PEERS, WHICH IS WHAT ITS OWN DOC PROMISES.
    ///
    /// It was counted over the whole `from=` string, and half of that string is
    /// the sending NODE's word: `render_from` builds `s-<sid>@n-<node>` from the
    /// record BODY's `from=<sid>`, checking only that it LOOKS like a session
    /// principal. `is_principal` admits ~36^32 sids, so one node minted a fresh
    /// 64-row allowance whenever it liked — rotate the pseudo-sid every 64 notes
    /// and `ERR quota` never fires while the ring turns over continuously.
    ///
    /// The scenario the quota exists for: a peer floods `note`s while a human's
    /// `task` waits unread. The flood is refused at the door, so it never
    /// reaches the ring and the human's row is never the one evicted.
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
        // The flood was refused at the door, never reaching the ring: the
        // human's unread task is still there and nothing was evicted.
        let reply = cmd_inbox(&ctx, "--peek");
        assert!(reply.contains("from=h-andrew kind=task"), "{reply}");
        assert!(reply.contains(" dropped=0 "), "{reply}");
        // The quota counts UNLISTED rows: once the agent has drained, the same
        // peer may speak again.
        cmd_inbox(&ctx, "");
        assert!(
            deliver(&store, &sid, 902, "s-rogue9999@n-rogue", "note", "again").starts_with("OK ")
        );
    }

    /// AN ACK RELEASES THE SENDER'S QUOTA, which is why `inbox seen` lists the
    /// rows at or below its argument as well as advancing the handled watermark.
    ///
    /// LOAD-BEARING: a reader that lists only with `--peek` and never runs a bare
    /// `inbox` is kept reachable past its 64th message by this side of `inbox
    /// seen` alone. A change that stopped touching `listed` would reinstate that
    /// deadlock silently.
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
        // A PEEK moves nothing.
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
    /// route read three quarters of a report as the whole of it. The rest is on
    /// the bus, which the recipient cannot reach, so the answer has to SAY SO; it
    /// cannot fetch it.
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
    /// sent", the remedy for "not sent" is to send again, and a `post` without
    /// `key=` carries no idempotency key — so the peer's inbox ends up holding
    /// the same `ask` twice.
    #[test]
    fn a_post_refused_for_want_of_a_link_says_it_is_still_queued() {
        with_link(|| {
            let store = new_store();
            let (_sid, ctx) = registered(&store);

            // NO FABRIC AT ALL, which is a DIFFERENT instruction and is spelled
            // differently. `spawn_supervisor` returns false on an instance with no
            // `[fabric] command` and starts nothing, so `outbox` is never called
            // by anyone: `queued=1`'s promise ("a replacement bridge will publish
            // it") is false here, and an agent that follows the verb row literally
            // waits forever for an answer no process will fetch — 128 posts, then
            // `ERR outbox full`. `absent` alone cannot be the test, because it is
            // also the state in the seconds before the FIRST bridge attaches.
            assert_eq!(
                cmd_post(&ctx, "to=@s-peer kind=ask --wait=0 no fabric?", None),
                "ERR fabric absent id=1 no-bridge=1\n"
            );

            // The ENTRY check on a CONFIGURED instance whose first bridge has not
            // attached yet: same `absent` state, opposite advice, because one is
            // coming.
            note_bridge_supervised();
            assert_eq!(
                cmd_post(&ctx, "to=@s-peer kind=ask --wait=0 which branch?", None),
                "ERR fabric absent id=2 queued=1\n"
            );

            // (The PER-WAKE check, a waiter parked when its bridge dies, is
            // `a_parked_post_learns_the_bridge_died_rather_than_timing_out`'s.)
            // AND THE CLAIM IS TRUE: every post is still in the outbox, bodies
            // and all, for the replacement bridge to drain — including the one
            // answered `no-bridge=1`, whose body is retained exactly the same way.
            // `no-bridge=1` says nothing will COME for it, not that it was
            // dropped: that is why the token replaces `queued=1` rather than
            // claiming the post was refused.
            let drained = cmd_outbox(&store, "");
            assert!(drained.contains("no fabric?"), "{drained}");
            assert!(drained.contains("which branch?"), "{drained}");
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
        assert!(
            says_more(&drained),
            "a cut walk says so, last, so the bridge does not take it for the whole queue"
        );
        // A PEEK, so the rest is not lost — the next drain sees the same queue.
        let again = cmd_outbox(&store, "");
        assert_eq!(
            again.len(),
            drained.len(),
            "a bounded drain retires nothing"
        );
    }

    /// `more` MEANS A POST WAS LEFT UNLISTED, not that the walk reached its
    /// budget: two sessions' queues of exactly the budget between them are
    /// listed whole, the walk stops at the bound on its next turn, and there is
    /// nothing more. NEGATIVE CONTROL: one more post of the first session's,
    /// past the budget, is unlisted, and `more` is said.
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

    /// ONE SESSION'S FULL QUEUE IS NOT EVERY DRAIN: the sessions are taken in
    /// turn, so a later session's post is listed beside an earlier session's
    /// queue that alone fills the byte budget — the queue a bridge holds while
    /// the broker cannot take that session's head post now, and must not hold
    /// any other session's mail behind. Per sender the order stays the queue's.
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

    /// The heads of several refused senders can fill a whole drain too.
    /// Keep every post queued, as a transient broker refusal does: another
    /// drain must expose the later sender without retiring anyone else's head.
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
