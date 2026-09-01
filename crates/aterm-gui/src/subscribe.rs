// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real-time SUBSCRIBER REGISTRY + PUSH face (design P1.3) — the additive layer
//! that turns the poll-only control socket into a server-PUSH face, so one agent
//! watches its OWN and OTHER sessions LIVE without busy-polling.
//!
//! ## Shape
//!
//! A `subscribe` connection (already its own thread, see [`crate::control`])
//! registers a [`SubscriberHandle`] keyed by the process-local `u64` id of each
//! session it watches — the SAME id the GUI routes `Wake::Output { session }` with
//! (main.rs `Wake::Output`). The handle is a SINGLE-SLOT non-blocking notify: an
//! `mpsc::sync_channel(1)` whose `try_send` NEVER blocks. The producer side (the
//! GUI/reader thread, via the one `Wake::Output` hook) calls [`Subscribers::notify`],
//! which `try_send`s a unit to every subscriber of that session and IGNORES a full
//! channel or a hung-up receiver. The subscriber thread blocks on `recv`, then
//! reads the session's CURRENT state and emits deltas.
//!
//! ## Coalescing — backpressure-safe BY CONSTRUCTION
//!
//! The notify slot has capacity ONE and `try_send` drops on a full slot, so a
//! pending-but-unread notify simply stays pending: N producer wakes between two
//! subscriber reads collapse into AT MOST ONE pending notify. When the subscriber
//! finally wakes it reads the LATEST state (the current `content_seq` and grid),
//! not a queue of every intermediate frame. A slow subscriber
//! therefore gets COARSER / fewer deltas and CANNOT block, backpressure, or even
//! slow the producing session's reader thread — the producer's `try_send` is O(1)
//! and infallible by design (it discards rather than waits). If the subscriber's
//! own socket write blocks or fails, its thread drops the connection (and
//! deregisters) — the producer is never involved in that path.
//!
//! ## Discipline
//!
//! The registry lock is a leaf: `notify` takes it, `try_send`s, and releases —
//! it is NEVER held across a `Terminal` lock or a socket write. The subscriber
//! thread reads the registry only to (de)register itself; it resolves and reads
//! target terminals through the [`crate::session_store::Store`] with the same
//! clone-then-release discipline the rest of the control path uses.
//!
//! ## Push-only
//!
//! Once a connection issues `subscribe` and the verb authorizes, [`crate::control`]
//! FLIPS that connection to push mode: it stops reading requests and enters
//! [`push_loop`]. A subscribed connection is PUSH-ONLY for the rest of its life —
//! the client reads `DELTA`/`EVENT`/`GAP` frames and never sends another verb.

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, LockResult, Mutex, MutexGuard};
use std::time::Duration;

use aterm_core::terminal::Terminal;

use crate::cast::{ByteFanout, ByteSubscription};
use crate::control::Scope;
use crate::session_store::Store;
use crate::turn_ledger::TurnLedger;

/// The PER-TARGET streams a subscription may watch — exactly the frames the
/// per-target `ReadScreen` gate in [`crate::control`] authorizes, one resolved
/// selector at a time. Public fields and NO authority attached: a `TargetStreams`
/// value says what to emit, never for whom. That is safe only because every target
/// handed to [`push_loop`] has already passed the gate individually.
///
/// Instance-wide streams deliberately do NOT live here (see [`InstanceStreams`]):
/// the gate loops over the selectors the client named, so it can only ever speak
/// for those sessions, while an instance-scoped stream reports sessions the client
/// never named and could not have named. Keeping the two scopes in one flat bag is
/// what let the `sessions` stream ride in on a per-target authorization.
///
/// `screen`/`cursor`/`cells` ride the `content_seq` delta path; `events` rides the
/// block-complete (OSC 133 D) signal; `bytes` rides the raw output fan-out.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct TargetStreams {
    /// Emit `DELTA <sid> seq=<n> screen <changed rows>` when content advances.
    pub screen: bool,
    /// Emit `DELTA <sid> seq=<n> cursor <row> <col> <visible> <style>` on any caret
    /// change (position, DECSCUSR style, or DECTCEM visibility) — even at an
    /// unchanged seq. `<visible>` is `0|1`, matching the poll `cursor` verb.
    pub cursor: bool,
    /// The lifecycle DIGEST: `EVENT <sid> block-complete <id> exit=<code>` on
    /// OSC 133 D, `EVENT <sid> turn <id> submitted=<0|1> status=<..> dur_ms=<..>`
    /// on each completed `turn` (scanned from the session TURN LEDGER), `EVENT <sid>
    /// meta field=<f> value=<pct|->` on a `meta set`/`clear` (the session EVENT
    /// TIMELINE's `meta-change` row), `EVENT <sid> title <pct>` when the window
    /// title changes (OSC 0/2 — often the cwd/command via shell integration),
    /// `EVENT <sid> bell total=<n>` on a BEL/alert, and — as the session is
    /// retired — `EVENT <sid> closing reason=<token> by=<sid|human|->` (the exit
    /// ledger's row, read from the timeline's `closing` record by the one wire
    /// path that still holds that timeline once the sid no longer resolves)
    /// followed once by `EVENT <sid> exited`.
    /// One low-rate stream an orchestrator watches for N sessions on one fd, pulling
    /// `screen`/`image` only when an event says something happened.
    pub events: bool,
    /// Emit `DELTA <sid> seq=<n> cells <nbytes>\n<styled-json>` when content
    /// advances — the LOSSLESS styled-screen frame (Item 1's payload) pushed live,
    /// so an outer agent sees the inner TUI's colour/attrs, not plaintext.
    pub cells: bool,
    /// Emit `BYTES <sid> <len>\n<raw bytes>` for EVERY program-output burst — the
    /// live, byte-lossless, every-frame channel (Item 2). Unlike screen/cells this
    /// never coalesces: the per-subscriber queue holds every burst between wakes.
    pub bytes: bool,
    /// The `trim` MODIFIER (not a frame source): a `screen` DELTA stops after the
    /// last non-blank row — `DELTA <sid> seq=<n> screen <nrows>` with `<nrows>` the
    /// count actually sent — by the same rule as `text trim`
    /// ([`crate::control::trimmed_len`]), so a poller and a subscriber agree on the
    /// row count. It rides here rather than in [`PushOptions`] because it shapes ONE
    /// per-target frame, and it carries no authority: it only shortens a frame the
    /// gate already authorized. Inert without `screen` (as `every-frame` is without
    /// `cells`); alone it names no source, so a `trim`-only list fails closed.
    pub trim: bool,
}

impl TargetStreams {
    /// Whether any `content_seq`-driven stream (screen/cursor/cells) is requested.
    #[must_use]
    fn wants_content(self) -> bool {
        self.screen || self.cursor || self.cells
    }

    /// Whether any per-target FRAME SOURCE is requested. `trim` is a modifier and
    /// does not count — comparing against `Default` would let `trim` alone pass the
    /// "names at least one source" check and ack a stream that never emits.
    #[must_use]
    fn any_frame_source(self) -> bool {
        self.screen || self.cursor || self.events || self.cells || self.bytes
    }
}

/// What the client ASKED for at INSTANCE scope, before any authority check. Parsing
/// produces this and nothing else consumes it but [`InstanceStreams::authorize`] —
/// the request and the grant are different types precisely so a parse result cannot
/// be handed to [`push_loop`] by mistake.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct RequestedInstance {
    /// The INSTANCE-lifecycle stream (not per-target): emit `EVENT * session-created
    /// <sid>` when a session is spawned (by anyone) and `EVENT * session-exited <sid>
    /// reason=<shell-exit|ctl-close|ui-close|window-close|app-quit|unknown>` when one
    /// closes — so a fleet supervisor watching `@.` learns of SIBLING
    /// sessions it is not watching, without polling `ls`/`instances`. The `*` tag
    /// marks an instance-level (not per-channel) event.
    ///
    /// OWNER-ONLY. The set it diffs is the whole instance roster, so no per-target
    /// `ReadScreen` edge can authorize it: an edge scoped to session B would learn
    /// the opaque sid of every sibling in the process. Owner already holds that view
    /// through the `sessions`/`who` verbs, so Owner keeps it and every other scope is
    /// REFUSED (`ERR denied`) rather than handed a stream that pushes nothing.
    ///
    /// BEST-EFFORT, 250ms-SAMPLED: this is a point-in-time set diff on each wake, and
    /// an unwatched sibling never fires the notify, so the loop observes it only on the
    /// bounded 250ms poll. A sibling that both spawns AND exits inside one 250ms window
    /// appears in neither snapshot and emits neither event — the store keeps no
    /// monotonic lifecycle log to recover a sub-tick create→exit. Adequate for
    /// supervision (a session that lived <250ms is rarely actionable); a driver needing
    /// every ephemeral lifecycle must poll `ls` faster or drive from an event log.
    pub sessions: bool,
}

/// The INSTANCE-scoped streams a subscription was GRANTED. The field is PRIVATE, so
/// outside this module the ONLY way to a value with `sessions` set is
/// [`InstanceStreams::authorize`], which demands a [`Scope`]. (`Default` is derived
/// and yields the empty set, which is why deriving it is safe.) That
/// unconstructibility IS the fix: [`push_loop`] seeds its watermark from the store's
/// entire live-session roster — a read no per-target gate ever authorized — and
/// taking this type instead of [`RequestedInstance`] makes reaching that roster
/// impossible without a scope decision.
///
/// The same shape carries forward: a future instance-wide stream (a fleet or
/// cross-instance roster) added to [`RequestedInstance`] inherits the check by
/// construction rather than by someone remembering to add one.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct InstanceStreams {
    sessions: bool,
}

impl InstanceStreams {
    /// Grant the instance-scoped streams `req` asked for. Owner holds instance-wide
    /// authority already — it is the scope the `sessions` and `who` verbs demand — so
    /// Owner gets what it asked for; every other scope is per-target by construction
    /// and gets the EMPTY set.
    ///
    /// The empty set is the INVARIANT, not the user-visible behaviour:
    /// [`crate::control`] refuses a non-Owner `sessions` request with `ERR denied`
    /// before it reaches here, because a silently-empty stream is a worse contract
    /// than a refusal and fail-closed matches the rest of the surface. This fallback
    /// is what keeps the invariant true if a future caller forgets the refusal.
    #[must_use]
    pub fn authorize(req: RequestedInstance, scope: Scope) -> Self {
        InstanceStreams {
            sessions: req.sessions && scope.is_owner_class(),
        }
    }

    /// Whether the instance lifecycle stream was granted.
    #[must_use]
    pub fn sessions(self) -> bool {
        self.sessions
    }
}

/// A parsed `<streams>` token, SPLIT BY SCOPE. The instance half is a
/// [`RequestedInstance`] and not an [`InstanceStreams`] because parsing sees no
/// [`Scope`]: the two halves are authorized by different checks (per resolved target
/// vs. once for the connection), and the type says so at the boundary.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Requested {
    /// The per-target frame sources, gated once per resolved selector.
    pub targets: TargetStreams,
    /// The instance-wide frame sources, gated once for the connection.
    pub instance: RequestedInstance,
    /// A MODIFIER (not a frame source): when set, the frames a wake emits are prefixed
    /// with `T <tag> <t_us>` lines stamping the instant on the SAME clock as `video`'s
    /// `index.json`. This turns the live stream into a TIMED frame source
    /// (frames-over-time) a driver can align against a `video` take. Opt-in, so a
    /// subscriber that does not request it sees the byte-identical un-timestamped
    /// stream. Carries no authority of its own — it only restamps frames the two sets
    /// above already authorized.
    ///
    /// The ledger is per TAG per wake, not one stamp per wake: AT MOST one `T` line is
    /// written per tag per wake, immediately before that tag's FIRST frame, and it
    /// speaks for every later frame the same tag emits in that wake (a wake is one
    /// read, so those lines share the instant). A tag that emits nothing this wake is
    /// not stamped at all, so a `T` line always has frames under it. A wake that writes
    /// three channels emits three `T` lines. That is what keeps a stamp attributable —
    /// a single leading stamp would date frames from channels whose data was read at a
    /// different point in the wake, and a client demultiplexing by leading token would
    /// have to guess which stream each `T` belonged to.
    ///
    /// `<tag>` is [`Tag`]'s rendering, so a per-target frame is stamped
    /// `T <local> <t_us>` and an instance (`sessions`) frame `T * <t_us>`. The second
    /// token is therefore NOT always numeric: a client that parses it as a `u64`
    /// breaks the first time an instance frame is stamped.
    pub timestamps: bool,
}

impl Requested {
    /// Parse a whitespace-or-comma separated stream list (`screen,cursor,events`).
    /// FAIL-CLOSED twice over: `None` on any unknown token, so a typo cannot silently
    /// subscribe to nothing; and `None` on a list that names no FRAME SOURCE at all.
    /// The second case is not pedantry — every token of `subscribe @. ts` parses, so
    /// without the check the client gets `OK subscribe 1` and then silence for the
    /// life of a push-only connection, which is indistinguishable from a hung server.
    #[must_use]
    pub fn parse(s: &str) -> Option<Requested> {
        let mut out = Requested::default();
        for tok in s.split([',', ' ', '\t']).filter(|t| !t.is_empty()) {
            match tok {
                "screen" => out.targets.screen = true,
                "cursor" => out.targets.cursor = true,
                "events" => out.targets.events = true,
                "cells" => out.targets.cells = true,
                "bytes" => out.targets.bytes = true,
                "timestamps" | "ts" => out.timestamps = true,
                "trim" => out.targets.trim = true,
                "sessions" => out.instance.sessions = true,
                _ => return None,
            }
        }
        let any_source = out.targets.any_frame_source() || out.instance.sessions;
        any_source.then_some(out)
    }
}

/// The subscription knobs that carry NO authority: where each stream resumes from
/// and how a wake is rendered. Grouped into ONE parameter so [`push_loop`]'s two
/// authority-bearing arguments — [`TargetStreams`] and [`InstanceStreams`] — stay
/// visibly separate in the signature rather than being buried in a flat option list,
/// which is exactly how the instance-scoped `sessions` flag came to travel with five
/// per-target flags in the first place. (It also keeps the signature inside clippy's
/// argument budget without a stylistic `allow`.)
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct PushOptions {
    /// `since=<seq>`: the client's last-seen `content_seq`. Seeds each watch's
    /// `last_sent_seq` so the immediate catch-up fires exactly when content moved
    /// past it.
    pub since: Option<u64>,
    /// `since-turn=<id>`: resume the `events` turn stream from that ledger id.
    pub since_turn: Option<u64>,
    /// `since-block=<id>`: resume the `events` block stream from that block id.
    pub since_block: Option<u64>,
    /// `every-frame`: re-emit the `cells` frame on every genuine wake even at an
    /// unchanged `content_seq` (animation fidelity).
    pub non_coalesced: bool,
    /// The `timestamps`/`ts` modifier — see [`Requested::timestamps`].
    pub timestamps: bool,
}

/// One subscriber's wake handle: a single-slot notify the producer `try_send`s into.
/// Keyed in [`Subscribers`] by every process-local session id this subscriber
/// watches, plus a unique `token` so a dropped subscriber removes EXACTLY its own
/// entries (never another connection's that happens to watch the same session).
struct SubscriberHandle {
    /// Unique per registration, for precise removal across all watched sessions.
    token: u64,
    /// Single-slot, non-blocking notify. A full slot means "already pending" — the
    /// producer drops the extra wake (coalescing); the subscriber will read the
    /// latest state on its next wake regardless.
    notify: SyncSender<()>,
}

/// The process-wide subscriber index. Keyed by the GUI's process-local `u64`
/// session id so the existing `Wake::Output { session }` fan-out can find every
/// subscriber of the session that just produced output in O(1).
#[derive(Default)]
pub struct SubscriberSet {
    /// session local id -> the handles watching it. A session with no subscribers
    /// has no entry (and `notify` is then a cheap miss).
    by_session: HashMap<u64, Vec<SubscriberHandle>>,
    /// Monotonic source of unique registration tokens.
    next_token: u64,
}

/// The subscriber registry plus a LOCK-FREE "is anyone subscribed?" flag. The
/// `Mutex<SubscriberSet>` is the source of truth; `any` mirrors `!by_session
/// .is_empty()` and is updated under the SAME lock acquisitions that mutate the map
/// (register / drop), with `Release`/`Acquire` so a producer that observes `true`
/// also sees the registration. It lets the producer's per-`Wake::Output` hook skip
/// the mutex entirely in the overwhelmingly common ZERO-subscriber case — a single
/// atomic load instead of an acquire/release of the lock. A stale `true` only costs
/// one redundant lock+miss; a stale `false` is possible only in the instant after a
/// register and is benign — the next output burst observes `true`, and the
/// subscriber's `recv_timeout` requeries, so no update is permanently lost.
pub struct SubscriberRegistry {
    inner: Mutex<SubscriberSet>,
    any: AtomicBool,
}

impl SubscriberRegistry {
    /// Lock the underlying set. Same shape as `Mutex::lock`, so every existing
    /// `registry.lock()` call site is unchanged.
    pub fn lock(&self) -> LockResult<MutexGuard<'_, SubscriberSet>> {
        self.inner.lock()
    }
    /// Lock-free fast-path: `true` iff at least one session has a subscriber. The
    /// producer checks this before taking the lock to `notify`. `Acquire` pairs with
    /// the `Release` store in [`Self::refresh_any`] — the textbook publish-a-flag
    /// idiom — so once the producer observes `true` it also sees the registration that
    /// set it. (A momentarily-stale `false` right after a register is benign: the next
    /// output burst observes `true`, and the subscriber's own `recv_timeout` requeries,
    /// so no update is permanently lost.)
    #[must_use]
    pub fn any(&self) -> bool {
        self.any.load(Ordering::Acquire)
    }
    /// Refresh the flag from the (locked) set's emptiness. Called by register/drop
    /// while they already hold the lock, so it can never disagree across a mutation.
    /// `Release` so the matching `Acquire` in [`Self::any`] sees the map mutation.
    fn refresh_any(&self, set: &SubscriberSet) {
        self.any
            .store(!set.by_session.is_empty(), Ordering::Release);
    }
}

/// Shared handle to the subscriber registry: held by `App` (the producer side,
/// for the one `Wake::Output` notify hook) and cloned into the control thread
/// (the consumer side, where a `subscribe` connection registers itself).
pub type Subscribers = Arc<SubscriberRegistry>;

/// A new, empty subscriber registry.
#[must_use]
pub fn new_registry() -> Subscribers {
    Arc::new(SubscriberRegistry {
        inner: Mutex::new(SubscriberSet::default()),
        any: AtomicBool::new(false),
    })
}

/// The consumer-side end of a registration: the subscriber thread `recv`s wakes
/// here, and on drop deregisters itself from every session it watched. RAII so a
/// subscriber that returns (write failure / client hangup) cannot leak an entry
/// that would make `notify` pay for a dead receiver forever.
pub struct Subscription {
    registry: Subscribers,
    /// The sessions this subscription registered under (for precise deregistration).
    sessions: Vec<u64>,
    /// This subscription's unique token (matches its handles in the registry).
    token: u64,
    /// The blocking wake end. `recv` parks until the producer notifies or the
    /// timeout elapses. It can no longer observe a hangup at all — this struct
    /// retains a sender (`notify`, below) for the lifetime of the subscription —
    /// and [`Self::wait`] already treated `Disconnected` and `Timeout`
    /// identically, so nothing above it changes.
    rx: Receiver<()>,
    /// The SEND end of the same single-slot notify, retained so the target set can
    /// GROW after registration ([`Self::watch`]).
    ///
    /// It used to be dropped at the end of `register`, which is precisely why a
    /// subscription's watch set was frozen for life: adding a session needs a
    /// handle to clone into the registry, and there was none. Retaining it costs
    /// one `SyncSender` (a pointer) and buys a live target set.
    notify: SyncSender<()>,
}

impl Subscription {
    /// Add `local_id` to this subscription's watch set, mid-stream.
    ///
    /// Idempotent per subscription: a session already registered under OUR token
    /// is not registered twice (a duplicate handle would double every notify for
    /// that session and leave a stale entry behind on drop).
    pub fn watch(&mut self, local_id: u64) {
        if self.sessions.contains(&local_id) {
            return;
        }
        let mut g = self.registry.lock().unwrap_or_else(|p| p.into_inner());
        g.by_session
            .entry(local_id)
            .or_default()
            .push(SubscriberHandle {
                token: self.token,
                notify: self.notify.clone(),
            });
        self.registry.refresh_any(&g);
        drop(g);
        self.sessions.push(local_id);
    }

    /// Drop `local_id` from this subscription's watch set.
    ///
    /// Called when a watched session leaves the store. Without it, a LONG-LIVED
    /// subscription (the whole point of a live target set) accumulates registry
    /// entries for dead sessions: every one is a `Vec` slot the producer's
    /// `notify` walks and a `sessions` entry `Drop` must later clean up. The
    /// same precise `token` match `Drop` uses, so it can only ever remove OUR
    /// handle, never another connection's on the same session.
    pub fn unwatch(&mut self, local_id: u64) {
        let mut g = self.registry.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(v) = g.by_session.get_mut(&local_id) {
            v.retain(|h| h.token != self.token);
            if v.is_empty() {
                g.by_session.remove(&local_id);
            }
        }
        self.registry.refresh_any(&g);
        drop(g);
        self.sessions.retain(|s| *s != local_id);
    }
    /// Block until the producer notifies this subscriber (output landed on one of
    /// the watched sessions), or until `timeout` elapses. Returns `true` on a wake,
    /// `false` on timeout. A spurious/coalesced wake is fine: the caller re-reads
    /// the latest state and emits a delta only if `content_seq` advanced.
    #[must_use]
    pub fn wait(&self, timeout: Duration) -> bool {
        matches!(self.rx.recv_timeout(timeout), Ok(()))
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let mut g = self.registry.lock().unwrap_or_else(|p| p.into_inner());
        for sid in &self.sessions {
            if let Some(v) = g.by_session.get_mut(sid) {
                v.retain(|h| h.token != self.token);
                if v.is_empty() {
                    g.by_session.remove(sid);
                }
            }
        }
        self.registry.refresh_any(&g);
    }
}

impl SubscriberSet {
    /// Register a subscriber watching `sessions` (their process-local ids). Returns
    /// a [`Subscription`] whose `wait` blocks until a watched session produces
    /// output; dropping it deregisters from all watched sessions. The single-slot
    /// notify is created here and its receiver handed back inside the subscription.
    #[must_use]
    pub fn register(registry: &Subscribers, sessions: &[u64]) -> Subscription {
        // capacity 1 == single-slot: at most one pending notify (coalescing).
        let (tx, rx) = sync_channel::<()>(1);
        let mut g = registry.lock().unwrap_or_else(|p| p.into_inner());
        let token = g.next_token;
        g.next_token = g.next_token.wrapping_add(1);
        for &sid in sessions {
            g.by_session.entry(sid).or_default().push(SubscriberHandle {
                token,
                notify: tx.clone(),
            });
        }
        registry.refresh_any(&g);
        drop(g);
        Subscription {
            registry: registry.clone(),
            sessions: sessions.to_vec(),
            token,
            rx,
            notify: tx,
        }
    }

    /// Notify every subscriber of session `local_id` that it produced output.
    /// NON-BLOCKING and infallible by construction: a full single-slot channel
    /// (notify already pending) or a hung-up receiver (subscriber thread gone) is
    /// silently ignored, so the producer's reader/GUI thread is NEVER stalled by a
    /// slow or dead subscriber. This is the ONLY method the producer calls.
    pub fn notify(&self, local_id: u64) {
        let Some(handles) = self.by_session.get(&local_id) else {
            return; // no subscribers for this session: cheap miss
        };
        for h in handles {
            match h.notify.try_send(()) {
                // delivered, or already pending (coalesced), or receiver gone:
                // every outcome is a no-op for the producer.
                Ok(()) | Err(TrySendError::Full(())) | Err(TrySendError::Disconnected(())) => {}
            }
        }
    }

    /// How many live subscribers are watching one session — the `watchers=` the
    /// `who` verb reports and the "eye" a presence indicator lights. Counts every
    /// stream (a `screen`+`events` subscriber counts once: one registration).
    #[must_use]
    pub fn watchers(&self, local_id: u64) -> usize {
        self.by_session.get(&local_id).map_or(0, Vec::len)
    }

    /// Number of distinct sessions with at least one subscriber (test/introspection).
    #[must_use]
    #[allow(dead_code)]
    pub fn watched_sessions(&self) -> usize {
        self.by_session.len()
    }
}

/// One watched target inside a subscription: its process-local id and the live
/// `(term, sid_string, ctx)` handle resolved ONCE at subscribe time, plus the
/// per-target send cursors (`last_sent` content seq, last block id) so the
/// coalescing compare is O(1) on each wake.
///
/// The multiplex `<local>` tag on every frame is the process-local CHANNEL id (the
/// registry key as a string). It is NOT the `s-…` sid — a compact per-connection
/// handle. The `run_subscribe` ack maps each `<local>` to its stable sid via one
/// `sub <local> <sid>` line, so a client that subscribed by `@s-…`/`@.` can
/// demultiplex frames back to the sids it knows.
struct Watch {
    /// The process-local id (the registry key + the `<local>` frame tag).
    local_id: u64,
    /// The live engine handle (cloned out of the store, clone-then-release).
    term: Arc<Mutex<Terminal>>,
    /// The last `content_seq` we emitted a DELTA for. A wake with an unchanged
    /// seq emits NOTHING (a pure viewport scroll never bumps content_seq).
    last_sent_seq: u64,
    /// The alt-screen flag as of the last content DELTA. An alt swap (1049) does
    /// NOT touch the main grid's `content_seq` (the poll path's compound cache key
    /// keys on both for exactly this reason — control_query.rs:665), so a swap
    /// whose seq happens to match `last_sent_seq` would be INVISIBLE to a pure seq
    /// compare. A flip is treated like a backward seq: GAP + full resync frame.
    last_alt: bool,
    /// The `(row, col, visible, style)` of the last cursor DELTA we emitted. The
    /// `cursor` stream is named after this datum, but a pure caret move / DECSCUSR
    /// style flip / DECTCEM visibility toggle bumps no `content_seq`, so keying
    /// cursor frames on seq alone starves the stream (a TUI moving the caret between
    /// painted fields, a vi-mode shape flip, a shell hiding the caret during
    /// completion). Compared on every wake so a cursor-only change still emits a
    /// DELTA. The `visible` bit matches the poll `cursor` verb's `<visible>` field.
    last_cursor: (u16, u16, bool, &'static str),
    /// A cheap fingerprint of the RENDER state that changes the styled output
    /// WITHOUT bumping `content_seq`: dynamic/palette colors, DECSCNM, and the
    /// cursor + selection overlays (including their geometry). Recolor, selection,
    /// and pure caret changes mark damage but do not advance the content sequence,
    /// so a seq-gated `cells` subscriber would otherwise retain stale colors or
    /// rectangles until the next glyph write. When this signature changes at an
    /// unchanged seq we re-emit the content frame (no GAP — the delta cursor is
    /// still valid). Seeded to the live signature at subscription so only later
    /// changes push.
    last_render_sig: u64,
    /// The id of the highest block we have already reported `block-complete` for,
    /// so re-scanning completed blocks on each wake never double-emits. `None`
    /// before any block has completed.
    last_block_id: Option<u64>,
    /// The session's TURN LEDGER (the `events` digest scans it for new turns).
    turns: Arc<Mutex<TurnLedger>>,
    /// The highest turn id already reported, mirroring `last_block_id` — a live
    /// stream, so it is seeded to the current high at subscription.
    last_turn_id: Option<u64>,
    /// The session's EVENT TIMELINE (session-metadata stage 1): the `events`
    /// digest scans it for `meta-change` records and pushes each as
    /// `EVENT <sid> meta <payload>` — the push face of the `meta` verb.
    timeline: Arc<Mutex<crate::session_timeline::SessionTimeline>>,
    /// The highest timeline event id already scanned, mirroring `last_turn_id`
    /// — a live stream, seeded to the current high at subscription.
    last_timeline_id: Option<u64>,
    /// The window title as of the last `EVENT … title` we emitted. A title change
    /// (OSC 0/2 — often mirroring the cwd or running command via shell integration)
    /// is a fleet-supervision signal an orchestrator should get on the `events`
    /// stream, not have to poll `title` for. Seeded to the live title at subscription
    /// so only CHANGES after that are pushed. `None` until the events stream first runs.
    last_title: Option<String>,
    /// The `bell_total` (monotonic fired-bell count) as of the last `EVENT … bell`.
    /// A bell (BEL / OSC 777 alert) is a supervision signal a fleet orchestrator
    /// should get on the `events` stream. Seeded to the live total at subscription
    /// so only bells AFTER that are pushed.
    last_bell: u64,
    /// The live byte-stream subscription for the `bytes` stream (Item 2), or `None`
    /// when `bytes` was not requested. Drained every wake into `BYTES`/`GAP` frames.
    byte_sub: Option<ByteSubscription>,
    /// `every-frame` mode: re-emit the `cells` frame on EVERY wake even when
    /// `content_seq` is unchanged (animation fidelity), instead of only on advance.
    non_coalesced: bool,
}

/// AUTHORITY for the `@*` LIVE TARGET SET: whether this subscription may adopt
/// sessions that are created AFTER it subscribed.
///
/// Constructible only by [`Self::authorize`], which ANDs the request with
/// `Scope::Owner` — the same shape (and the same reason) as
/// [`InstanceStreams::authorize`]. `@*` names sessions the subscriber could not
/// have named itself, so it reveals the existence and opaque sid of siblings
/// exactly the way the instance `sessions` stream does, and it is gated exactly
/// the way that stream is. Because Owner is the only holder, the per-target
/// `ReadScreen` gate an adopted session would face is trivially satisfied by
/// construction — which is WHY the capability is Owner-only rather than "Owner
/// or an edge that happens to reach far enough": there is no such edge, and a
/// type that cannot represent one cannot leak one.
#[derive(Clone, Copy)]
pub struct AdoptScope(bool);

impl AdoptScope {
    /// Grant `@*` adoption IFF it was requested AND the connection is Owner.
    #[must_use]
    pub fn authorize(requested: bool, scope: Scope) -> Self {
        AdoptScope(requested && scope.is_owner_class())
    }

    /// The refusing default — no adoption. What every non-`@*` subscribe passes.
    #[must_use]
    #[allow(dead_code)] // used by tests + any future non-adopting `push_loop` caller
    pub fn none() -> Self {
        AdoptScope(false)
    }

    /// Whether this subscription adopts.
    fn on(self) -> bool {
        self.0
    }
}

/// A frame's CHANNEL tag — which of a subscription's channels the frame speaks for.
/// `Instance` renders as `*`, the tag `EVENT * session-created …` already uses, so
/// stamping an instance frame (`T * <t_us>`) needs no change to the wire grammar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tag {
    /// A per-target frame, tagged with the target's process-local channel id.
    Channel(u64),
    /// A connection-level frame that speaks for no single target (the `sessions`
    /// lifecycle stream).
    Instance,
}

impl std::fmt::Display for Tag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tag::Channel(id) => write!(f, "{id}"),
            Tag::Instance => f.write_str("*"),
        }
    }
}

/// The wire `<local>` channel tag for a target: the rendering of [`Tag::Channel`].
/// One connection watching multiple sessions demultiplexes frames by this leading
/// token, resolving it to a stable sid via the ack's `sub <local> <sid>` map.
fn sid_tag(local_id: u64) -> String {
    Tag::Channel(local_id).to_string()
}

/// One CONTIGUOUS run of already-formatted wire bytes plus the channel it belongs to.
/// Deliberately NOT one wire line: every line a producer builds for one tag in one
/// wake shares a single instant (a wake is one read), so the stamp ledger gains
/// nothing from splitting and would pay an allocation per line, 4×/sec per
/// subscriber.
///
/// The fields are private and the constructors live here, so the ONLY route from
/// bytes to the socket is [`Egress::emit`]. That is what turns "was this frame
/// stamped?" into a property of the door rather than of each write site — the six
/// old write sites each had to remember, and four of them structurally could not,
/// because their producers took no timestamp flag at all.
struct Frame {
    tag: Tag,
    body: Vec<u8>,
}

impl Frame {
    /// A text frame: `DELTA` / `EVENT` / `GAP` bodies are all UTF-8.
    fn text(tag: Tag, body: String) -> Frame {
        Frame {
            tag,
            body: body.into_bytes(),
        }
    }

    /// A RAW frame — the `bytes` stream is byte-lossless, so its body is arbitrary
    /// binary and must never round-trip through `String`.
    fn raw(tag: Tag, body: Vec<u8>) -> Frame {
        Frame { tag, body }
    }
}

/// The client hung up. A ZST, so `Result<(), Gone>` is as cheap as `()` and `?`
/// replaces the identical `if writer.write_all(..).is_err() { return; }` block that
/// stood at every one of the old six write sites. NOT a failure to report: a
/// push-only connection ends exactly this way.
#[derive(Clone, Copy, Debug)]
struct Gone;

/// The ONE door from a [`Frame`] to the subscriber socket. It owns the writer for the
/// push loop's whole life, owns the timestamp policy, and owns the per-wake stamp
/// LEDGER — so `T <tag> <t_us>` is emitted once per tag per wake for EVERY frame
/// kind, including the four that were structurally unstampable while the stamp lived
/// inside one producer: the `bytes` bursts, the instance lifecycle events, a closing
/// target's `exited`, and the events-resume `GAP`.
struct Egress<'w, W: Write> {
    writer: &'w mut W,
    /// The `timestamps`/`ts` modifier — see [`Requested::timestamps`].
    timestamps: bool,
    /// The tags already stamped THIS wake. Cleared (not reallocated) by
    /// [`Egress::begin_wake`], so the ledger costs nothing after the first wake; a
    /// linear scan is the right lookup because the tag count is the watch count + 1.
    stamped: Vec<Tag>,
    /// Whether anything reached the writer since the last flush, so the common idle
    /// wake — 4 ticks/sec on a quiet session — costs no syscall.
    wrote: bool,
}

impl<W: Write> Egress<'_, W> {
    fn new(writer: &mut W, timestamps: bool) -> Egress<'_, W> {
        Egress {
            writer,
            timestamps,
            stamped: Vec::new(),
            wrote: false,
        }
    }

    /// Open a wake: clear the stamp ledger so each tag is stamped once for the frames
    /// this wake produces, and reset the flush flag.
    fn begin_wake(&mut self) {
        self.stamped.clear();
        self.wrote = false;
    }

    /// Write `frame`, preceded by `T <tag> <t_us>` iff timestamps were requested and
    /// this tag is still unstamped this wake. An EMPTY body is a no-op that does NOT
    /// consume the tag's stamp — otherwise a producer that had nothing to say would
    /// silence the stamp of the next producer on the same tag.
    fn emit(&mut self, frame: Frame) -> Result<(), Gone> {
        if frame.body.is_empty() {
            return Ok(());
        }
        if self.timestamps && !self.stamped.contains(&frame.tag) {
            self.stamped.push(frame.tag);
            let stamp = format!("T {} {}\n", frame.tag, crate::metrics::now_us());
            self.put(stamp.as_bytes())?;
        }
        self.put(&frame.body)
    }

    /// Close a wake: flush iff it wrote anything. Reporting the flush error (rather
    /// than ignoring it) is what surfaces a client that hung up during a wake whose
    /// writes all landed in a buffer — otherwise a dead subscriber lingers registered
    /// until its silent session next produces output, i.e. effectively forever.
    fn end_wake(&mut self) -> Result<(), Gone> {
        if !self.wrote {
            return Ok(());
        }
        self.wrote = false;
        self.writer.flush().map_err(|_| Gone)
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), Gone> {
        self.writer.write_all(bytes).map_err(|_| Gone)?;
        self.wrote = true;
        Ok(())
    }
}

/// Format a full screen DELTA for `sid` at `seq`: a header line followed by one
/// row per screen line (CHANGED set is the whole screen here — a coalesced wake
/// re-reads the latest grid rather than a diff, the backpressure-safe choice). The
/// row count is on the header so a client can frame the body without guessing.
///
/// `DELTA <sid> seq=<n> screen <nrows>\n` then `<nrows>` trimmed rows.
fn frame_screen(sid: &str, seq: u64, rows: &[String]) -> String {
    let mut out = format!("DELTA {sid} seq={seq} screen {}\n", rows.len());
    for r in rows {
        out.push_str(r);
        out.push('\n');
    }
    out
}

/// Format a cursor DELTA for `sid` at `seq`:
/// `DELTA <sid> seq=<n> cursor <row> <col> <visible> <style>\n`. The `<visible>`
/// bit (`0|1`) matches the poll `cursor` verb's field, so a DECTCEM hide/show is
/// visible on the push stream too (it moves no caret and bumps no content_seq).
fn frame_cursor(sid: &str, seq: u64, row: u16, col: u16, visible: bool, style: &str) -> String {
    format!(
        "DELTA {sid} seq={seq} cursor {row} {col} {} {style}\n",
        u8::from(visible)
    )
}

/// A cheap fingerprint of the RENDER state that changes the styled output without
/// advancing `content_seq` (see [`Watch::last_render_sig`]): DECTCEM visibility,
/// cursor position/style, DECSCNM reverse-video, the dynamic default fg/bg +
/// cursor/selection colours, the side-adjusted selection kind/range, and the full
/// 256-entry palette. Bounded (256 palette reads + a few dynamics per wake), and
/// only computed for a `screen`/`cells` subscriber — negligible next to the grid
/// gather this lets us SKIP when nothing changed. (Note: `palette_color` scans the
/// override list, so a heavy live OSC-4 theme makes each read O(overrides), not
/// strictly O(1) — still a small constant, well under the styled gather it avoids.)
fn render_sig(t: &Terminal) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |b: u8| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    mix(u8::from(t.cursor_visible()));
    mix(u8::from(t.reverse_video()));
    let cursor = t.cursor();
    for byte in cursor.row.to_le_bytes() {
        mix(byte);
    }
    for byte in cursor.col.to_le_bytes() {
        mix(byte);
    }
    for byte in crate::control::cursor_style_name(t.cursor_style()).bytes() {
        mix(byte);
    }
    // Delimit the variable-length style name from the next field.
    mix(0);
    for c in [t.default_foreground(), t.default_background()] {
        mix(c.r);
        mix(c.g);
        mix(c.b);
    }
    match t.cursor_color() {
        Some(c) => {
            mix(1);
            mix(c.r);
            mix(c.g);
            mix(c.b);
        }
        None => mix(0),
    }
    for color in [t.selection_background(), t.selection_foreground()] {
        match color {
            Some(c) => {
                mix(1);
                mix(c.r);
                mix(c.g);
                mix(c.b);
            }
            None => mix(0),
        }
    }
    let selection = t.text_selection();
    match selection.project_range(t.cols().saturating_sub(1)) {
        Some(projected) => {
            mix(1);
            let kind = match selection.selection_type() {
                aterm_core::selection::SelectionType::Simple => 0,
                aterm_core::selection::SelectionType::Block => 1,
                aterm_core::selection::SelectionType::Semantic => 2,
                aterm_core::selection::SelectionType::Lines => 3,
                _ => u8::MAX,
            };
            mix(kind);
            mix(u8::from(projected.is_block));
            for byte in projected.start_row.to_le_bytes() {
                mix(byte);
            }
            for byte in projected.start_col.to_le_bytes() {
                mix(byte);
            }
            for byte in projected.end_row.to_le_bytes() {
                mix(byte);
            }
            for byte in projected.end_col.to_le_bytes() {
                mix(byte);
            }
        }
        None => mix(0),
    }
    for i in 0..=255u8 {
        let c = t.palette_color(i);
        mix(c.r);
        mix(c.g);
        mix(c.b);
    }
    h
}

/// Format a styled CELLS DELTA — the lossless styled-screen frame pushed live.
/// LENGTH-PREFIXED so the (large, single-line) JSON body frames cleanly on the
/// line-based socket: `DELTA <sid> seq=<n> cells <nbytes>\n<json>\n`. The body is
/// exactly Item 1's `styled_frame_payload` (one physical line); the trailing `\n`
/// is a non-counted terminator the client discards after reading `<nbytes>`.
fn frame_cells(sid: &str, seq: u64, payload: &str) -> String {
    format!("DELTA {sid} seq={seq} cells {}\n{payload}\n", payload.len())
}

/// Drain the `bytes` stream into `BYTES`/`GAP` frames as RAW bytes (binary-safe —
/// no escaping, no UTF-8 decode). A counted `GAP <sid> bytes-dropped=<n>` precedes
/// the bursts whenever the per-subscriber queue overflowed since the last drain.
/// Canonical binary framing: `BYTES <sid> <len>\n<len bytes>\n`.
///
/// Takes `&Watch`, not `&mut`: the byte queue IS its own watermark (draining empties
/// it), so unlike the seq/turn/block streams this producer moves no send cursor — and
/// that is what lets [`Closing::drain`] read the tail of a watch it is consuming.
fn drain_bytes_frames(watch: &Watch) -> Vec<Frame> {
    let Some(bs) = &watch.byte_sub else {
        return Vec::new();
    };
    let sid = sid_tag(watch.local_id);
    let (bursts, dropped) = bs.drain();
    let mut out: Vec<u8> = Vec::new();
    if dropped > 0 {
        out.extend_from_slice(format!("GAP {sid} bytes-dropped={dropped}\n").as_bytes());
    }
    for burst in bursts {
        out.extend_from_slice(format!("BYTES {sid} {}\n", burst.len()).as_bytes());
        out.extend_from_slice(&burst);
        out.push(b'\n');
    }
    if out.is_empty() {
        return Vec::new();
    }
    vec![Frame::raw(Tag::Channel(watch.local_id), out)]
}

/// Format a block-complete EVENT for `sid`:
/// `EVENT <sid> block-complete <id> exit=<code|->\n`.
fn frame_block_complete(sid: &str, id: u64, exit: Option<i32>) -> String {
    let exit = exit.map_or_else(|| "-".to_string(), |c| c.to_string());
    format!("EVENT {sid} block-complete {id} exit={exit}\n")
}

/// Emit `EVENT <sid> title <pct-encoded>` when the window title CHANGED since the
/// last drain (OSC 0/2, often the cwd or running command via shell integration) —
/// a fleet-supervision signal on the `events` stream. Returns the new watermark.
/// The title is pct-encoded so a title with spaces/newlines stays one line.
///
/// PURE (takes the already-sampled `title`, not the engine handle): the digest
/// samples the title, the bell total and the new completed blocks in ONE
/// [`sample_engine_events`] lock hold, because an events-only subscription used
/// to take `term_lock` three separate times per target per 250 ms tick purely to
/// learn that nothing had changed — and that lock is the one the renderer and
/// the keystroke-encode path contend for.
fn drain_title_event(
    sid: &str,
    title: &str,
    last_title: Option<String>,
    out: &mut String,
) -> Option<String> {
    if last_title.as_deref() != Some(title) {
        out.push_str(&format!(
            "EVENT {sid} title {}\n",
            crate::control::pct_encode(title)
        ));
    }
    Some(title.to_string())
}

/// Scan the target's EVENT TIMELINE and push the record kinds the wire carries,
/// for every record with id strictly greater than `last_id`:
///
/// * `EVENT <sid> meta <payload>` for a `meta-change` — the push face of the
///   `meta` verb, next to the title emitter (a fleet supervisor learns a sibling
///   was renamed/annotated without polling `meta`); the payload is the record's
///   own pre-pct-encoded `field=<f> value=<pct|->` tail.
/// * `EVENT <sid> closing <payload>` for the `closing` row the store writes as
///   it retires the session — the `reason=<token> by=<sid|human|->` the `exits`
///   ledger holds. This watch's own `Arc` is the ONLY wire path that can still
///   read that row: the `timeline` verb resolves its target through the
///   registry, and the sid stops resolving in the same store write that records
///   it. It lands in the dying watch's final pass ([`Closing::drain`]), so it
///   always precedes `EVENT <sid> exited`.
///
/// Every other kind (`spawned`, `state-change`, `title-change`, `cwd-change`) is
/// skipped: title has its own emitter and lifecycle state is what `exited`
/// says. [`timeline_wire_kind`] is the one table both the filter and the frame
/// name come from. The returned watermark advances to the timeline HIGH (not
/// just the last pushed record), so skipped records are scanned once, never
/// re-walked every wake. Small payload strings are cloned OUT under the lock;
/// the lock is never held across a write.
fn drain_timeline_events(
    timeline: &Arc<Mutex<crate::session_timeline::SessionTimeline>>,
    sid: &str,
    last_id: Option<u64>,
    out: &mut String,
) -> Option<u64> {
    let (fresh, high) = {
        let tl = timeline.lock().unwrap_or_else(|p| p.into_inner());
        // O(1) HIGH-WATER COMPARE BEFORE THE SCAN. `high_id()` is `back()`, so
        // this is one integer compare; the overwhelmingly common wake is a bare
        // 250 ms liveness tick on a session whose timeline has not moved, and
        // that wake must not touch the retained deque at all. Returning
        // `Some(hi)` (not `last_id`) reproduces the old
        // `tl.high_id().or(last_id)` watermark byte-for-byte on this arm.
        match tl.high_id() {
            None => return last_id,
            Some(hi) if last_id.is_some_and(|a| hi <= a) => return Some(hi),
            Some(hi) => {
                // `since` now SEEKS (partition_point) instead of filtering the
                // whole retained ring, so this costs O(log n + matched).
                let fresh: Vec<(&'static str, String)> = tl
                    .since(last_id)
                    .filter_map(|e| timeline_wire_kind(e.kind).map(|k| (k, e.payload.clone())))
                    .collect();
                (fresh, Some(hi))
            }
        }
    };
    for (kind, payload) in fresh {
        out.push_str(&format!("EVENT {sid} {kind} {payload}\n"));
    }
    high
}

/// The `EVENT <sid> <kind>` token a timeline record is pushed under on the
/// `events` digest, or `None` for a kind the digest does not carry. ONE table,
/// so the drain's filter and the frame it formats cannot disagree about which
/// rows leave the process: the `closing` row first shipped recorded-but-never-
/// pushed, because the filter named `meta-change` and nothing else, and the
/// verb table claimed a watch could read it — the drift this table forecloses.
fn timeline_wire_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "meta-change" => Some("meta"),
        "closing" => Some("closing"),
        // The FABRIC digest (design §11.2): `inbox`, `inbox-seen`, `post`,
        // `post-landed`, `hold`. Their wire name IS the record kind, and the list
        // lives beside the code that WRITES them
        // ([`crate::fabric::FABRIC_EVENT_KINDS`]) rather than being retyped here,
        // so a sixth fabric event cannot ship recorded-but-never-pushed — the
        // exact drift the `closing` row shipped with. NONE of them carries a body:
        // `inbox` is a count and an offset, `post` an address and a kind. The
        // digest tells an agent that mail EXISTS; reading it is a separate,
        // deliberate `inbox` call, so a watcher is never handed message text it
        // did not ask for.
        other => crate::fabric::fabric_event_wire_kind(other),
    }
}

/// Emit `EVENT <sid> bell total=<n>` when the monotonic fired-bell count advanced
/// since the last drain (BEL / OSC 777 alert) — a supervision signal on the
/// `events` stream. Returns the new watermark. One event per drain even if several
/// bells fired between wakes (the total tells how many); the throttle already
/// collapses bursts, so this cannot flood.
///
/// PURE (takes the already-sampled `total`) — see [`drain_title_event`] for why
/// the engine reads are hoisted into one [`sample_engine_events`] lock hold.
fn drain_bell_event(sid: &str, total: u64, last_bell: u64, out: &mut String) -> u64 {
    if total > last_bell {
        out.push_str(&format!("EVENT {sid} bell total={total}\n"));
    }
    total
}

/// A `sessions` subscriber's position on the instance roster: the journal
/// watermark it has drained to, plus the live set as of that watermark.
///
/// The SET is not the cursor — the watermark is. The set is carried only so the
/// RECOVERY path (a watermark that fell past the journal's drop-oldest
/// low-water) can still produce a correct diff instead of replaying a lossy
/// delta or resyncing the client with a new wire frame nobody parses. On the
/// normal path it is mutated by the journal records themselves, one insert or
/// remove per real lifecycle event, so it costs nothing on an idle tick.
struct RosterCursor {
    /// Highest roster-journal seq already drained. `0` = "seen nothing".
    seq: u64,
    /// The live sids as of `seq` (recovery input only — see the type doc).
    known: std::collections::HashSet<String>,
}

/// Emit `EVENT * session-created <sid>` / `EVENT * session-exited <sid> reason=<…>` for
/// every roster change since the cursor's watermark — the INSTANCE lifecycle
/// stream (`*` = instance-level, not a per-channel event). Surfaces a SIBLING
/// spawn/exit a fleet supervisor is not watching, so it need not poll `ls`.
///
/// THREE THINGS CHANGED HERE, AND THE THIRD IS A CORRECTNESS FIX.
///
///  1. AN IDLE TICK IS O(1). The store now keeps a monotonic membership journal,
///     so "did anything change?" is one `u64` compare under a read lock. It used
///     to be: build a fresh `HashSet<String>` of every live sid (one allocation
///     and one hash per session), then run two `HashSet::difference` passes over
///     it — for every `sessions` subscriber, on every 250 ms wake, including the
///     bare liveness ticks that are the overwhelming majority.
///
///  2. A TICK WITH CHANGES IS O(changes), not O(sessions).
///
///  3. A SUB-TICK SPAWN+EXIT NOW SURFACES. The old design could not report it,
///     and the push loop's own module doc said so: "A sibling that both spawns
///     AND exits inside one 250ms window appears in neither snapshot and emits
///     neither event — the store keeps no monotonic lifecycle log." A snapshot
///     diff reports only the NET change between two instants; events that
///     cancelled out are gone. The journal records each transition as it
///     happens, so BOTH events are emitted, in order, on the next wake — without
///     shortening the tick or adding a notify path.
///
/// The `known` set and [`diff_session_events`] survive as the RECOVERY path: a
/// cursor that fell past the journal's retained low-water rebuilds and diffs
/// exactly as before. That arm is behaviour-identical to the old code (it IS the
/// old code), so the worst case degrades to today's cost rather than to a wire
/// change or a dropped event.
///
/// The cursor is advanced in place only after its frames have been produced, so
/// the watermark can never move without the events having been written.
fn drain_session_events(store: &Store, cursor: &mut RosterCursor) -> Vec<Frame> {
    let mut out = String::new();
    {
        // ONE read-lock hold. Never taken across a Terminal lock or a socket
        // write — the guard is dropped before the frame goes out, exactly like
        // the snapshot read it replaces.
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        let high = g.roster_seq();
        if high == cursor.seq {
            // THE IDLE TICK: nothing has entered or left the registry since we
            // last looked. One integer compare, no allocation, no set build.
            return Vec::new();
        }
        match g.roster_low_seq() {
            // Fast path: every record we have not seen is still retained, so the
            // delta is exact and complete.
            Some(low) if cursor.seq + 1 >= low => {
                for rec in g.roster_since(cursor.seq) {
                    match rec.change {
                        crate::session_store::RosterChange::Created => {
                            out.push_str(&format!("EVENT * session-created {}\n", rec.sid));
                            cursor.known.insert(rec.sid.clone());
                        }
                        crate::session_store::RosterChange::Exited => {
                            // `reason=` is a TRAILING additive token (old clients
                            // key on the sid and ignore the tail): the journal row
                            // knows why the session went, so the push says so —
                            // the one thing a driver could never ask afterwards.
                            out.push_str(&format!(
                                "EVENT * session-exited {} reason={}\n",
                                rec.sid,
                                rec.reason.as_str()
                            ));
                            cursor.known.remove(&rec.sid);
                        }
                    }
                }
            }
            // RECOVERY: records between our watermark and the retained window
            // were drop-oldest evicted (or the journal is somehow empty at a
            // non-zero high). Rebuild and diff — the pre-journal behaviour,
            // verbatim. Net-lossy for the cancelled-out pairs, which is exactly
            // and only as lossy as the design was before the journal existed.
            _ => {
                let live = g.live_sids();
                diff_session_events(&live, &cursor.known, &mut out);
                cursor.known = live;
            }
        }
        cursor.seq = high;
    }
    if out.is_empty() {
        return Vec::new();
    }
    vec![Frame::text(Tag::Instance, out)]
}

/// The pure set-diff half of [`drain_session_events`] (store-free, unit-testable):
/// `session-created` for sids newly live, `session-exited … reason=unknown` for sids
/// gone (a set diff knows THAT a session went, never why).
fn diff_session_events(
    live: &std::collections::HashSet<String>,
    known: &std::collections::HashSet<String>,
    out: &mut String,
) {
    for sid in live.difference(known) {
        out.push_str(&format!("EVENT * session-created {sid}\n"));
    }
    for sid in known.difference(live) {
        // The set diff has no journal row to read a reason from: it reports the
        // NET change between two instants, and the row that said why was
        // evicted. `unknown` is the honest token, and it keeps the frame shape
        // identical to the journal path's so a parser sees one grammar.
        out.push_str(&format!(
            "EVENT * session-exited {sid} reason={}\n",
            crate::session_store::ExitReason::Unknown.as_str()
        ));
    }
}

/// Scan the target's TURN LEDGER and emit a turn EVENT for every record whose id
/// is strictly greater than `last_turn_id`, advancing it — the turn-lifecycle
/// twin of `drain_block_events`. The small `(id, submitted, status, dur_ms)`
/// tuples are cloned OUT under the lock; the lock is never held across a write.
/// Digest form (no text/hash — that is what the `turns` verb is for):
/// `EVENT <sid> turn <id> submitted=<0|1> status=<..> dur_ms=<..>\n`.
fn drain_turn_events(
    turns: &Arc<Mutex<TurnLedger>>,
    sid: &str,
    last_turn_id: Option<u64>,
    out: &mut String,
) -> Option<u64> {
    let fresh: Vec<(u64, bool, &'static str, u64)> = {
        let l = turns.lock().unwrap_or_else(|p| p.into_inner());
        // O(1) HIGH-WATER COMPARE BEFORE THE SCAN — the timeline twin. `high_id()`
        // is `back()`. An idle wake (the common case: 4 Hz liveness ticks on a
        // session nobody is driving) now costs one integer compare instead of a
        // walk over up to `LEDGER_CAP` retained records. The early-out returns
        // `last_turn_id` unchanged, which is exactly what the old fold produced
        // when it collected nothing.
        match l.high_id() {
            None => return last_turn_id,
            Some(hi) if last_turn_id.is_some_and(|a| hi <= a) => return last_turn_id,
            // `since` SEEKS now (partition_point), so this is O(log n + matched).
            Some(_) => l
                .since(last_turn_id)
                .map(|r| (r.id, r.submitted, r.status, r.dur_ms))
                .collect(),
        }
    };
    let mut high = last_turn_id;
    for (id, submitted, status, dur_ms) in fresh {
        out.push_str(&format!(
            "EVENT {sid} turn {id} submitted={} status={status} dur_ms={dur_ms}\n",
            u8::from(submitted)
        ));
        high = Some(high.map_or(id, |h| h.max(id)));
    }
    high
}

/// Format the one-shot `exited` EVENT for `sid`: `EVENT <sid> exited\n`. Emitted
/// by the push loop when a watched session leaves the registry.
fn frame_exited(sid: &str) -> String {
    format!("EVENT {sid} exited\n")
}

/// Format a resync GAP for `sid`: `GAP <sid> resync=<seq>\n`. Emitted when the
/// engine's `content_seq` moved BACKWARD relative to our last-sent (a reset /
/// alt-screen swap / engine rebuild made the prior delta cursor meaningless), so
/// the client knows to treat the next DELTA as a fresh full snapshot.
fn frame_gap(sid: &str, seq: u64) -> String {
    format!("GAP {sid} resync={seq}\n")
}

/// A `GAP <sid> events-resync=<floor>` frame: a `since-turn=` resume anchor sat below
/// the ledger's retained low-water, so turn records in `(anchor, floor)` were
/// drop-oldest evicted and are gone — the resumed subscriber missed them (re-read
/// `history` if it needs the content). The events-stream twin of `frame_gap`.
///
/// Returns a [`Frame`], not a `String`, because unlike its sibling formatters this
/// one is emitted STANDALONE rather than appended to a target's wake body — and a
/// standalone write is exactly what has to go through the egress to be stamped.
fn frame_gap_events(tag: Tag, floor: u64) -> Frame {
    Frame::text(tag, format!("GAP {tag} events-resync={floor}\n"))
}

/// Emit a `block-complete` EVENT for every already-sampled completed block,
/// advancing and returning the `last_block_id` watermark.
///
/// PURE (takes the `(id, exit)` tuples [`sample_engine_events`] cloned out under
/// the one lock hold): the SCAN moved there, so this side does only formatting
/// and the Terminal lock is never held across it. `completed` is oldest-first
/// and every id in it is strictly greater than `last_block_id`, so the fold's
/// `max` is just the last element — it is kept as a fold so the watermark can
/// never advance past a block whose line was not actually written.
fn drain_block_events(
    sid: &str,
    completed: &[(u64, Option<i32>)],
    last_block_id: Option<u64>,
    out: &mut String,
) -> Option<u64> {
    let mut high = last_block_id;
    for &(id, exit) in completed {
        out.push_str(&frame_block_complete(sid, id, exit));
        high = Some(high.map_or(id, |h| h.max(id)));
    }
    high
}

/// Everything the `events` digest reads out of the ENGINE for one target, taken
/// under ONE `term_lock` hold: the completed blocks past the watermark, the
/// window title, and the monotonic bell total.
///
/// WHY IT EXISTS — TWO SEPARATE COSTS, BOTH PAID ON EVERY IDLE TICK.
///
///  1. THE SCAN. The old `drain_block_events` walked `all_blocks()` — up to
///     `OUTPUT_BLOCKS_MAX` = 1000 records — and filtered by watermark, for every
///     watched target on every 250 ms wake, INCLUDING the bare liveness timeouts
///     where nothing has happened. Its own comment claimed the watermark made an
///     idle wake cheap; it made the OUTPUT empty, not the walk. Work scaled with
///     RETAINED HISTORY DEPTH rather than with the live window.
///
///  2. THE LOCK. Blocks, title and bell each took `term_lock` separately, so an
///     events-only subscription acquired the session's Terminal lock THREE times
///     per target per tick to learn that nothing changed — on the same lock the
///     renderer's frame snapshot and the keystroke-encode path contend for.
///
/// Both go away here. `newest_completed_block()` is the O(1) high-water of block
/// COMPLETION (`&self`, reverse `VecDeque` walk, `current_block` checked first),
/// and it is EXACT for this purpose: `all_blocks()` yields strictly increasing
/// ids, so the newest complete block also holds the largest complete id — if that
/// id is at or below the watermark, no completed block can be new, and the deque
/// is never touched. When something IS new, the walk runs BACKWARD from the
/// newest and breaks the moment it reaches the watermark, so it visits exactly
/// the un-reported suffix (plus any not-yet-complete blocks inside it — a block
/// can be archived without ever reaching OSC 133;D, e.g. a Ctrl-C'd prompt, so
/// the break is keyed on the ID, never on `is_complete()`), then reverses to
/// restore the oldest-first emission order the wire format has always used.
struct EngineSample {
    /// Blocks that COMPLETED past the watermark, oldest-first — empty on an idle
    /// wake, which is the point.
    blocks: Vec<(u64, Option<i32>)>,
    /// The live window title (`OSC 0/2`).
    title: String,
    /// The monotonic fired-bell count.
    bell: u64,
}

fn sample_engine_events(term: &Arc<Mutex<Terminal>>, last_block_id: Option<u64>) -> EngineSample {
    let t = crate::term_lock(term);
    let blocks = match t.newest_completed_block().map(|b| b.id) {
        // Nothing has ever completed, or nothing completed past the watermark:
        // the O(1) early-out. No deque walk, no allocation.
        None => Vec::new(),
        Some(hi) if last_block_id.is_some_and(|h| hi <= h) => Vec::new(),
        Some(_) => {
            let mut v: Vec<(u64, Option<i32>)> = Vec::new();
            for b in t.all_blocks().rev() {
                if last_block_id.is_some_and(|h| b.id <= h) {
                    break;
                }
                if b.is_complete() {
                    v.push((b.id, b.exit_code));
                }
            }
            v.reverse();
            v
        }
    };
    // Small, bounded clones OUT under the lock; every format!/push_str happens
    // above us with the guard already dropped, and the guard is never held
    // across a socket write (the caller writes; we only fill a String).
    EngineSample {
        blocks,
        title: t.title().to_string(),
        bell: t.bell_total(),
    }
}

/// Build the frames a single wake produces for one watched target, mutating its
/// send cursors. Returns the (possibly empty) frames for the caller to hand to
/// [`Egress::emit`]. PURE w.r.t. the socket — the caller does the write — so this is
/// unit-testable headlessly with no real connection.
///
/// It no longer applies the `T <sid> <t_us>` stamp: that moved to [`Egress`], which
/// is the only place that can see EVERY frame kind. This was the one site that ever
/// stamped anything, which is why four of the six write sites could not be stamped
/// at all.
///
/// COALESCING: a screen/cursor DELTA is emitted ONLY when `content_seq` ADVANCED
/// past `last_sent_seq`; an unchanged seq (e.g. a pure viewport scroll, which never
/// bumps `content_seq`) emits nothing. A wake always re-reads the LATEST state, so
/// N coalesced producer wakes collapse into ONE delta carrying the newest grid.
fn frames_for_watch(watch: &mut Watch, streams: TargetStreams, woke: bool) -> Vec<Frame> {
    let sid = sid_tag(watch.local_id);
    let mut out = String::new();

    if streams.wants_content() {
        // ONE lock hold gathers everything this wake needs. Cheap fields (seq, alt,
        // cursor) are read unconditionally; the EXPENSIVE ones (the full grid rows,
        // the styled snapshot) are cloned ONLY when this wake will actually emit a
        // content frame. Two payoffs:
        //   • Atomicity (R6): seq, cursor, rows and cells all come from a single
        //     consistent engine snapshot — a DELTA's seq can never pair with a grid
        //     or caret sampled a lock-acquisition later.
        //   • Efficiency (R5): an idle wake (unchanged seq, not an every-frame
        //     re-emit) pays only for the cheap reads — no full-grid clone, no styled
        //     gather. A 250ms liveness TIMEOUT (`woke == false`) never re-emits.
        let last_seq = watch.last_sent_seq;
        let last_render = watch.last_render_sig;
        // The render signature only drives the `screen`/`cells` re-emit on a
        // recolor, selection, DECSCNM, or styled-frame cursor change; the `cursor`
        // stream sources everything it emits from `cur` (incl. DECTCEM), so a
        // CURSOR-ONLY subscriber must NOT compute it — else it pays the palette
        // hash under the lock every wake AND spuriously re-emits an unchanged
        // caret on unrelated render state. Gate on screen||cells.
        let wants_render = streams.screen || streams.cells;
        let alt_flip_before = |alt: bool| alt != watch.last_alt;
        let (seq, alt, cur, sig, rows, styled) = {
            let t = crate::term_lock(&watch.term);
            let seq = t.content_seq();
            let alt = t.is_alternate_screen();
            // Cursor-only: reuse the last signature so `render_changed` is always false.
            let sig = if wants_render {
                render_sig(&t)
            } else {
                last_render
            };
            let cur = streams.cursor.then(|| {
                let c = t.cursor();
                (
                    c.row,
                    c.col,
                    t.cursor_visible(),
                    crate::control::cursor_style_name(t.cursor_style()),
                )
            });
            // A render-state change (recolor / selection / cursor / DECSCNM)
            // mutates the styled output at an UNCHANGED seq, so fold it into
            // content_change alongside the alt-swap resync so `cells`/`screen`
            // re-gather and push it.
            let render_changed = sig != last_render;
            let content_change = seq != last_seq || alt_flip_before(alt) || render_changed;
            // `every-frame` re-emits `cells` on an UNCHANGED seq for animation
            // fidelity — but ONLY on a genuine wake, never a bare liveness timeout.
            let every_frame = watch.non_coalesced && streams.cells && woke && !content_change;
            // Rows only for a full/gap frame (content changed); styled whenever we
            // will send cells (content changed OR an every-frame re-emit).
            let rows: Option<Vec<String>> = (content_change && streams.screen).then(|| {
                let n = t.rows() as usize;
                let mut rows: Vec<String> =
                    (0..n).map(|r| crate::control::visible_row(&t, r)).collect();
                // `trim`: the pushed twin of `text trim` — the frame stops after the
                // last non-blank row by the ONE shared rule, so a subscriber and a
                // poller agree on the row count. One O(rows) scan, no allocation.
                if streams.trim {
                    rows.truncate(crate::control::trimmed_len(rows.iter().map(String::as_str)));
                }
                rows
            });
            let styled = ((content_change || every_frame) && streams.cells)
                .then(|| crate::control::gather_styled_frame(&t));
            (seq, alt, cur, sig, rows, styled)
        };
        // Serialize the styled snapshot with the lock RELEASED (expensive per-cell
        // JSON/base64) so it never blocks keystroke encode or a frame snapshot.
        let cells_payload = styled.as_ref().map(crate::control::serialize_styled_frame);

        // Emit screen + cursor + cells from the single snapshot above (used by the
        // GAP-resync and forward-advance branches). Each stream is gated by whether
        // its payload was gathered, so this respects the requested stream set.
        let emit = |out: &mut String, seq: u64| {
            if let Some(r) = rows.as_ref() {
                out.push_str(&frame_screen(&sid, seq, r));
            }
            if let Some((cr, cc, cv, cs)) = cur {
                out.push_str(&frame_cursor(&sid, seq, cr, cc, cv, cs));
            }
            if let Some(p) = cells_payload.as_ref() {
                out.push_str(&frame_cells(&sid, seq, p));
            }
        };
        // An alt-screen swap (1049) does not move the main grid's content_seq, so a
        // swap landing on the SAME seq is a full-screen change a seq compare misses.
        // Treat a flag flip like a backward seq: resync (GAP + full frame).
        let alt_flip = alt != watch.last_alt;
        let render_changed = sig != watch.last_render_sig;
        let mut content_framed = true;
        if seq < watch.last_sent_seq || alt_flip {
            // The engine's content seq moved BACKWARD (reset / engine rebuild) OR the
            // alt buffer swapped: the client's prior delta cursor is meaningless —
            // signal a resync and send a full frame.
            out.push_str(&frame_gap(&sid, seq));
            watch.last_sent_seq = seq;
            watch.last_alt = alt;
            emit(&mut out, seq);
        } else if seq > watch.last_sent_seq || render_changed {
            // Forward content advance, OR a same-seq RENDER change (recolor /
            // selection / cursor / DECSCNM) that mutated the styled output: emit
            // a fresh full frame. No GAP for a render-only change — the client's
            // seq cursor is still valid, only presentation state moved.
            watch.last_sent_seq = seq;
            watch.last_alt = alt;
            emit(&mut out, seq);
        } else if let Some(p) = cells_payload.as_ref() {
            // `every-frame` mode: re-emit the styled frame on an unchanged seq so a
            // fast-repainting TUI's transient states are observable (animation
            // fidelity). Only `cells` re-emits; screen/cursor stay coalesced.
            out.push_str(&frame_cells(&sid, seq, p));
            content_framed = false;
        } else {
            content_framed = false;
        }
        // Sync the render watermark after handling, so the NEXT wake only re-emits on
        // a further render change.
        watch.last_render_sig = sig;

        // CURSOR: a content frame above already carried the current cursor, so sync
        // the watermark. Otherwise a pure caret reposition / DECSCUSR style flip /
        // DECTCEM visibility toggle — which bumps no content_seq — would starve the
        // `cursor` stream: emit a DELTA (stamped at the current seq) when the caret
        // datum changed with no content frame this wake.
        if let Some(cur) = cur {
            if content_framed {
                watch.last_cursor = cur;
            } else if cur != watch.last_cursor {
                let (cr, cc, cv, cs) = cur;
                out.push_str(&frame_cursor(&sid, seq, cr, cc, cv, cs));
                watch.last_cursor = cur;
            }
        }
    }

    if streams.events {
        // ONE Terminal-lock hold covers all three ENGINE-sourced sources (blocks,
        // title, bell); the turn ledger and the event timeline have their own
        // small mutexes and each is now gated by an O(1) high-water compare. An
        // events-only subscription's idle wake is therefore: one lock acquisition
        // + three integer compares, where it used to be three lock acquisitions
        // and a walk over ~2000 retained records per target.
        //
        // EMISSION ORDER IS UNCHANGED — blocks, turns, timeline (meta, then the
        // closing row), title, bell — which is the only order the wire has ever
        // promised. What did move is the SAMPLING INSTANT of title and bell:
        // they are now read at the top of the events pass instead of the
        // bottom. Each of the five sources carries its own independent
        // watermark and none is derived from another, so the only effect is
        // which side of a 250 ms tick boundary a title/bell change that races
        // the pass lands on — the same one-tick skew the old order had in the
        // opposite direction, and a skew every watermark-driven stream here
        // already tolerates by construction.
        let engine = sample_engine_events(&watch.term, watch.last_block_id);
        watch.last_block_id =
            drain_block_events(&sid, &engine.blocks, watch.last_block_id, &mut out);
        watch.last_turn_id = drain_turn_events(&watch.turns, &sid, watch.last_turn_id, &mut out);
        watch.last_timeline_id =
            drain_timeline_events(&watch.timeline, &sid, watch.last_timeline_id, &mut out);
        watch.last_title =
            drain_title_event(&sid, &engine.title, watch.last_title.take(), &mut out);
        watch.last_bell = drain_bell_event(&sid, engine.bell, watch.last_bell, &mut out);
    }

    if out.is_empty() {
        return Vec::new();
    }
    vec![Frame::text(Tag::Channel(watch.local_id), out)]
}

/// A resolved subscribe target tuple, cloned OUT of the store at subscribe time:
/// the process-local id, the live engine handle, the session's live byte
/// fan-out (for the `bytes` stream — a `subscribe` registers on it lazily), its
/// turn ledger, and its event timeline (the `events` digest's meta-push source).
pub type ResolvedTarget = (
    u64,
    Arc<Mutex<Terminal>>,
    Arc<ByteFanout>,
    Arc<Mutex<TurnLedger>>,
    Arc<Mutex<crate::session_timeline::SessionTimeline>>,
);

/// Build one target's send cursors — the per-`Watch` seeding, in ONE place.
///
/// It was inline in [`push_loop`]'s `map`, which was fine while the target set
/// was frozen at subscribe time. A `@*` subscription ADOPTS sessions later, and
/// an adopted watch has to be seeded by exactly the same rules or it would
/// replay a backlog (empty watermarks) or start blind (wrong watermarks). One
/// function is the only way to keep those two paths from drifting.
///
/// The `opts.since*` resume anchors are per-session and are refused for a
/// multi-target subscription (and for `@*` outright), so in adopt mode they are
/// always `None` here and this function behaves identically for both callers.
fn new_watch(target: &ResolvedTarget, streams: TargetStreams, opts: &PushOptions) -> Watch {
    let (id, term, fanout, turns, timeline) = target;
    Watch {
        local_id: *id,
        term: term.clone(),
        turns: turns.clone(),
        // `since-turn=<id>` resumes the turn stream from that id (push turns
        // with id > since_turn); absent it, seed to the live high so only
        // post-subscription turns push. Only meaningful with the events stream.
        last_turn_id: opts
            .since_turn
            .or_else(|| initial_turn_watermark(turns, streams)),
        timeline: timeline.clone(),
        // Seed to the live timeline high so only meta changes AFTER
        // subscription push (a live stream, like turns/blocks/title).
        last_timeline_id: initial_timeline_watermark(timeline, streams),
        // Seed to the live title so a fresh `events` subscriber gets title
        // CHANGES from here, not a spurious event for the title already showing.
        last_title: streams
            .events
            .then(|| crate::term_lock(term).title().to_string()),
        // Seed to the live bell total so a fresh subscriber gets bells from here.
        last_bell: if streams.events {
            crate::term_lock(term).bell_total()
        } else {
            0
        },
        last_sent_seq: opts.since.unwrap_or(0),
        // Seed to the live alt-screen state so a subscriber that connects while
        // a TUI is already on the alt buffer does not spuriously resync on its
        // first wake; a genuine swap after this flips it.
        last_alt: if streams.wants_content() {
            crate::term_lock(term).is_alternate_screen()
        } else {
            false
        },
        // Seed to a sentinel so the FIRST content/cursor wake always emits the
        // caret (a fresh subscriber has no prior cursor); real positions differ.
        last_cursor: (u16::MAX, u16::MAX, false, ""),
        // Seed the render signature to the LIVE value so a subscriber does
        // not spuriously resync on its first wake; a later recolor,
        // selection/cursor change, or DECSCNM flip changes it. Only
        // meaningful for a content-bearing subscription.
        last_render_sig: if streams.wants_content() {
            render_sig(&crate::term_lock(term))
        } else {
            0
        },
        // Seed the block watermark to the CURRENT high so we only push blocks
        // that COMPLETE after subscription, never the historical backlog —
        // `events` is a live stream, not a replay (matches `since` for screen).
        last_block_id: opts
            .since_block
            .or_else(|| initial_block_watermark(term, streams)),
        // Register on the byte fan-out ONLY when `bytes` is requested, so an
        // idle/unsubscribed session pays nothing for the live byte channel.
        byte_sub: if streams.bytes {
            Some(fanout.subscribe())
        } else {
            None
        },
        non_coalesced: opts.non_coalesced,
    }
}

/// The PUSH LOOP for a `subscribe` connection. The connection has already
/// AUTHORIZED every target via the control gate; here we just register for wakes,
/// emit an immediate catch-up (so a fresh subscriber sees the current screen) and
/// optionally honor `since=<seq>`, then block on the registry notify and push a
/// coalesced frame on each wake until the client disconnects (write fails) or the
/// loop is asked to stop.
///
/// PUSH-ONLY: once here, the connection never reads another request line. The
/// writer is wrapped in the connection's one [`Egress`] and is the ONLY thing this
/// loop touches on the socket. A write failure (broken pipe / slow-then-dead client)
/// ends the loop and drops the [`Subscription`] (deregistering), so the producer
/// never pays for a dead subscriber.
///
/// `opts.since` (optional, applied per target): the client's last-seen
/// `content_seq`. If the live content has advanced past it, the first wake's compare
/// already emits a catch-up DELTA; we seed each watch's `last_sent_seq` to it so the
/// immediate catch-up fires exactly when content moved past `since`.
///
/// The two stream sets are SEPARATE parameters because they were authorized by two
/// different checks. `streams` is per-target: every entry of `targets` passed the
/// `ReadScreen` gate individually. `instance` is connection-wide and can only be
/// built by [`InstanceStreams::authorize`], which is why the roster read below
/// cannot be reached by a subscriber that only proved per-target authority.
/// The three authorization tokens a push connection carries, kept together
/// because they are checked in three different places and must travel as a set.
///
/// `streams` is PER-TARGET: every entry of `targets` passed the `ReadScreen` gate
/// individually. `instance` is CONNECTION-WIDE and can only be built by
/// [`InstanceStreams::authorize`]. `adopt` gates late-joining targets. Bundling
/// them is not just argument-count hygiene: a caller can no longer pass two of
/// the three and silently default the third, because the struct has no `Default`
/// and every field must be named at the construction site.
#[derive(Clone, Copy)]
pub struct PushScopes {
    /// Per-target `ReadScreen` authority, one check per entry of `targets`.
    pub streams: TargetStreams,
    /// Connection-wide authority; the only key to the whole-instance roster.
    pub instance: InstanceStreams,
    /// Whether targets that appear AFTER subscription may be adopted.
    pub adopt: AdoptScope,
}

/// The two watermarks a push connection advances as it runs: the roster cursor
/// (whole-instance lifecycle) and the adopt sequence (late-joining targets).
/// Paired so [`pump`] takes one `&mut` instead of two — they are advanced together
/// on every wake, and a caller holding only one of them would be describing an
/// inconsistent point in the stream.
struct PushCursors {
    roster: Option<RosterCursor>,
    adopt_seq: u64,
}

#[cfg(test)]
pub fn push_loop<W: Write>(
    registry: &Subscribers,
    store: &Store,
    targets: &[ResolvedTarget],
    scopes: PushScopes,
    opts: PushOptions,
    writer: &mut W,
) {
    push_loop_with_peer_probe(registry, store, targets, scopes, opts, writer, || false);
}

/// The production push loop with an explicit peer-liveness probe.
///
/// A push-only connection can remain completely quiet for hours. A dead reader
/// therefore cannot be discovered by waiting for the next write: doing so pins a
/// reserved subscription worker forever when the target is quiet. `peer_gone` is
/// sampled on every bounded liveness wake and must be non-blocking (or tightly
/// bounded). The control-socket host supplies a read-side EOF/HUP probe; the
/// generic [`push_loop`] wrapper keeps in-memory/test writers source-compatible.
pub fn push_loop_with_peer_probe<W: Write, P: FnMut() -> bool>(
    registry: &Subscribers,
    store: &Store,
    targets: &[ResolvedTarget],
    scopes: PushScopes,
    opts: PushOptions,
    writer: &mut W,
    mut peer_gone: P,
) {
    // `adopt` is deliberately NOT unpacked here: adoption is decided per wake
    // inside `pump`, which receives the whole `scopes` value. Naming it in this
    // scope too would invite a second, divergent read of the same authority.
    let PushScopes {
        streams, instance, ..
    } = scopes;
    let local_ids: Vec<u64> = targets.iter().map(|(id, _, _, _, _)| *id).collect();
    let mut sub = SubscriberSet::register(registry, &local_ids);

    // INSTANCE lifecycle watermark: seed to the CURRENT journal high (and the
    // current live set, for the recovery path) so only spawns/exits AFTER
    // subscription are pushed — a fresh subscriber `ls`s for the baseline. The
    // 250ms bounded wait below already re-polls, so a sibling spawn surfaces
    // within one tick; no separate notify wiring is needed. This is the
    // WHOLE-INSTANCE roster, hence the `InstanceStreams` gate on reaching it at
    // all.
    //
    // BOTH fields are read under ONE guard. That is not cosmetic: taking the
    // seq and the set in two separate lock acquisitions would let a spawn land
    // between them and be counted twice (once in the baseline set, once again
    // when the journal replayed it), which is exactly the class of bug a
    // watermark is supposed to remove.
    let roster = instance.sessions().then(|| {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        RosterCursor {
            seq: g.roster_seq(),
            known: g.live_sids(),
        }
    });

    // Build the per-target send cursors. `since` seeds `last_sent_seq` so the
    // IMMEDIATE catch-up below fires exactly when the live content advanced past
    // the client's last-seen seq; otherwise we start at 0 (a brand-new subscriber
    // gets a full snapshot on the first wake / immediate pass).
    let mut watches: Vec<Watch> = targets
        .iter()
        .map(|t| new_watch(t, streams, &opts))
        .collect();

    let mut egress = Egress::new(writer, opts.timestamps);
    // `Gone` is not a failure to report: the client hanging up IS how a push-only
    // connection ends. Returning here drops `sub`, which deregisters this subscriber
    // from every session it watched.
    // ADOPTION WATERMARK, seeded at ZERO on purpose. The initial target set was
    // resolved from a store snapshot taken in the control dispatch, strictly
    // BEFORE this point, so a session created in between belongs to neither the
    // snapshot nor a delta anchored at "now". Anchoring at 0 makes the first
    // adoption pass replay the retained journal, which is bounded, happens once,
    // and is self-correcting: a record for a session already watched (or already
    // gone) resolves to nothing and is skipped.
    let mut cursors = PushCursors {
        roster,
        adopt_seq: 0,
    };
    let _ = pump(
        store,
        &mut sub,
        &mut watches,
        scopes,
        &opts,
        &mut cursors,
        &mut egress,
        &mut peer_gone,
    );
}

/// The push loop proper: the catch-up wake, then one wake per notify (or per bounded
/// timeout) until the client hangs up or every watched session has closed.
///
/// Split out of [`push_loop`] purely so `?` on [`Gone`] can carry the "client is
/// gone" exit, which is what collapses the six copies of
/// `if writer.write_all(..).is_err() { return; }` the old body carried — the shape
/// that let each write site disagree about whether it stamped, flushed or drained.
/// (It cannot be `push_loop` itself: `Gone` is private and `push_loop` is `pub`.)
#[allow(
    clippy::too_many_arguments,
    reason = "the internal pump borrows each independently-owned subscription cursor and the nonblocking peer probe"
)]
fn pump<W: Write, P: FnMut() -> bool>(
    store: &Store,
    sub: &mut Subscription,
    watches: &mut Vec<Watch>,
    scopes: PushScopes,
    opts: &PushOptions,
    cursors: &mut PushCursors,
    egress: &mut Egress<'_, W>,
    peer_gone: &mut P,
) -> Result<(), Gone> {
    // The same authority set `push_loop` was handed, carried whole rather than
    // re-split into the two tokens this body happens to read today: a later wake
    // that needs `instance` must not have to widen the signature to get it, and a
    // caller must never be able to hand `pump` a scope set the connection did not
    // actually prove.
    let PushScopes { streams, adopt, .. } = scopes;
    let PushCursors { roster, adopt_seq } = cursors;
    let since_turn = opts.since_turn;
    // The catch-up is a wake like any other, so it opens one: its frames share a
    // stamp ledger, and its flush is `end_wake`.
    egress.begin_wake();

    // EVENTS-RESUME GAP: a `since-turn=<n>` anchor BELOW the ledger's retained
    // low-water means turn records were drop-oldest evicted between the anchor and the
    // window — signal it so the resumed subscriber knows it missed some (turn ids are
    // process-global, so a per-session id gap can't reveal the loss). Resume anchors
    // are single-target (R4), so at most one watch matches.
    if streams.events
        && let Some(anchor) = since_turn
    {
        for w in watches.iter() {
            let low = w.turns.lock().unwrap_or_else(|p| p.into_inner()).low_id();
            if let Some(low) = low
                && anchor + 1 < low
            {
                egress.emit(frame_gap_events(Tag::Channel(w.local_id), low))?;
            }
        }
    }

    // IMMEDIATE catch-up: emit the current state once so a fresh subscriber is not
    // blind until the next output burst. With `since`, this fires a DELTA only if
    // content already advanced past `since`; without it, it sends the full screen.
    // (The `bytes` stream has no backlog to replay — it is live from this point.)
    for w in watches.iter_mut() {
        // `woke = true`: the immediate catch-up is a genuine first emit, not a
        // liveness timeout, so an every-frame subscriber gets its opening cells frame.
        for f in frames_for_watch(w, streams, true) {
            egress.emit(f)?;
        }
    }
    egress.end_wake()?;

    loop {
        // A bounded wait: on a real wake we push immediately; on a timeout we still
        // loop (a no-op pass that costs one cheap content_seq compare per target and
        // lets a half-closed socket surface via the next write). The producer never
        // waits on us regardless (single-slot notify), so this interval only bounds
        // OUR own liveness, not the producer's.
        let woke = sub.wait(Duration::from_millis(250));
        // A quiet stream performs no writes, so only the socket's read-side
        // EOF/HUP can release its reserved worker after the reader disappears.
        // The client contract keeps its write half open for the subscription's
        // lifetime; any byte or EOF on this push-only channel ends the stream.
        if peer_gone() {
            return Err(Gone);
        }
        egress.begin_wake();

        // CLOSINGS FIRST. A target deregistered from the store (its pane closed) must
        // leave the watch set so we stop reading a dead engine — but its buffered TAIL
        // is delivered before it goes, because `prune_closed` hands the watch back
        // instead of dropping it and `Closing::drain` consumes it by value. Draining
        // here rather than in the live pass below also closes the residual window the
        // live pass leaves: bursts teed BETWEEN a live drain and the liveness re-check
        // were still lost, and on the pane-close path that is not a race —
        // `app_tabs` issues no subscriber notify at all, so the death is always
        // discovered on our own 250ms tick with a full tick of bytes queued.
        for closing in prune_closed(store, watches) {
            // Leave the notify registry too. Harmless-but-wasteful on a
            // fixed-target subscription (it ends soon after its last target
            // dies); load-bearing on a `@*` one, which outlives an unbounded
            // number of sessions and would otherwise accumulate a registry
            // entry per dead session for the producer's `notify` to walk.
            sub.unwatch(closing.local_id());
            for f in closing.drain(streams) {
                egress.emit(f)?;
            }
        }

        // ADOPT: a `@*` subscription's target set is LIVE — sessions created
        // after it subscribed join it here, each acked with the same
        // `sub <local> <sid>` line the initial handshake emits, so a client
        // demultiplexes an adopted channel exactly the way it does an original
        // one (the shipped bridge already reads `sub` lines anywhere in the
        // stream, because the ack was never guaranteed to arrive in one read).
        if adopt.on() {
            for f in adopt_new_targets(store, watches, sub, streams, opts, adopt_seq) {
                egress.emit(f)?;
            }
        }

        // INSTANCE lifecycle (connection-level, once per wake): a sibling spawn/exit
        // the subscriber is not watching, so a fleet supervisor need not poll `ls`.
        if let Some(cursor) = roster.as_mut() {
            for f in drain_session_events(store, cursor) {
                egress.emit(f)?;
            }
        }

        // The "everything closed" exit sits BELOW both drains above, never between
        // them: the last watch's tail and the `EVENT * session-exited` that reports
        // its death are produced by exactly the wake that discovers it, and returning
        // above them would have discarded both.
        // ...but a `@*` subscription's empty set is NOT the end of the stream: it
        // is an instance that momentarily has no sessions, and the whole point of
        // the live target set is that the next one gets adopted. A fixed-target
        // subscription still ends here, unchanged.
        if watches.is_empty() && !adopt.on() {
            return egress.end_wake();
        }

        // Per watch: the UTF-8 text/cells frames, then the RAW binary byte frames.
        // Writing per-watch keeps each session's frames contiguous; the byte frames
        // are length-prefixed so a client demuxes text vs binary unambiguously.
        for w in watches.iter_mut() {
            for f in frames_for_watch(w, streams, woke) {
                egress.emit(f)?;
            }
            if streams.bytes {
                for f in drain_bytes_frames(w) {
                    egress.emit(f)?;
                }
            }
        }
        egress.end_wake()?;
    }
}

/// The block-id watermark to start a fresh `events` subscription at: the current
/// highest completed block id (so only blocks completing AFTER subscription are
/// pushed). `None` when the `events` stream is not requested or no block has
/// completed yet.
fn initial_block_watermark(term: &Arc<Mutex<Terminal>>, streams: TargetStreams) -> Option<u64> {
    if !streams.events {
        return None;
    }
    // IDENTICAL to the `all_blocks().filter(is_complete).map(id).max()` this
    // replaces: `all_blocks()` yields strictly increasing ids, so the newest
    // COMPLETE block by position is also the largest complete id — which is
    // what `newest_completed_block` returns, in O(1) instead of a walk over up
    // to `OUTPUT_BLOCKS_MAX` records. One seed per subscription, but it is the
    // same identity the per-tick early-out in `sample_engine_events` rests on,
    // so stating it once, here, keeps the two from drifting apart.
    crate::term_lock(term)
        .newest_completed_block()
        .map(|b| b.id)
}

/// Seed the turn watermark to the ledger's current high so the `events` digest
/// streams only turns that COMPLETE after subscription (live, never the backlog —
/// mirrors `initial_block_watermark`). `None` when `events` was not requested.
fn initial_turn_watermark(turns: &Arc<Mutex<TurnLedger>>, streams: TargetStreams) -> Option<u64> {
    if !streams.events {
        return None;
    }
    turns.lock().unwrap_or_else(|p| p.into_inner()).high_id()
}

/// The timeline twin of [`initial_turn_watermark`]: seed the timeline scan to the
/// CURRENT timeline high so only post-subscription records (a `meta-change`, the
/// `closing` row) push — never the `spawned` history.
fn initial_timeline_watermark(
    timeline: &Arc<Mutex<crate::session_timeline::SessionTimeline>>,
    streams: TargetStreams,
) -> Option<u64> {
    if !streams.events {
        return None;
    }
    timeline.lock().unwrap_or_else(|p| p.into_inner()).high_id()
}

/// ADOPT every live session this `@*` subscription is not already watching —
/// driven by the STORE'S ROSTER JOURNAL, so an idle wake costs one integer
/// compare and a busy one costs O(sessions created).
///
/// THE COST CATEGORY THIS DELETES. `subscribe` froze its target list at
/// subscribe time, and the push loop is documented PUSH-ONLY — it never reads
/// another request line — so there was NO way to add a session to a live
/// subscription. A federation bridge that wanted a newly-opened tab therefore
/// had to open a WHOLE NEW subscription for it: a fresh child process, a fresh
/// UDS connection, and a fresh server push thread, once per DISCOVERY MOMENT
/// rather than once per instance. The server admits
/// `CONTROL_SUBSCRIPTION_WORKERS` = 4 of those at a time, so the fifth tab-open
/// was refused — and refused silently, because a push-only client that has
/// already marked the sid as seen never asks again. Past four staggered
/// discoveries, sessions simply stopped being federated. One live target set
/// replaces all of it with one connection per instance.
///
/// WHY IT RIDES THE SAME JOURNAL THE `sessions` STREAM DOES. "Which sessions am
/// I not watching?" and "which sessions appeared?" are the same question asked
/// by two consumers, and answering it by rebuilding the whole roster is what
/// made the second one expensive in the first place. The journal answers both
/// from one monotonic log: `roster_seq()` says whether anything moved at all,
/// and `roster_since` names exactly what did. So adoption does not re-introduce
/// the per-tick whole-registry walk the roster cursor just deleted.
///
/// DISCIPLINE. Clone-then-release: handles are cloned out under the store read
/// guard, which is dropped before `new_watch` takes any `Terminal` lock and
/// before any write. The `MAX_SUBSCRIBE_TARGETS` cap that bounds an explicit
/// selector list bounds this too — a subscription cannot fan out past it by
/// living a long time. When the cap is reached adoption is DEFERRED, not
/// dropped: the watermark is NOT advanced, so a later wake with a free slot
/// picks the same session up, and an Owner that also asked for the `sessions`
/// stream saw its `session-created` immediately regardless.
fn adopt_new_targets(
    store: &Store,
    watches: &mut Vec<Watch>,
    sub: &mut Subscription,
    streams: TargetStreams,
    opts: &PushOptions,
    adopt_seq: &mut u64,
) -> Vec<Frame> {
    let room = crate::control::MAX_SUBSCRIBE_TARGETS.saturating_sub(watches.len());
    if room == 0 {
        return Vec::new();
    }
    let fresh: Vec<(ResolvedTarget, String)> = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        let high = g.roster_seq();
        if high == *adopt_seq {
            // THE IDLE TICK: nothing has entered or left the registry.
            return Vec::new();
        }
        let unwatched = |h: &&crate::session_store::SessionHandle| {
            !watches.iter().any(|w| w.local_id == h.local_id)
        };
        let carve = |h: &crate::session_store::SessionHandle| {
            (
                (
                    h.local_id,
                    h.term.clone(),
                    h.ctx.byte_fanout.clone(),
                    h.ctx.turns.clone(),
                    h.ctx.timeline.clone(),
                ),
                h.sid.as_str().to_string(),
            )
        };
        let mut picked: Vec<(ResolvedTarget, String)> = match g.roster_low_seq() {
            // FAST PATH: resolve only the sids the journal says were CREATED.
            // An `Exited` record needs nothing — `prune_closed` above already
            // retired that watch — and a Created sid that has since exited
            // resolves to `None` and is skipped, which is what makes replaying
            // a stale watermark safe.
            Some(low) if adopt_seq.saturating_add(1) >= low => g
                .roster_since(*adopt_seq)
                .filter(|r| r.change == crate::session_store::RosterChange::Created)
                .filter_map(|r| g.by_sid(&aterm_session::SessionId::new(&r.sid)))
                .filter(|h| unwatched(h))
                .map(carve)
                .collect(),
            // RECOVERY: the watermark fell past the journal's retained window
            // (or this is the very first pass on an instance whose journal has
            // already rolled). Walk the registry once — the pre-journal cost,
            // paid only where the journal genuinely cannot answer.
            _ => g
                .live_handles()
                .filter(|h| unwatched(h))
                .map(carve)
                .collect(),
        };
        // ADVANCE ONLY IF WE TOOK EVERYTHING. Truncating to the cap and moving
        // the watermark anyway would put the deferred sessions permanently
        // behind it — the exact "seen, therefore never retried" bug that made
        // the old bridge lose sessions.
        if picked.len() <= room {
            *adopt_seq = high;
        } else {
            picked.truncate(room);
        }
        picked
        // guard drops here, BEFORE `new_watch` takes any Terminal lock
    };
    let mut out = Vec::with_capacity(fresh.len());
    for (target, sid) in fresh {
        // Register for wakes BEFORE building the watch, so a burst that lands
        // between the two still wakes us. (It could not be lost either way — a
        // wake re-reads the session's CURRENT state — but registering second
        // would mean the first burst waited for the 250 ms liveness tick.)
        sub.watch(target.0);
        out.push(Frame::text(
            Tag::Instance,
            format!("sub {} {sid}\n", target.0),
        ));
        watches.push(new_watch(&target, streams, opts));
    }
    out
}

/// A watch whose session has left the store but whose buffered TAIL has not been
/// emitted yet. Obtainable only from [`prune_closed`], consumable only by
/// [`Closing::drain`] — which takes `self` BY VALUE, so the [`ByteSubscription`]
/// inside is dropped strictly AFTER its queue has been read.
///
/// The point is that "closed, tail not yet delivered" is now a REPRESENTABLE state.
/// It previously existed only as a comment about statement order above a
/// `Vec::retain`, which is why moving the prune one block up silently destroyed
/// every queued burst and the final content DELTA, leaving the client with a bare
/// socket EOF unless it happened to have asked for the `events` stream.
#[must_use = "a closing watch still holds an undelivered tail — call drain()"]
struct Closing(Watch);

impl Closing {
    /// The final frames for a dying target, in protocol order: one last content /
    /// events pass, then every byte burst still queued, then `EVENT <sid> exited`.
    ///
    /// Reading the target's `Terminal` AFTER the store deregistered it is deliberate
    /// and sound: `Watch.term` is an independent `Arc` clone taken at subscribe time,
    /// and [`crate::session_store`] records the death mark BEFORE the handle leaves
    /// the registry precisely so a holder that kept the handle — naming "a live
    /// subscribe watch" — still reads an honest final event. That final event is
    /// the `closing reason= by=` row, and the events pass here is the only path
    /// that delivers it (`EVENT <sid> closing …`, then the `exited` marker below):
    /// the `timeline` verb cannot resolve a deregistered sid, so a driver that was
    /// not already subscribed asks `exits` instead.
    ///
    /// `woke = false`: this pass exists to deliver state the client has not SEEN, not
    /// to re-emit. An `every-frame` re-emit at an unchanged seq would only append a
    /// duplicate frame to a session that is already gone.
    /// The process-local id of the watch inside — read BEFORE `drain` consumes
    /// it, so the notify registry can be cleaned up in the same pass.
    fn local_id(&self) -> u64 {
        self.0.local_id
    }

    fn drain(mut self, streams: TargetStreams) -> Vec<Frame> {
        let tag = Tag::Channel(self.0.local_id);
        let mut out = frames_for_watch(&mut self.0, streams, false);
        out.extend(drain_bytes_frames(&self.0));
        if streams.events {
            out.push(Frame::text(tag, frame_exited(&sid_tag(self.0.local_id))));
        }
        out
    }
}

/// Partition the watch set by liveness: live watches stay in `watches`, and the ones
/// whose session has been DEREGISTERED from the store (their pane closed) are HANDED
/// BACK as [`Closing`] values rather than dropped. It decides only WHO died — it lost
/// its `TargetStreams` parameter along with the `EVENT … exited` it used to format,
/// because deciding what a dead target still owes the client is `Closing::drain`'s
/// job, and keeping the two together is what makes the tail impossible to skip.
///
/// `mem::take` + `partition` rather than `retain` for the same reason: `retain` drops
/// the removed watches in place, and a dropped `Watch` drops the `ByteSubscription`
/// it owns, whose `Drop` frees every burst teed since the last drain.
#[must_use]
fn prune_closed(store: &Store, watches: &mut Vec<Watch>) -> Vec<Closing> {
    let g = store.read().unwrap_or_else(|p| p.into_inner());
    // Almost every wake has zero deaths. Probe first so that case stays a scan and
    // does not reallocate the whole watch set 4×/sec per subscriber.
    if watches.iter().all(|w| g.by_local(w.local_id).is_some()) {
        return Vec::new();
    }
    let (dead, live): (Vec<Watch>, Vec<Watch>) = std::mem::take(watches)
        .into_iter()
        .partition(|w| g.by_local(w.local_id).is_none());
    *watches = live;
    dead.into_iter().map(Closing).collect()
}

/// BENCH-ONLY driver for the `events` DIGEST (`benches/subscribe_digest.rs`) —
/// the `bench_support` precedent applied inside this module.
///
/// WHY IT LIVES HERE. The thing that has to be timed is [`frames_for_watch`],
/// the per-target per-wake body the push loop runs, and the [`Watch`] it mutates
/// — both module-private, as they should be. A bench is an EXTERNAL target and
/// sees neither. This module is the one seam they are driven through; it is
/// gated on the `bench-support` feature, which no shipping build enables, and it
/// contains NO logic of its own beyond fixture construction: `wake` calls the
/// shipping function directly, in the same loop shape `pump` uses.
///
/// WHAT THE FIXTURE HAS TO GET RIGHT. A `Watch` seeded with `None` watermarks
/// would emit the entire retained backlog on its first wake and then be silent —
/// which would price the wrong thing twice over. So the watermarks here are
/// seeded exactly as [`push_loop`] seeds them (live highs, so `events` is a live
/// stream and never a replay), and the bench PROVES that with a two-sided guard:
/// an idle wake must produce zero bytes, and a wake after a real ledger append
/// must produce a frame.
#[cfg(feature = "bench-support")]
pub(crate) mod bench_seam {
    use super::{TargetStreams, Watch, frames_for_watch};
    use crate::session_timeline::SessionTimeline;
    use crate::turn_ledger::{TurnLedger, TurnRecord};
    use aterm_core::terminal::Terminal;
    use std::sync::{Arc, Mutex};

    /// One shell-integration command cycle: prompt, command, output, exit 0.
    /// Two archived rows per cycle, one `OutputBlock` per cycle.
    const CYCLE: &[u8] =
        b"\x1b]133;A\x07$ \x1b]133;B\x07cmd\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07";

    /// K watched targets whose three per-target event ledgers are filled to a
    /// requested RETAINED DEPTH — the scaling variable the digest's cost was
    /// found to track.
    pub(crate) struct DigestFixture {
        watches: Vec<Watch>,
        streams: TargetStreams,
        terms: Vec<Arc<Mutex<Terminal>>>,
        turns: Vec<Arc<Mutex<TurnLedger>>>,
        timelines: Vec<Arc<Mutex<SessionTimeline>>>,
        next_turn_id: u64,
    }

    impl DigestFixture {
        /// `targets` watches, each carrying `blocks` shell blocks, `turns` turn
        /// records and `timeline` timeline events. Depths above the shipping caps
        /// simply saturate — [`Self::retained`] reports what was actually reached
        /// so the bench can assert on the real numbers rather than the asked-for
        /// ones.
        /// NOT named `new`: the lock-order census resolves a held one-hop call
        /// by callee NAME, so a `fn new` here captured every `Vec::new()` and
        /// `Scrollback::new()` made under a held `term` guard and reported an
        /// OB-7 re-entrancy suspect against this fixture's own private engine.
        /// The distinct name is what keeps that census honest.
        pub(crate) fn build(targets: usize, blocks: usize, turns: usize, timeline: usize) -> Self {
            let mut f = DigestFixture {
                watches: Vec::with_capacity(targets),
                // EVENTS ONLY — the shape the finding is about. A content-bearing
                // subscription takes its own Terminal lock for the grid; an
                // events-only one used to take three purely to learn nothing had
                // changed, and that is the cost being priced.
                streams: TargetStreams {
                    events: true,
                    ..Default::default()
                },
                terms: Vec::with_capacity(targets),
                turns: Vec::with_capacity(targets),
                timelines: Vec::with_capacity(targets),
                next_turn_id: 0,
            };
            for i in 0..targets {
                // `fixture_term`, not `term`: the census's identity is the
                // receiver NAME, so a local called `term` would MERGE with the
                // shipping session mutex and put this private, never-shared
                // engine into the production lock graph.
                let fixture_term = Arc::new(Mutex::new(Terminal::new(24, 80)));
                {
                    let mut t = fixture_term.lock().expect("fresh engine");
                    for _ in 0..blocks {
                        t.process(CYCLE);
                    }
                    // A settled title, so the title watermark seeds to a real value.
                    t.process(b"\x1b]2;bench\x07");
                }
                let ledger = Arc::new(Mutex::new(TurnLedger::default()));
                {
                    let mut l = ledger.lock().expect("fresh ledger");
                    for _ in 0..turns {
                        f.next_turn_id += 1;
                        l.push(turn_record(f.next_turn_id));
                    }
                }
                let tl = Arc::new(Mutex::new(SessionTimeline::default()));
                {
                    let mut t = tl.lock().expect("fresh timeline");
                    for j in 0..timeline {
                        // A realistic mix: the digest FILTERS for `meta-change`,
                        // so an all-`meta-change` fixture would flatter a design
                        // that scanned the whole ring, and an all-other fixture
                        // would never exercise the emit path.
                        if j % 8 == 0 {
                            t.record("meta-change", "field=role value=lead".to_string());
                        } else {
                            t.record("state-change", "state=alive".to_string());
                        }
                    }
                }
                f.watches
                    .push(seeded_watch(i as u64, &fixture_term, &ledger, &tl));
                f.terms.push(fixture_term);
                f.turns.push(ledger);
                f.timelines.push(tl);
            }
            f
        }

        /// ONE wake: the shipping per-target body for every watch, in the loop
        /// shape `pump` runs it in. Returns the total frame bytes produced —
        /// `0` is what an idle wake must produce, and the bench asserts it.
        pub(crate) fn wake(&mut self, woke: bool) -> usize {
            let mut bytes = 0usize;
            for w in &mut self.watches {
                for f in frames_for_watch(w, self.streams, woke) {
                    bytes += f.body.len();
                }
            }
            bytes
        }

        /// What the fixture ACTUALLY reached on target 0: `(blocks, turns,
        /// timeline events)`. The lower half of the bench's reach guard — a
        /// fixture that failed to fill the ledgers would price an empty scan.
        pub(crate) fn retained(&self) -> (usize, usize, usize) {
            // Bound to NAMED locals rather than locked through `self.terms[0]`
            // directly: the census takes a lock's identity from the receiver
            // NAME, and an index expression has none — locking through one
            // leaves an UNKNOWN-identity site, which is the honesty gap this
            // crate's `no_unknown_identities_on_this_tree` exists to hold at
            // zero. The `fixture_` prefix keeps them out of the production
            // `term` / `turns` / `timelines` identities as well.
            let fixture_term = &self.terms[0];
            let fixture_turns = &self.turns[0];
            let fixture_timeline = &self.timelines[0];
            (
                fixture_term
                    .lock()
                    .expect("bench engine")
                    .all_blocks()
                    .count(),
                fixture_turns.lock().expect("bench ledger").len(),
                fixture_timeline.lock().expect("bench timeline").len(),
            )
        }

        /// Land ONE genuinely new turn record on every target — the "something
        /// happened" arm, and the other half of the reach guard: after this, a
        /// wake MUST produce frames, which proves the digest reaches the ledger
        /// rather than short-circuiting somewhere above it.
        pub(crate) fn land_turn(&mut self) {
            let mut id = self.next_turn_id;
            // Named, not `l`: a single-letter receiver is UNKNOWN to the
            // census for the same reason an index expression is.
            for fixture_ledger in &self.turns {
                id += 1;
                fixture_ledger
                    .lock()
                    .expect("bench ledger")
                    .push(turn_record(id));
            }
            self.next_turn_id = id;
        }
    }

    /// A watch seeded EXACTLY as `push_loop` seeds one for an events-only
    /// subscription: every watermark at the live high, so only what happens
    /// AFTER this point is pushed.
    fn seeded_watch(
        local_id: u64,
        term: &Arc<Mutex<Terminal>>,
        turns: &Arc<Mutex<TurnLedger>>,
        timeline: &Arc<Mutex<SessionTimeline>>,
    ) -> Watch {
        let (last_block_id, last_title, last_bell) = {
            let t = term.lock().expect("bench engine");
            (
                t.all_blocks()
                    .filter(|b| b.is_complete())
                    .map(|b| b.id)
                    .max(),
                Some(t.title().to_string()),
                t.bell_total(),
            )
        };
        Watch {
            local_id,
            term: term.clone(),
            last_sent_seq: 0,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_render_sig: 0,
            last_block_id,
            turns: turns.clone(),
            last_turn_id: turns.lock().expect("bench ledger").high_id(),
            timeline: timeline.clone(),
            last_timeline_id: timeline.lock().expect("bench timeline").high_id(),
            last_title,
            last_bell,
            byte_sub: None,
            non_coalesced: false,
        }
    }

    /// The INSTANCE ROSTER tick: rebuild the live-sid set, then run the SHIPPING
    /// set-diff against the last one. This is what a `sessions` subscriber pays
    /// on every 250 ms wake, changed or not.
    ///
    /// WHAT IS REAL AND WHAT IS MODELLED, stated plainly so the number is not
    /// over-claimed. The DIFF is the shipping function, called directly. The
    /// REBUILD is modelled: `SessionStore::live_sids` walks a `HashMap` of
    /// registered handles and allocates one `String` per session, and this
    /// walks a `HashSet` of the same sid strings and allocates one `String` per
    /// session. Allocation count, string width and hashing are therefore
    /// identical; the gap is the container being walked (and the store's
    /// uncontended `RwLock` read, which the roster journal does not remove
    /// anyway). Building the real thing would need a registered `SessionHandle`,
    /// which needs a whole `SessionCtx` — a fixture that exists only behind
    /// `#[cfg(test)]` and would have to be duplicated here to be reachable, at
    /// which point it could drift from the real handle shape. Naming the gap is
    /// the more honest trade.
    pub(crate) struct RosterRebuild {
        /// The set as the subscriber last saw it (its watermark, pre-journal).
        known: std::collections::HashSet<String>,
        /// The instance's live sids right now.
        live: std::collections::HashSet<String>,
        /// Source of fresh sids for [`Self::churn`].
        next: u64,
    }

    impl RosterRebuild {
        /// An instance with `sessions` live sessions, and a subscriber already
        /// caught up to them (so the first tick is an UNCHANGED one).
        pub(crate) fn new(sessions: usize) -> Self {
            let live: std::collections::HashSet<String> =
                (0..sessions as u64).map(sid_string).collect();
            RosterRebuild {
                known: live.clone(),
                live,
                next: sessions as u64,
            }
        }

        /// ONE wake of the pre-journal roster body: rebuild the set, diff it,
        /// adopt it. Returns the emitted bytes — `0` on an unchanged tick, which
        /// is the state the bench measures and asserts.
        pub(crate) fn tick(&mut self) -> usize {
            // Models `SessionStore::live_sids` (see the type doc): one owned
            // `String` per live session, collected into a fresh set.
            let live: std::collections::HashSet<String> =
                self.live.iter().map(|s| s.to_string()).collect();
            let mut out = String::new();
            super::diff_session_events(&live, &self.known, &mut out);
            self.known = live;
            out.len()
        }

        /// Retire one session and open another — so the NEXT tick has a real
        /// created + exited pair to report. The upper half of the reach guard:
        /// a diff that emitted nothing after this would not be running.
        pub(crate) fn churn(&mut self) {
            if let Some(victim) = self.live.iter().next().cloned() {
                self.live.remove(&victim);
            }
            self.live.insert(sid_string(self.next));
            self.next += 1;
        }

        /// How many sessions this instance has live — the lower reach guard.
        pub(crate) fn sessions(&self) -> usize {
            self.live.len()
        }
    }

    /// A stable sid string of the shipped width (`s-` + 16 lowercase hex), so the
    /// modelled rebuild allocates and hashes the same bytes the real one does.
    fn sid_string(i: u64) -> String {
        format!("s-{:016x}", i.wrapping_mul(0x9E37_79B9_7F4A_7C15))
    }

    /// A small, realistic turn record (the ledger clamps text itself).
    fn turn_record(id: u64) -> TurnRecord {
        TurnRecord {
            id,
            started_ms: id,
            dur_ms: 12,
            submitted: true,
            status: "settled",
            text: "run the suite".to_string(),
            screen_hash: id.wrapping_mul(0x9E37_79B9_7F4A_7C15),
            seq: id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn test_watch(term: Arc<Mutex<Terminal>>) -> Watch {
        Watch {
            local_id: 1,
            term,
            last_sent_seq: 0,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_render_sig: 0,
            last_block_id: None,
            turns: Arc::new(Mutex::new(TurnLedger::default())),
            timeline: Arc::new(Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            last_timeline_id: None,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: None,
            non_coalesced: false,
        }
    }

    /// `@*` ADOPTION IS OWNER-ONLY, at the type level. The capability has no
    /// public constructor that can produce a permissive value from a non-Owner
    /// scope, so a future caller that forgets the refusal in `run_subscribe`
    /// still cannot grant it — the same invariant `InstanceStreams::authorize`
    /// carries for the instance `sessions` stream, and for the same reason: a
    /// live target set names sessions the subscriber could not have named.
    #[test]
    fn adopt_scope_is_owner_only() {
        assert!(
            AdoptScope::authorize(true, Scope::Owner).on(),
            "Owner asking for @* gets it"
        );
        let edge = Scope::Edge(aterm_session::EdgeToken::from_bytes([9u8; 32]));
        assert!(
            !AdoptScope::authorize(true, edge).on(),
            "an EDGE asking for @* is refused, whatever its per-target reach"
        );
        assert!(
            !AdoptScope::authorize(false, Scope::Owner).on(),
            "not asked for = not granted"
        );
        assert!(!AdoptScope::none().on(), "the refusing default refuses");
    }

    /// A LIVE target set's notify registration must track its watch set exactly:
    /// an adopted session starts waking the subscription, a closed one stops, and
    /// neither leaves a handle behind.
    ///
    /// The leak is the point of the second half. A `@*` subscription outlives an
    /// unbounded number of sessions; without `unwatch`, every dead one leaves a
    /// registry entry the producer's `notify` walks on every output burst, and
    /// the `any()` fast-path flag can never fall back to false.
    #[test]
    fn watch_and_unwatch_track_the_registry_exactly() {
        let registry = new_registry();
        let mut sub = SubscriberSet::register(&registry, &[1]);
        let watchers = |id: u64| registry.lock().unwrap().watchers(id);
        assert_eq!((watchers(1), watchers(2)), (1, 0));

        sub.watch(2);
        assert_eq!(watchers(2), 1, "an adopted session is registered");
        sub.watch(2);
        assert_eq!(
            watchers(2),
            1,
            "adoption is idempotent — no duplicate handle"
        );

        // The adopted session really does wake us.
        registry.lock().unwrap().notify(2);
        assert!(
            sub.wait(Duration::from_millis(500)),
            "a notify on the adopted session wakes the subscription"
        );

        sub.unwatch(2);
        assert_eq!(watchers(2), 0, "a closed session is deregistered");
        assert!(registry.any(), "session 1 is still watched");
        registry.lock().unwrap().notify(2);
        assert!(
            !sub.wait(Duration::from_millis(50)),
            "a notify on a dropped session no longer wakes the subscription"
        );

        sub.unwatch(1);
        assert!(
            !registry.any(),
            "the lock-free fast-path flag follows the LAST removal, so a producer \
             stops paying for a subscription that watches nothing"
        );
    }

    /// THE `@*` END-TO-END CLAIM: a subscription ADOPTS a session created after
    /// it subscribed — the thing a frozen target list could not express at all,
    /// and the reason the old federation bridge had to open a whole new
    /// connection (and eventually ran out of the four the server admits).
    ///
    /// It also pins the three properties the adoption has to have to be usable:
    /// the adopted channel is acked with the SAME `sub <local> <sid>` line the
    /// handshake emits (so a client demultiplexes it identically), it is
    /// registered for wakes, and it is seeded LIVE — an adopted watch that
    /// replayed a backlog would flood a fleet supervisor on every tab-open.
    #[test]
    fn adoption_picks_up_a_session_created_after_subscribe() {
        let store = crate::session_store::new_store();
        let registry = new_registry();
        let mut sub = SubscriberSet::register(&registry, &[]);
        let mut watches: Vec<Watch> = Vec::new();
        let mut adopt_seq = 0u64;
        let streams = TargetStreams {
            events: true,
            ..Default::default()
        };
        let opts = PushOptions::default();
        let adopt = |w: &mut Vec<Watch>, sub: &mut Subscription, seq: &mut u64| {
            text(&adopt_new_targets(&store, w, sub, streams, &opts, seq))
        };

        assert!(
            adopt(&mut watches, &mut sub, &mut adopt_seq).is_empty(),
            "an instance with no sessions adopts nothing"
        );

        let h = crate::session_store::test_handle(5);
        let sid = h.sid.as_str().to_string();
        store.write().unwrap_or_else(|p| p.into_inner()).register(h);

        assert_eq!(
            adopt(&mut watches, &mut sub, &mut adopt_seq),
            format!("sub 5 {sid}\n"),
            "the adopted channel is acked exactly like a handshake channel"
        );
        assert_eq!(watches.len(), 1, "and it joined the watch set");
        assert_eq!(
            registry.lock().unwrap().watchers(5),
            1,
            "and it is registered for producer wakes"
        );

        assert!(
            adopt(&mut watches, &mut sub, &mut adopt_seq).is_empty(),
            "a second pass adopts nothing — the watermark caught up"
        );
        assert!(
            frames_for_watch(&mut watches[0], streams, true).is_empty(),
            "an adopted watch is seeded at the LIVE highs, so it replays no backlog"
        );
    }

    /// A subscriber registered for a session is woken by a notify on that session,
    /// and NOT by a notify on an unrelated session.
    #[test]
    fn notify_wakes_only_subscribed_sessions() {
        let reg = new_registry();
        let sub = SubscriberSet::register(&reg, &[7]);

        // A notify on an unrelated session does not wake us.
        reg.lock().unwrap().notify(99);
        assert!(
            !sub.wait(Duration::from_millis(20)),
            "unrelated notify must not wake"
        );

        // A notify on our session wakes us.
        reg.lock().unwrap().notify(7);
        assert!(sub.wait(Duration::from_millis(200)), "our notify must wake");
    }

    /// COALESCING: a flood of notifies between two `wait`s collapses to a single
    /// pending wake (single-slot). The producer's `notify` never blocks regardless
    /// of how far behind the subscriber is.
    #[test]
    fn notify_coalesces_and_never_blocks_producer() {
        let reg = new_registry();
        let sub = SubscriberSet::register(&reg, &[1]);

        // 1000 notifies with NO intervening read: every one is O(1) and non-blocking
        // even though the slot fills after the first. (If notify ever blocked, this
        // would deadlock on the same thread.)
        for _ in 0..1000 {
            reg.lock().unwrap().notify(1);
        }
        // Exactly one wake is pending (coalesced); the second wait times out.
        assert!(sub.wait(Duration::from_millis(200)), "first wake delivered");
        assert!(
            !sub.wait(Duration::from_millis(20)),
            "flood coalesced to one wake"
        );
    }

    /// A notify to a session with a DROPPED subscriber is a no-op and the registry
    /// self-cleans: the dropped subscription deregisters, so the producer pays
    /// nothing for a dead subscriber.
    #[test]
    fn dropped_subscriber_deregisters_and_notify_is_noop() {
        let reg = new_registry();
        {
            let _sub = SubscriberSet::register(&reg, &[5]);
            assert_eq!(reg.lock().unwrap().watched_sessions(), 1);
        } // _sub dropped here
        assert_eq!(
            reg.lock().unwrap().watched_sessions(),
            0,
            "deregistered on drop"
        );
        // Still a safe no-op.
        reg.lock().unwrap().notify(5);
    }

    /// A STALLED subscriber (never calls `wait`) cannot block or backpressure the
    /// producer: 100k notifies complete instantly while the slot stays full.
    #[test]
    fn stalled_subscriber_never_blocks_producer() {
        let reg = new_registry();
        let _sub = SubscriberSet::register(&reg, &[3]); // never wait()ed: wedged
        let start = std::time::Instant::now();
        for _ in 0..100_000 {
            reg.lock().unwrap().notify(3);
        }
        // If notify blocked on a full slot this would never finish; assert it is fast.
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "producer not blocked by stall"
        );
    }

    /// Multiplex: one subscription watching two sessions is woken by EITHER.
    #[test]
    fn one_subscription_watches_multiple_sessions() {
        let reg = new_registry();
        let sub = SubscriberSet::register(&reg, &[10, 20]);
        reg.lock().unwrap().notify(20);
        assert!(
            sub.wait(Duration::from_millis(200)),
            "woken by either watched session"
        );
    }

    /// A [`Watch`] on `term` seeded the way a subscription with no resume anchor
    /// seeds one: every send cursor at its "nothing sent yet" sentinel. Tests
    /// override only the field they are actually varying (`..watch_on(..)`), so what
    /// a case is testing stays visible instead of being buried in a 15-field literal
    /// repeated eighteen times.
    fn watch_on(local_id: u64, term: &Arc<Mutex<Terminal>>) -> Watch {
        Watch {
            local_id,
            term: term.clone(),
            last_sent_seq: 0,
            last_alt: false,
            last_cursor: (u16::MAX, u16::MAX, false, ""),
            last_render_sig: 0,
            last_block_id: None,
            turns: Arc::new(Mutex::new(TurnLedger::default())),
            timeline: Arc::new(Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            last_timeline_id: None,
            last_turn_id: None,
            last_title: None,
            last_bell: 0,
            byte_sub: None,
            non_coalesced: false,
        }
    }

    /// The wire bytes a producer's frames carry, concatenated in emission order —
    /// i.e. what the socket sees MINUS whatever [`Egress`] prepends. Asserting on
    /// this keeps the frame-shape tests independent of the stamp policy, which is
    /// exactly the separation that moving the stamp to the egress bought.
    fn frame_bytes(frames: &[Frame]) -> Vec<u8> {
        frames.iter().flat_map(|f| f.body.iter().copied()).collect()
    }

    /// [`frame_bytes`] decoded — the text streams are all UTF-8 by construction.
    fn text(frames: &[Frame]) -> String {
        String::from_utf8(frame_bytes(frames)).expect("text frames are UTF-8")
    }

    /// One wake's text for a watch: the shape every coalescing case asserts on.
    fn wake_text(w: &mut Watch, streams: TargetStreams, woke: bool) -> String {
        text(&frames_for_watch(w, streams, woke))
    }

    /// Everything an [`Egress`] wrote for `frames`, stamps included — the actual wire
    /// bytes. `Vec<u8>` is a `Write`, so no socket is involved.
    fn wire(timestamps: bool, frames: Vec<Frame>) -> Vec<u8> {
        let mut sink: Vec<u8> = Vec::new();
        let mut egress = Egress::new(&mut sink, timestamps);
        egress.begin_wake();
        for f in frames {
            egress.emit(f).expect("a Vec sink never hangs up");
        }
        egress.end_wake().expect("a Vec sink never hangs up");
        sink
    }

    /// A per-target [`TargetStreams`] with the given fields set, for the terse
    /// `requested(..)` expectations below.
    fn targets_of(screen: bool, cursor: bool, events: bool) -> TargetStreams {
        TargetStreams {
            screen,
            cursor,
            events,
            ..Default::default()
        }
    }

    /// Stream parsing: a subset of the known streams parses; an empty list or a
    /// typo fails closed (so a bad request never silently subscribes to nothing).
    #[test]
    fn streams_parse_subset_and_fail_closed() {
        assert_eq!(
            Requested::parse("screen"),
            Some(Requested {
                targets: targets_of(true, false, false),
                ..Default::default()
            })
        );
        assert_eq!(
            Requested::parse("screen,cursor,events"),
            Some(Requested {
                targets: targets_of(true, true, true),
                ..Default::default()
            }),
        );
        assert_eq!(
            Requested::parse("cursor screen"),
            Some(Requested {
                targets: targets_of(true, true, false),
                ..Default::default()
            }),
        );
        assert_eq!(Requested::parse(""), None, "empty fails closed");
        assert_eq!(
            Requested::parse("bogus"),
            None,
            "unknown stream fails closed"
        );
        assert_eq!(
            Requested::parse("screen,bogus"),
            None,
            "one bad token fails the whole list"
        );
    }

    /// `trim` is a MODIFIER inside the stream list: beside a frame source it parses
    /// and lands on the per-target set (it shapes the `screen` frame); alone, or
    /// with only other modifiers, it names no source and fails closed like `ts`.
    /// A `trim` AFTER the list is not this parser's business — the handler's
    /// unknown-arg rule refuses it, exactly as it refuses a trailing `ts`.
    #[test]
    fn trim_modifier_parses_inside_the_stream_list_and_never_alone() {
        assert_eq!(
            Requested::parse("screen,trim"),
            Some(Requested {
                targets: TargetStreams {
                    screen: true,
                    trim: true,
                    ..Default::default()
                },
                ..Default::default()
            })
        );
        assert_eq!(
            Requested::parse("trim screen ts"),
            Some(Requested {
                targets: TargetStreams {
                    screen: true,
                    trim: true,
                    ..Default::default()
                },
                timestamps: true,
                ..Default::default()
            })
        );
        assert_eq!(Requested::parse("trim"), None, "bare modifier");
        assert_eq!(Requested::parse("trim,ts"), None, "only modifiers");
    }

    /// A list of MODIFIERS ONLY parses every token yet names no frame source. It
    /// must fail closed: otherwise the connection acks `OK subscribe 1`, flips to
    /// push-only, and then stays silent forever — indistinguishable from a hang.
    #[test]
    fn modifier_only_stream_list_fails_closed() {
        assert_eq!(Requested::parse("timestamps"), None, "bare modifier");
        assert_eq!(Requested::parse("ts"), None, "bare modifier alias");
        assert_eq!(Requested::parse("ts,timestamps"), None, "only modifiers");
        // Non-vacuity: the same modifier ALONGSIDE a frame source still parses.
        assert!(Requested::parse("ts,screen").is_some());
        // `sessions` alone is a frame source (an instance-scoped one), so it parses
        // here — it is REFUSED later, by authority, not by the grammar.
        assert_eq!(
            Requested::parse("sessions"),
            Some(Requested {
                instance: RequestedInstance { sessions: true },
                ..Default::default()
            })
        );
    }

    /// AUTHORITY SCOPE: `sessions` is instance-wide, so only an Owner subscription
    /// can hold it. The per-target `ReadScreen` gate resolves the selectors the
    /// client named and cannot speak for the rest of the instance roster, so an Edge
    /// scope gets the EMPTY instance set even when it asks — and there is no other
    /// way to build a non-empty one (the field is private and `authorize` is the
    /// only constructor, which is a compile-level fact this test cannot express).
    #[test]
    fn only_owner_is_granted_the_instance_sessions_stream() {
        let asked = RequestedInstance { sessions: true };
        assert!(
            InstanceStreams::authorize(asked, Scope::Owner).sessions(),
            "Owner already holds the roster via the `sessions`/`who` verbs"
        );
        let edge = Scope::Edge(aterm_session::EdgeToken::from_bytes([7u8; 32]));
        assert!(
            !InstanceStreams::authorize(asked, edge).sessions(),
            "a per-target edge cannot authorize the instance roster"
        );
        // And an Owner that did not ask still does not get it.
        assert!(
            !InstanceStreams::authorize(RequestedInstance::default(), Scope::Owner).sessions(),
            "authorize grants, it does not add"
        );
    }

    /// CORE coalescing claim at the FRAME level: a screen DELTA is emitted only when
    /// the engine's `content_seq` ADVANCES; a wake with unchanged content (a pure
    /// viewport scroll never bumps `content_seq`) emits NOTHING. Each emitted frame
    /// is `<sid>`-tagged so a multiplexed client can demultiplex it.
    #[test]
    fn screen_delta_on_content_change_none_on_viewport_scroll() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let streams = TargetStreams {
            screen: true,
            ..Default::default()
        };
        let mut w = watch_on(4, &term);

        // First wake on a fresh engine: content_seq is already > 0 (the engine
        // initialized its grid), so an immediate catch-up DELTA is produced, tagged
        // with our sid (4).
        crate::term_lock(&term).process(b"hello");
        let f1 = wake_text(&mut w, streams, true);
        assert!(
            f1.starts_with("DELTA 4 seq="),
            "sid-tagged screen delta: {f1:?}"
        );
        assert!(f1.contains("hello"), "delta carries the live screen text");
        let seq_after_write = w.last_sent_seq;
        assert!(seq_after_write > 0);

        // A wake with NO content change (we only move the viewport — a pure scroll
        // does not bump content_seq) emits NOTHING.
        crate::term_lock(&term).scroll_display(1);
        let f2 = wake_text(&mut w, streams, true);
        assert!(f2.is_empty(), "viewport scroll produces no delta: {f2:?}");
        assert_eq!(
            w.last_sent_seq, seq_after_write,
            "seq unchanged by a scroll"
        );

        // A real content change DOES advance the seq and re-emits a delta.
        crate::term_lock(&term).process(b" world");
        let f3 = wake_text(&mut w, streams, true);
        assert!(
            f3.starts_with("DELTA 4 seq="),
            "content change re-emits: {f3:?}"
        );
        assert!(
            w.last_sent_seq > seq_after_write,
            "seq advanced on real content"
        );
    }

    /// `trim` shortens a screen DELTA to the rows up to the last non-blank one, and
    /// `screen <nrows>` on the header is the count actually sent — the pushed face
    /// of `text trim`, by the same rule (interior blanks kept). Without it the frame
    /// is the whole grid, byte-identical to the pre-`trim` wire.
    #[test]
    fn trim_modifier_shortens_screen_delta_to_last_nonblank_row() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term).process(b"one\r\n\r\nthree");
        let full = wake_text(
            &mut watch_on(1, &term),
            TargetStreams {
                screen: true,
                ..Default::default()
            },
            true,
        );
        assert!(
            full.starts_with("DELTA 1 seq=")
                && full.lines().next().unwrap_or("").ends_with(" screen 24"),
            "untrimmed frame carries the whole grid: {full:?}"
        );
        let trimmed = wake_text(
            &mut watch_on(2, &term),
            TargetStreams {
                screen: true,
                trim: true,
                ..Default::default()
            },
            true,
        );
        let header = trimmed.lines().next().unwrap_or("");
        assert!(
            header.starts_with("DELTA 2 seq=") && header.ends_with(" screen 3"),
            "trimmed header counts the rows sent: {header:?}"
        );
        let rows: Vec<&str> = trimmed.lines().skip(1).collect();
        assert_eq!(
            rows,
            ["one", "", "three"],
            "interior blank kept, tail dropped"
        );
    }

    /// MULTIPLEX: two distinct watches produce frames tagged with their OWN sid, so
    /// a single connection watching both can demultiplex by the leading `<sid>`.
    #[test]
    fn multiplex_two_sids_tag_their_own_deltas() {
        let term_a = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let term_b = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term_a).process(b"alpha");
        crate::term_lock(&term_b).process(b"bravo");
        let streams = TargetStreams {
            screen: true,
            ..Default::default()
        };
        let mut wa = watch_on(1, &term_a);
        let mut wb = watch_on(2, &term_b);

        let fa = wake_text(&mut wa, streams, true);
        let fb = wake_text(&mut wb, streams, true);
        assert!(fa.starts_with("DELTA 1 "), "watch A tags sid 1: {fa:?}");
        assert!(fa.contains("alpha"));
        assert!(fb.starts_with("DELTA 2 "), "watch B tags sid 2: {fb:?}");
        assert!(fb.contains("bravo"));
    }

    /// A cursor DELTA carries the deterministic cursor state in the SAME wire shape
    /// the `cursor` verb reports, tagged with the sid.
    #[test]
    fn cursor_delta_reports_position_and_style() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term).process(b"abc");
        let streams = TargetStreams {
            cursor: true,
            ..Default::default()
        };
        let mut w = watch_on(9, &term);
        let f = wake_text(&mut w, streams, true);
        // "abc" advances the cursor to col 3 on row 0.
        assert!(f.contains("DELTA 9 seq="), "sid-tagged cursor delta: {f:?}");
        assert!(f.contains("cursor 0 3 "), "cursor row/col reported: {f:?}");
    }

    /// `since=<seq>` SEMANTICS at the frame level: seeding `last_sent_seq` to the
    /// CURRENT content seq means a fresh wake emits NOTHING (the client is already
    /// caught up); seeding it BELOW the current content seq emits an immediate
    /// catch-up DELTA.
    #[test]
    fn since_seeds_catch_up_only_when_content_advanced() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term).process(b"state");
        let cur = crate::term_lock(&term).content_seq();
        let streams = TargetStreams {
            screen: true,
            ..Default::default()
        };

        // since == current seq: caught up, no catch-up frame.
        let mut caught_up = Watch {
            last_sent_seq: cur,
            // Seed to the LIVE render signature (as production does), so an unchanged
            // render does not spuriously trip the recolor/DECSCNM/DECTCEM re-emit path.
            last_render_sig: render_sig(&crate::term_lock(&term)),
            ..watch_on(1, &term)
        };
        assert!(
            wake_text(&mut caught_up, streams, true).is_empty(),
            "no frame when caught up"
        );

        // since below current seq: an immediate catch-up DELTA fires.
        let mut behind = Watch {
            last_sent_seq: cur - 1,
            ..watch_on(1, &term)
        };
        assert!(
            wake_text(&mut behind, streams, true).starts_with("DELTA 1 "),
            "catch-up delta when content advanced past since",
        );
    }

    /// CURSOR STREAM completeness: a pure caret move (CSI H) bumps no `content_seq`,
    /// so a seq-gated stream would never push it. The `cursor` stream must emit a
    /// DELTA on the change anyway (its whole purpose), matching the poll `cursor` verb.
    #[test]
    fn cursor_only_move_emits_delta_on_unchanged_seq() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term).process(b"abc"); // content advances; cursor at (0,3)
        let streams = TargetStreams {
            cursor: true,
            ..Default::default()
        };
        let mut w = watch_on(5, &term);
        // First wake syncs the cursor watermark off the content advance.
        assert!(wake_text(&mut w, streams, true).contains("cursor 0 3 "));
        let seq_before = crate::term_lock(&term).content_seq();
        // CSI H moves the caret WITHOUT a cell write — content_seq stays put.
        crate::term_lock(&term).process(b"\x1b[10;5H");
        assert_eq!(
            crate::term_lock(&term).content_seq(),
            seq_before,
            "a pure cursor move must not bump content_seq (else this tests nothing)"
        );
        // The seq is unchanged, but the caret moved to (9,4): a cursor DELTA still fires.
        let f = wake_text(&mut w, streams, true);
        assert!(
            f.contains("cursor 9 4 "),
            "cursor-only move emits a delta on an unchanged seq: {f:?}"
        );
    }

    /// CURSOR VISIBILITY (DECTCEM ?25l/?25h) bumps no `content_seq`, but the `cursor`
    /// stream carries the `<visible>` bit (matching the poll `cursor` verb), so a pure
    /// hide/show still emits a DELTA reflecting the flip.
    #[test]
    fn cursor_visibility_toggle_emits_delta_with_visible_bit() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term).process(b"hi"); // caret at (0,2), cursor visible
        let streams = TargetStreams {
            cursor: true,
            ..Default::default()
        };
        let mut w = watch_on(1, &term);
        // First wake syncs; the caret is visible (bit 1).
        assert!(
            wake_text(&mut w, streams, true).contains("cursor 0 2 1 "),
            "initial cursor delta carries visible=1"
        );
        let seq_before = crate::term_lock(&term).content_seq();
        crate::term_lock(&term).process(b"\x1b[?25l"); // DECTCEM hide — no cell write
        assert_eq!(
            crate::term_lock(&term).content_seq(),
            seq_before,
            "a DECTCEM toggle must not bump content_seq (else this tests nothing)"
        );
        let f = wake_text(&mut w, streams, true);
        assert!(
            f.contains("cursor 0 2 0 "),
            "a pure visibility hide emits a cursor delta with visible=0: {f:?}"
        );
    }

    /// A RECOLOR (OSC 4 palette / OSC 10-12 dynamic colour) or DECSCNM reverse-video
    /// mutates the styled output WITHOUT bumping `content_seq`, so a seq-gated `cells`
    /// subscriber would show stale colours. The per-wake render signature notices it
    /// and re-emits a `cells` DELTA at the unchanged seq (no GAP).
    #[test]
    fn recolor_reemits_cells_on_unchanged_seq() {
        let term = Arc::new(Mutex::new(Terminal::new(4, 8)));
        crate::term_lock(&term).process(b"x");
        let streams = TargetStreams {
            cells: true,
            ..Default::default()
        };
        let mut w = watch_on(1, &term);
        // First wake emits cells and syncs the render signature.
        assert!(wake_text(&mut w, streams, true).contains(" cells "));
        let seq_before = crate::term_lock(&term).content_seq();
        // OSC 11: set the default BACKGROUND (the dark/light-toggle path) — a render
        // change with no cell write. (OSC 4 palette set is gated by
        // `allow_palette_reconfigure`; OSC 10/11/12 defaults are not.)
        crate::term_lock(&term).process(b"\x1b]11;rgb:00/00/80\x07");
        assert_eq!(
            crate::term_lock(&term).content_seq(),
            seq_before,
            "a recolor must not bump content_seq (else this tests nothing)"
        );
        let f = wake_text(&mut w, streams, true);
        assert!(
            f.contains("cells ") && f.contains(&format!("seq={seq_before} ")),
            "recolor re-emits a cells DELTA at the unchanged seq: {f:?}"
        );
        assert!(
            !f.contains("GAP"),
            "a render-only change needs no GAP resync: {f:?}"
        );

        // DECSCNM reverse-video (a distinct render path via the reverse_video accessor)
        // likewise re-emits at an unchanged seq.
        let seq2 = crate::term_lock(&term).content_seq();
        crate::term_lock(&term).process(b"\x1b[?5h"); // DECSCNM on
        assert_eq!(
            crate::term_lock(&term).content_seq(),
            seq2,
            "DECSCNM bumps no content_seq"
        );
        assert!(
            wake_text(&mut w, streams, true).contains("cells "),
            "a DECSCNM flip re-emits a cells DELTA at the unchanged seq"
        );
    }

    /// Text selection mutates render state without advancing `content_seq`.
    /// A coalesced cells-only subscriber must still receive the typed,
    /// side-adjusted selection (including a sparse blank tail), and clearing it
    /// must remove the old rectangle at the same sequence.
    #[test]
    fn selection_only_change_reemits_cells_on_unchanged_seq() {
        use aterm_core::selection::{SelectionSide, SelectionType};

        let term = Arc::new(Mutex::new(Terminal::new(2, 8)));
        crate::term_lock(&term).process(b"x");
        let streams = TargetStreams {
            cells: true,
            ..Default::default()
        };
        let mut watch = test_watch(term.clone());
        assert!(
            wake_text(&mut watch, streams, true).contains(" cells "),
            "first wake establishes the cells/render watermarks"
        );
        let seq = crate::term_lock(&term).content_seq();

        {
            let mut t = crate::term_lock(&term);
            let sel = t.text_selection_mut();
            sel.start_selection(0, 6, SelectionSide::Left, SelectionType::Block);
            sel.update_selection(1, 7, SelectionSide::Right);
            sel.complete_selection();
        }
        assert_eq!(
            crate::term_lock(&term).content_seq(),
            seq,
            "selection-only mutation must remain a render-only negative control"
        );
        let selected = wake_text(&mut watch, streams, true);
        assert!(
            selected.contains(" cells ")
                && selected.contains(
                    "\"selection\":{\"start_row\":0,\"start_col\":6,\"end_row\":1,\
                     \"end_col\":7,\"kind\":\"block\",\"is_block\":true}"
                ),
            "selection-only sparse-tail change must push typed CELLS: {selected:?}"
        );
        assert!(
            frames_for_watch(&mut watch, streams, true).is_empty(),
            "unchanged selection must not defeat coalescing"
        );

        crate::term_lock(&term).text_selection_mut().clear();
        assert_eq!(crate::term_lock(&term).content_seq(), seq);
        let cleared = wake_text(&mut watch, streams, true);
        assert!(
            cleared.contains(" cells ") && cleared.contains("\"selection\":null"),
            "selection clear must remove the stale rectangle at the same seq: {cleared:?}"
        );
    }

    /// OSC 17/19 recolor a stationary selection without a grid write. Both the
    /// render signature and the pushed payload must carry those changes.
    #[test]
    fn selection_recolor_reemits_cells_on_unchanged_seq() {
        use aterm_core::selection::{SelectionSide, SelectionType};

        let term = Arc::new(Mutex::new(Terminal::new(2, 8)));
        {
            let mut t = crate::term_lock(&term);
            t.process(b"x");
            let sel = t.text_selection_mut();
            sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
            sel.update_selection(0, 7, SelectionSide::Right);
            sel.complete_selection();
        }
        let streams = TargetStreams {
            cells: true,
            ..Default::default()
        };
        let mut watch = test_watch(term.clone());
        assert!(wake_text(&mut watch, streams, true).contains(" cells "));
        let seq = crate::term_lock(&term).content_seq();

        crate::term_lock(&term).process(b"\x1b]17;rgb:12/34/56\x07");
        assert_eq!(
            crate::term_lock(&term).content_seq(),
            seq,
            "OSC 17 is render-only"
        );
        let background = wake_text(&mut watch, streams, true);
        assert!(
            background.contains(" cells ") && background.contains("\"selection_bg\":\"123456\""),
            "OSC 17 must push the new selection fill: {background:?}"
        );

        crate::term_lock(&term).process(b"\x1b]19;rgb:fe/dc/ba\x07");
        assert_eq!(
            crate::term_lock(&term).content_seq(),
            seq,
            "OSC 19 is render-only"
        );
        let foreground = wake_text(&mut watch, streams, true);
        assert!(
            foreground.contains(" cells ") && foreground.contains("\"selection_fg\":\"fedcba\""),
            "OSC 19 must push the new selected-text ink: {foreground:?}"
        );
    }

    /// A styled CELLS frame contains its own cursor object. Therefore a cells-only
    /// subscriber (without the separate `cursor` stream) must refresh on pure
    /// position/style/color changes at an unchanged content sequence.
    #[test]
    fn cursor_only_change_reemits_cells_on_unchanged_seq() {
        let term = Arc::new(Mutex::new(Terminal::new(12, 16)));
        crate::term_lock(&term).process(b"x");
        let streams = TargetStreams {
            cells: true,
            ..Default::default()
        };
        let mut watch = test_watch(term.clone());
        assert!(wake_text(&mut watch, streams, true).contains(" cells "));
        let seq = crate::term_lock(&term).content_seq();

        crate::term_lock(&term).process(b"\x1b[10;5H");
        assert_eq!(
            crate::term_lock(&term).content_seq(),
            seq,
            "pure cursor move is render-only"
        );
        let moved = wake_text(&mut watch, streams, true);
        assert!(
            moved.contains(" cells ")
                && moved.contains("\"cursor\":{\"row\":9,\"col\":4,\"visible\":true"),
            "cells-only watcher must receive the moved cursor: {moved:?}"
        );

        crate::term_lock(&term).process(b"\x1b[6 q");
        assert_eq!(crate::term_lock(&term).content_seq(), seq);
        let shaped = wake_text(&mut watch, streams, true);
        assert!(
            shaped.contains(" cells ") && shaped.contains("\"style\":\"steady_bar\""),
            "cells-only watcher must receive DECSCUSR: {shaped:?}"
        );

        crate::term_lock(&term).process(b"\x1b]12;rgb:ab/cd/ef\x07");
        assert_eq!(crate::term_lock(&term).content_seq(), seq);
        let colored = wake_text(&mut watch, streams, true);
        assert!(
            colored.contains(" cells ") && colored.contains("\"color\":\"abcdef\""),
            "cells-only watcher must receive OSC 12 cursor color: {colored:?}"
        );
        assert!(
            frames_for_watch(&mut watch, streams, true).is_empty(),
            "unchanged cursor overlay must remain coalesced"
        );
    }

    /// ALT-SCREEN swap (1049) does not move the MAIN grid's `content_seq`, so a swap
    /// landing on a seq the client already saw would be invisible to a pure seq
    /// compare. A flag flip must force a resync (GAP + full frame) even at equal seq.
    #[test]
    fn alt_screen_flip_forces_resync_even_at_equal_seq() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term).process(b"\x1b[?1049h"); // enter the alternate screen
        let (seq, alt) = {
            let t = crate::term_lock(&term);
            (t.content_seq(), t.is_alternate_screen())
        };
        assert!(alt, "1049h enters the alternate screen");
        let streams = TargetStreams {
            screen: true,
            ..Default::default()
        };
        // The watch believes it is caught up at this seq but on the MAIN buffer
        // (last_alt=false): the alt flip is the ONLY thing that changed.
        let mut w = Watch {
            last_sent_seq: seq,
            ..watch_on(1, &term)
        };
        let f = wake_text(&mut w, streams, true);
        assert!(
            f.contains("GAP 1 ") && f.contains("DELTA 1 "),
            "an alt-screen flip at equal seq resyncs (GAP + full frame): {f:?}"
        );
        assert!(w.last_alt, "the alt watermark is updated after the resync");
    }

    /// ITEM 2: the `cells` and `bytes` stream tokens parse (additively with the
    /// existing screen/cursor/events).
    #[test]
    fn streams_parse_accepts_cells_and_bytes() {
        assert_eq!(
            Requested::parse("cells"),
            Some(Requested {
                targets: TargetStreams {
                    cells: true,
                    ..Default::default()
                },
                ..Default::default()
            })
        );
        assert_eq!(
            Requested::parse("bytes"),
            Some(Requested {
                targets: TargetStreams {
                    bytes: true,
                    ..Default::default()
                },
                ..Default::default()
            })
        );
        assert_eq!(
            Requested::parse("cells,bytes,screen"),
            Some(Requested {
                targets: TargetStreams {
                    cells: true,
                    bytes: true,
                    screen: true,
                    ..Default::default()
                },
                ..Default::default()
            }),
        );
        // `timestamps` / `ts` are MODIFIER tokens (no own frames), so they land
        // OUTSIDE both stream sets — they restamp frames, they never authorize any.
        assert_eq!(
            Requested::parse("screen,timestamps"),
            Some(Requested {
                targets: targets_of(true, false, false),
                timestamps: true,
                ..Default::default()
            })
        );
        assert_eq!(
            Requested::parse("cursor ts"),
            Some(Requested {
                targets: targets_of(false, true, false),
                timestamps: true,
                ..Default::default()
            })
        );
    }

    /// The `timestamps` modifier prefixes a wake's frames with a `T <sid> <t_us>`
    /// line (one per wake, video's `now_us` clock), turning the stream into a timed
    /// frame source. Off by default (byte-identical un-timestamped stream).
    #[test]
    fn timestamps_prefixes_a_wake_with_a_t_line() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term).process(b"hello");
        let streams = TargetStreams {
            screen: true,
            ..Default::default()
        };
        let mut w = watch_on(7, &term);
        let f = String::from_utf8(wire(true, frames_for_watch(&mut w, streams, true))).unwrap();
        assert!(f.starts_with("T 7 "), "wake prefixed with a T line: {f:?}");
        assert!(
            f.contains("\nDELTA 7 "),
            "the frames follow the T line: {f:?}"
        );
        // Without the modifier, no T line.
        let mut w2 = watch_on(7, &term);
        let f2 = String::from_utf8(wire(false, frames_for_watch(&mut w2, streams, true))).unwrap();
        assert!(
            f2.starts_with("DELTA 7 "),
            "no T line without the modifier: {f2:?}"
        );
    }

    /// 4.5: the stamp is the EGRESS's, not one producer's. A `bytes`-only
    /// subscription with `timestamps` gets its `BYTES` bursts stamped — the frame
    /// kind that could never be stamped while the `T` line was written inside
    /// `frames_for_watch`, because `drain_bytes_frames` took no timestamp flag at
    /// all (nor did the instance events, the closing `exited`, or the resume `GAP`).
    #[test]
    fn bytes_only_subscription_gets_its_bursts_stamped() {
        let fan = Arc::new(ByteFanout::new());
        let term = Arc::new(Mutex::new(Terminal::new(2, 4)));
        let bs = fan.subscribe();
        fan.tee(&Arc::from(&b"out"[..]));
        let w = Watch {
            byte_sub: Some(bs),
            ..watch_on(7, &term)
        };
        let stamped = String::from_utf8(wire(true, drain_bytes_frames(&w))).unwrap();
        assert!(
            stamped.starts_with("T 7 "),
            "the bytes burst is stamped: {stamped:?}"
        );
        assert!(
            stamped.contains("\nBYTES 7 3\nout\n"),
            "the burst follows its stamp, byte-exact: {stamped:?}"
        );
    }

    /// NEGATIVE CONTROL for [`bytes_only_subscription_gets_its_bursts_stamped`]:
    /// prove the assertion is not vacuous. The stamp must be absent when the modifier
    /// is off (else `starts_with("T 7 ")` would be testing the `BYTES` header's own
    /// shape), and it must appear exactly ONCE per tag per wake (else "stamped" would
    /// be satisfied by a per-frame stamp, which is a different, chattier contract).
    #[test]
    fn the_bytes_stamp_is_opt_in_and_once_per_wake_non_vacuous() {
        let fan = Arc::new(ByteFanout::new());
        let term = Arc::new(Mutex::new(Terminal::new(2, 4)));
        let bs = fan.subscribe();
        fan.tee(&Arc::from(&b"a"[..]));
        let w = Watch {
            byte_sub: Some(bs),
            ..watch_on(7, &term)
        };
        let unstamped = String::from_utf8(wire(false, drain_bytes_frames(&w))).unwrap();
        assert_eq!(
            unstamped, "BYTES 7 1\na\n",
            "without the modifier the stream is byte-identical to the untimed one"
        );

        // Two frames on the SAME tag in one wake share one stamp; a third on the
        // instance tag gets its own, because `*` is a different channel.
        let tagged = String::from_utf8(wire(
            true,
            vec![
                Frame::text(Tag::Channel(7), "EVENT 7 bell total=1\n".to_string()),
                Frame::raw(Tag::Channel(7), b"BYTES 7 1\nz\n".to_vec()),
                Frame::text(Tag::Instance, "EVENT * session-exited s-a\n".to_string()),
            ],
        ))
        .unwrap();
        // Counted by LINE PREFIX: `EVENT 7 …` contains the substring `T 7 `, so a
        // naive substring count would silently pass on a per-frame stamp.
        let stamps = |tag: &str| {
            tagged
                .lines()
                .filter(|l| l.starts_with(&format!("T {tag} ")))
                .count()
        };
        assert_eq!(stamps("7"), 1, "one stamp per tag per wake: {tagged:?}");
        assert_eq!(
            stamps("*"),
            1,
            "the instance channel is stamped too, and separately: {tagged:?}"
        );
    }

    /// A `cells` DELTA carries the LOSSLESS styled-screen JSON payload (Item 1),
    /// length-prefixed, on content advance.
    #[test]
    fn cells_delta_carries_styled_payload() {
        let term = Arc::new(Mutex::new(Terminal::new(2, 4)));
        crate::term_lock(&term).process(b"\x1b[1mhi");
        let streams = TargetStreams {
            cells: true,
            ..Default::default()
        };
        let mut w = watch_on(5, &term);
        let f = wake_text(&mut w, streams, true);
        assert!(f.starts_with("DELTA 5 seq="), "sid-tagged cells delta: {f}");
        assert!(f.contains(" cells "), "is a cells frame: {f}");
        assert!(
            f.contains("\"rows\":[["),
            "carries the styled frame payload: {f}"
        );
        assert!(f.contains("\"bold\""), "carries resolved decorations: {f}");
        // The length prefix matches the JSON body byte length.
        let header = f.lines().next().unwrap();
        let nbytes: usize = header.rsplit(' ').next().unwrap().parse().unwrap();
        let body = &f[header.len() + 1..f.len() - 1]; // between header\n and trailing \n
        assert_eq!(body.len(), nbytes, "length prefix matches body: {f}");
    }

    /// SESSION-METADATA stage 1 — the events digest pushes `EVENT <sid> meta
    /// <payload>` for each `meta-change` timeline record past the watermark,
    /// SKIPS the lifecycle records the wire does not carry (`spawned`,
    /// `state-change`), and advances the watermark to the timeline HIGH so
    /// nothing double-emits and skipped records are never re-walked. A seeded
    /// watermark (subscription-time high) means only post-subscription changes
    /// push — a live stream, not a replay.
    #[test]
    fn meta_change_drains_as_a_meta_event_and_watermark_advances() {
        let timeline = std::sync::Arc::new(std::sync::Mutex::new(
            crate::session_timeline::SessionTimeline::default(),
        ));
        // Pre-subscription history: a spawn + an OLD meta change.
        timeline
            .lock()
            .unwrap()
            .record("spawned", "state=alive".to_string());
        timeline
            .lock()
            .unwrap()
            .record("meta-change", "field=title value=old".to_string());
        let seeded = initial_timeline_watermark(
            &timeline,
            TargetStreams {
                events: true,
                ..Default::default()
            },
        );
        assert_eq!(seeded, Some(2), "seeded to the live high");

        // Nothing new: no frames, watermark stays.
        let mut out = String::new();
        let wm = drain_timeline_events(&timeline, "7", seeded, &mut out);
        assert!(
            out.is_empty(),
            "no replay of pre-subscription history: {out}"
        );
        assert_eq!(wm, Some(2));

        // A post-subscription meta change + an interleaved non-meta record: only
        // the meta record pushes; the watermark passes BOTH.
        timeline
            .lock()
            .unwrap()
            .record("meta-change", "field=title value=build%20agent".to_string());
        timeline
            .lock()
            .unwrap()
            .record("state-change", "state=exited".to_string());
        let mut out = String::new();
        let wm = drain_timeline_events(&timeline, "7", wm, &mut out);
        assert_eq!(
            out, "EVENT 7 meta field=title value=build%20agent\n",
            "exactly the meta record, verbatim payload"
        );
        assert_eq!(wm, Some(4), "watermark passes the non-meta record too");
        // Drained again: silence (no double emit).
        let mut out = String::new();
        let wm2 = drain_timeline_events(&timeline, "7", wm, &mut out);
        assert!(out.is_empty());
        assert_eq!(wm2, Some(4));
    }

    /// THE EXIT LEDGER ON THE WIRE — the deterministic half. A `subscribe … events`
    /// watch seeded exactly as [`push_loop`] seeds it ([`new_watch`], at the live
    /// timeline high, so the `spawned` row is history) is pruned by the shipping
    /// liveness verdict after a control-socket `close` deregisters its session, and
    /// its final pass ([`Closing::drain`]) is BYTE-EXACTLY the `closing` row the
    /// store wrote — `reason=ctl-close by=<caller>` — followed by `exited`. Nothing
    /// else: the `state-change state=closed` written beside it is not a wire kind
    /// (`exited` is that fact), and equality, not `contains`, is what proves it.
    #[test]
    fn a_ctl_close_reaches_a_live_events_watch_as_closing_then_exited() {
        let store = crate::session_store::new_store();
        let h = crate::session_store::test_handle(7);
        let target: ResolvedTarget = (
            7,
            h.term.clone(),
            h.ctx.byte_fanout.clone(),
            h.ctx.turns.clone(),
            h.ctx.timeline.clone(),
        );
        store.write().unwrap_or_else(|p| p.into_inner()).register(h);
        let streams = TargetStreams {
            events: true,
            ..Default::default()
        };
        let mut watches = vec![new_watch(&target, streams, &PushOptions::default())];
        // Live and idle: a wake pushes nothing (the seed hides `spawned`), and the
        // liveness verdict keeps the watch.
        assert!(
            frames_for_watch(&mut watches[0], streams, true).is_empty(),
            "a fresh watch replays no history"
        );
        assert!(
            prune_closed(&store, &mut watches).is_empty(),
            "alive: nothing to prune"
        );

        // The `close` verb's deregistration, as `retire_session_registration`
        // performs it under the attribution the `Wake::CloseSession` arm opens.
        store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .deregister_local_as(
                7,
                crate::session_store::ExitReason::CtlClose,
                crate::session_store::ExitActor::Sid("s-boss".into()),
            );

        let mut closing = prune_closed(&store, &mut watches);
        assert!(watches.is_empty(), "the dead watch left the live set");
        let out = text(
            &closing
                .pop()
                .expect("handed back, not dropped")
                .drain(streams),
        );
        assert_eq!(
            out, "EVENT 7 closing reason=ctl-close by=s-boss\nEVENT 7 exited\n",
            "why, then that: the ledger row precedes the end-of-stream marker, and \
             nothing else leaks"
        );
    }

    /// The same fact through the shipping [`push_loop`], end to end, with the close
    /// landing AFTER the loop has seeded and caught up. The peer probe is the one
    /// hook the loop calls on its own thread strictly after the catch-up wake, so
    /// deregistering from inside it is race-free where a feeder thread would have
    /// to guess at the seeding instant. A `closing` row written after subscription
    /// is a push; one written earlier would be history and this test would
    /// (rightly) see nothing — so it also pins that the row is written AT
    /// deregistration, not before.
    #[test]
    fn push_loop_delivers_closing_before_exited_for_a_ctl_close() {
        let store = crate::session_store::new_store();
        let h = crate::session_store::test_handle(9);
        let target: ResolvedTarget = (
            9,
            h.term.clone(),
            h.ctx.byte_fanout.clone(),
            h.ctx.turns.clone(),
            h.ctx.timeline.clone(),
        );
        store.write().unwrap_or_else(|p| p.into_inner()).register(h);
        let streams = TargetStreams {
            events: true,
            ..Default::default()
        };
        let registry = new_registry();
        let mut sink: Vec<u8> = Vec::new();
        let mut closed = false;
        let close_once = || {
            if !closed {
                closed = true;
                store
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .deregister_local_as(
                        9,
                        crate::session_store::ExitReason::CtlClose,
                        crate::session_store::ExitActor::Sid("s-boss".into()),
                    );
            }
            false
        };
        push_loop_with_peer_probe(
            &registry,
            &store,
            &[target],
            PushScopes {
                streams,
                instance: InstanceStreams::default(),
                adopt: AdoptScope::none(),
            },
            PushOptions::default(),
            &mut sink,
            close_once,
        );
        assert!(
            closed,
            "the probe ran: the loop reached its first real wake"
        );
        let out = String::from_utf8_lossy(&sink).into_owned();
        assert_eq!(
            out, "EVENT 9 closing reason=ctl-close by=s-boss\nEVENT 9 exited\n",
            "the whole stream after subscription is the ledger row, then the end marker"
        );
    }

    /// The `bytes` stream drains EVERY burst byte-exactly (incl. non-UTF-8) as
    /// length-prefixed `BYTES` frames — live and every-frame, no coalescing.
    #[test]
    fn bytes_drain_is_byte_lossless_and_every_frame() {
        let fan = Arc::new(ByteFanout::new());
        let term = Arc::new(Mutex::new(Terminal::new(2, 4)));
        let bs = fan.subscribe();
        fan.tee(&Arc::from(&b"\x1b[31m"[..]));
        fan.tee(&Arc::from(&[0x80u8, 0x00][..])); // non-UTF-8 + NUL
        let w = Watch {
            byte_sub: Some(bs),
            ..watch_on(7, &term)
        };
        let out = frame_bytes(&drain_bytes_frames(&w));
        let mut expected: Vec<u8> = Vec::new();
        expected.extend_from_slice(b"BYTES 7 5\n\x1b[31m\n");
        expected.extend_from_slice(b"BYTES 7 2\n\x80\x00\n");
        assert_eq!(out, expected, "byte-exact, every-frame, length-prefixed");
    }

    /// A queue overflow surfaces as a counted `GAP … bytes-dropped=` before the
    /// surviving (newest) bursts.
    #[test]
    fn bytes_gap_emitted_when_queue_overflowed() {
        let fan = Arc::new(ByteFanout::with_budget(4));
        let term = Arc::new(Mutex::new(Terminal::new(2, 4)));
        let bs = fan.subscribe();
        for _ in 0..10 {
            fan.tee(&Arc::from(&b"abcd"[..]));
        }
        let w = Watch {
            byte_sub: Some(bs),
            ..watch_on(7, &term)
        };
        let out = String::from_utf8_lossy(&frame_bytes(&drain_bytes_frames(&w))).into_owned();
        assert!(
            out.starts_with("GAP 7 bytes-dropped="),
            "gap precedes bursts: {out}"
        );
        assert!(out.contains("BYTES 7 4\n"), "newest burst survives: {out}");
    }

    /// The `events` digest emits `EVENT <sid> title <pct>` on a title change and
    /// NOT on an unchanged re-scan (watermark), with the title pct-encoded so a
    /// space/newline in it stays on one line.
    #[test]
    fn drain_title_event_emits_once_per_change_pct_encoded() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        // OSC 2 sets the window title to "my dir" (a space -> pct-encoded).
        crate::term_lock(&term).process(b"\x1b]2;my dir\x07");
        let mut out = String::new();
        // Sampled through the SHIPPING sampler, so this test now covers the one
        // lock hold as well as the formatter it feeds.
        let title = |t: &Arc<Mutex<Terminal>>| sample_engine_events(t, None).title;
        // First drain with no watermark emits the current title.
        let wm = drain_title_event("3", &title(&term), None, &mut out);
        assert!(
            out.contains("EVENT 3 title my%20dir\n"),
            "title emitted + pct-encoded: {out:?}"
        );
        // A re-scan with the same title emits nothing.
        out.clear();
        let wm = drain_title_event("3", &title(&term), wm, &mut out);
        assert!(out.is_empty(), "unchanged title emits nothing: {out:?}");
        // A new title emits again.
        crate::term_lock(&term).process(b"\x1b]2;other\x07");
        out.clear();
        let _ = drain_title_event("3", &title(&term), wm, &mut out);
        assert!(
            out.contains("EVENT 3 title other\n"),
            "change re-emits: {out:?}"
        );
    }

    /// DIFFERENTIAL + REACH for the block half of [`sample_engine_events`]: the
    /// O(1) early-out and the reverse walk must agree with the forward
    /// `all_blocks().filter(is_complete && id > wm)` scan they replaced, at
    /// EVERY watermark — and in particular across a block that was ARCHIVED
    /// WITHOUT EVER COMPLETING (an abandoned prompt / Ctrl-C'd command line).
    /// That is the case a reverse walk which stopped on `!is_complete()` rather
    /// than on the ID would silently truncate, and no dense all-completed
    /// fixture can catch it.
    #[test]
    fn sample_engine_events_blocks_match_the_forward_filter() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let run = |bytes: &[u8]| {
            crate::term_lock(&term).process(bytes);
        };
        // Two completed commands, an ABANDONED prompt, then a third completed one.
        run(b"\x1b]133;A\x07$ \x1b]133;B\x07one\n\x1b]133;C\x07o\n\x1b]133;D;0\x07");
        run(b"\x1b]133;A\x07$ \x1b]133;B\x07two\n\x1b]133;C\x07t\n\x1b]133;D;1\x07");
        run(b"\x1b]133;A\x07$ \x1b]133;B\x07abandoned");
        run(b"\x1b]133;A\x07$ \x1b]133;B\x07three\n\x1b]133;C\x07x\n\x1b]133;D;0\x07");

        // The exact expression the sampler replaced, kept here as the oracle.
        let reference = |wm: Option<u64>| -> Vec<(u64, Option<i32>)> {
            let t = crate::term_lock(&term);
            t.all_blocks()
                .filter(|b| b.is_complete() && wm.is_none_or(|h| b.id > h))
                .map(|b| (b.id, b.exit_code))
                .collect()
        };
        let all = reference(None);
        assert_eq!(
            all.len(),
            3,
            "REACH: the fixture must produce three COMPLETED blocks around one \
             abandoned prompt, not {all:?} — otherwise the walk is priced against \
             a shape that cannot exercise the break"
        );
        assert!(
            all.iter().any(|&(_, exit)| exit == Some(1)),
            "REACH: a non-zero exit must be present so the payload is not degenerate"
        );

        // Every watermark from below the floor to past the ceiling, plus `None`.
        let ceiling = all.last().expect("three blocks").0;
        let mut probes: Vec<Option<u64>> = vec![None];
        for id in 0..=(ceiling + 2) {
            probes.push(Some(id));
        }
        for wm in probes {
            assert_eq!(
                sample_engine_events(&term, wm).blocks,
                reference(wm),
                "watermark {wm:?} diverged from the forward filter"
            );
        }

        // The two arms the digest stands on, named explicitly.
        assert!(
            sample_engine_events(&term, Some(ceiling)).blocks.is_empty(),
            "at the high-water an idle wake must produce nothing"
        );
        assert_eq!(
            sample_engine_events(&term, None).blocks,
            all,
            "no watermark = every completed block, oldest-first"
        );
    }

    /// The `sessions` stream emits `EVENT * session-created <sid>` for a sid newly
    /// live and `EVENT * session-exited <sid>` for one gone, and NOTHING for an
    /// unchanged set (watermark) — so a supervisor learns of a SIBLING spawn/exit
    /// without polling `ls`.
    #[test]
    fn diff_session_events_emits_created_and_exited() {
        use std::collections::HashSet;
        let known: HashSet<String> = ["s-aaa".to_string(), "s-bbb".to_string()].into();
        // A new session (s-ccc) appears; s-bbb is gone.
        let live: HashSet<String> = ["s-aaa".to_string(), "s-ccc".to_string()].into();
        let mut out = String::new();
        diff_session_events(&live, &known, &mut out);
        assert!(
            out.contains("EVENT * session-created s-ccc\n"),
            "created: {out:?}"
        );
        assert!(
            out.contains("EVENT * session-exited s-bbb reason=unknown\n"),
            "exited (a set diff cannot know why, and says so): {out:?}"
        );
        assert!(
            !out.contains("s-aaa"),
            "unchanged session emits nothing: {out:?}"
        );
        // No change -> nothing.
        let mut out2 = String::new();
        diff_session_events(&live, &live, &mut out2);
        assert!(out2.is_empty(), "unchanged set emits nothing: {out2:?}");
    }

    /// A cursor seeded at the current journal high, the way `push_loop` seeds one.
    fn roster_cursor(store: &Store) -> RosterCursor {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        RosterCursor {
            seq: g.roster_seq(),
            known: g.live_sids(),
        }
    }

    /// THE SCP-4 CORRECTNESS FIX, with its own negative control.
    ///
    /// A sibling that BOTH spawns and exits between two wakes used to appear in
    /// neither snapshot and emit neither event — the push loop's module doc says
    /// so outright. The journal records each transition as it happens, so both
    /// events now surface, in order, on the next wake.
    ///
    /// The negative control is the OLD mechanism run over the SAME two instants:
    /// it must emit nothing. Without it this test could pass for the wrong
    /// reason (e.g. if the fixture accidentally left the session registered),
    /// and the whole claim is that the two mechanisms DISAGREE here.
    #[test]
    fn a_sub_tick_spawn_and_exit_both_surface() {
        let store = crate::session_store::new_store();
        let mut cursor = roster_cursor(&store);
        let before = cursor.known.clone();

        let h = crate::session_store::test_handle(7);
        let sid = h.sid.as_str().to_string();
        {
            let mut g = store.write().unwrap_or_else(|p| p.into_inner());
            g.register(h);
            g.deregister_local_as(
                7,
                crate::session_store::ExitReason::CtlClose,
                crate::session_store::ExitActor::Sid("s-boss".into()),
            );
        }

        // NEGATIVE CONTROL: the pre-journal snapshot diff, on the same instants.
        let after = store.read().unwrap_or_else(|p| p.into_inner()).live_sids();
        let mut old_way = String::new();
        diff_session_events(&after, &before, &mut old_way);
        assert!(
            old_way.is_empty(),
            "the snapshot diff cannot see a cancelled-out pair — if it can, this \
             fixture is not reproducing the sub-tick window: {old_way:?}"
        );

        let got = text(&drain_session_events(&store, &mut cursor));
        assert!(
            got.contains(&format!("EVENT * session-created {sid}\n")),
            "the spawn must surface: {got:?}"
        );
        assert!(
            got.contains(&format!("EVENT * session-exited {sid} reason=ctl-close\n")),
            "the exit must surface, WITH the journalled reason as the trailing token: {got:?}"
        );
        assert!(
            got.find("session-created") < got.find("session-exited"),
            "in the order they happened: {got:?}"
        );
        // The cursor advanced exactly once: a second drain is silent (and, being
        // the idle path, never touches the registry's contents at all).
        assert!(
            drain_session_events(&store, &mut cursor).is_empty(),
            "a caught-up cursor emits nothing"
        );
    }

    /// RECOVERY: a cursor that fell past the journal's drop-oldest low-water
    /// rebuilds and diffs the whole set — the pre-journal behaviour, reached
    /// only in the case the journal cannot answer. It reports the NET set change
    /// and nothing else, and it leaves the cursor caught up.
    #[test]
    fn a_cursor_past_the_journal_low_water_rebuilds_and_diffs() {
        use crate::session_store::ROSTER_JOURNAL_CAP;
        let store = crate::session_store::new_store();
        // A cursor from before anything happened...
        let mut cursor = RosterCursor {
            seq: 0,
            known: std::collections::HashSet::new(),
        };
        // ...then churn far past the cap. One session (id 0) survives.
        let survivor;
        {
            let mut g = store.write().unwrap_or_else(|p| p.into_inner());
            let h0 = crate::session_store::test_handle(0);
            survivor = h0.sid.as_str().to_string();
            g.register(h0);
            for i in 1..=(ROSTER_JOURNAL_CAP as u64) {
                g.register(crate::session_store::test_handle(i));
                g.deregister_local(i);
            }
            assert!(
                g.roster_low_seq().expect("journal is non-empty") > cursor.seq + 1,
                "REACH: the fixture must push the cursor PAST the retained window, \
                 or this test silently exercises the fast path instead"
            );
        }

        let got = text(&drain_session_events(&store, &mut cursor));
        assert_eq!(
            got,
            format!("EVENT * session-created {survivor}\n"),
            "recovery reports the NET set change, not the evicted history"
        );
        assert!(
            drain_session_events(&store, &mut cursor).is_empty(),
            "recovery leaves the cursor caught up"
        );
    }

    /// The `events` digest emits `EVENT <sid> bell total=<n>` when the monotonic
    /// fired-bell count advances, and NOT on an unchanged re-scan (watermark).
    #[test]
    fn drain_bell_event_emits_on_new_bells() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let mut out = String::new();
        let bell = |t: &Arc<Mutex<Terminal>>| sample_engine_events(t, None).bell;
        // No bells yet: baseline 0, nothing emitted.
        let wm = drain_bell_event("4", bell(&term), 0, &mut out);
        assert!(out.is_empty() && wm == 0, "no bell yet: {out:?}");
        // A BEL fires: total advances to 1, event emitted.
        crate::term_lock(&term).process(b"\x07");
        out.clear();
        let wm = drain_bell_event("4", bell(&term), wm, &mut out);
        assert!(
            out.contains("EVENT 4 bell total=1\n"),
            "bell emitted: {out:?}"
        );
        // A re-scan with no new bell emits nothing.
        out.clear();
        let _ = drain_bell_event("4", bell(&term), wm, &mut out);
        assert!(
            out.is_empty(),
            "unchanged bell total emits nothing: {out:?}"
        );
    }

    /// The `events` digest surfaces new TURN records as `EVENT <sid> turn …`
    /// lines, scanning the ledger by id watermark exactly like block-complete:
    /// a record past the watermark emits once and advances it; a re-scan with no
    /// new record emits nothing (no double-report).
    #[test]
    fn drain_turn_events_emits_once_per_new_record() {
        use crate::turn_ledger::{TurnLedger, TurnRecord};
        let ledger = Arc::new(Mutex::new(TurnLedger::default()));
        let push = |id: u64, submitted: bool, status: &'static str| {
            ledger.lock().unwrap().push(TurnRecord {
                id,
                started_ms: id,
                dur_ms: 7,
                submitted,
                status,
                text: format!("m{id}"),
                screen_hash: id,
                seq: id,
            });
        };

        // Seed the watermark at the current high (a live stream ignores backlog).
        push(1, true, "settled");
        let mut wm = ledger.lock().unwrap().high_id();
        let mut out = String::new();
        wm = drain_turn_events(&ledger, "3", wm, &mut out);
        assert!(
            out.is_empty(),
            "backlog before subscription is not replayed"
        );

        // A NEW turn emits exactly one digest line and advances the watermark.
        push(2, false, "timeout");
        out.clear();
        wm = drain_turn_events(&ledger, "3", wm, &mut out);
        assert_eq!(out, "EVENT 3 turn 2 submitted=0 status=timeout dur_ms=7\n");
        assert_eq!(wm, Some(2));

        // Re-scan with nothing new: silent.
        out.clear();
        let wm2 = drain_turn_events(&ledger, "3", wm, &mut out);
        assert!(out.is_empty() && wm2 == Some(2), "no double-report");
    }

    /// `every-frame` mode re-emits the `cells` frame even when `content_seq` is
    /// unchanged (animation fidelity); coalesced mode emits nothing on no change.
    #[test]
    fn every_frame_reemits_cells_on_unchanged_seq() {
        let term = Arc::new(Mutex::new(Terminal::new(2, 4)));
        crate::term_lock(&term).process(b"x");
        let streams = TargetStreams {
            cells: true,
            ..Default::default()
        };
        // Coalesced: second call on unchanged seq emits nothing.
        let mut w = watch_on(1, &term);
        assert!(wake_text(&mut w, streams, true).starts_with("DELTA 1 "));
        assert!(
            wake_text(&mut w, streams, true).is_empty(),
            "coalesced: no re-emit on unchanged seq"
        );
        // every-frame: re-emits cells on unchanged seq.
        let mut w2 = Watch {
            non_coalesced: true,
            ..watch_on(1, &term)
        };
        assert!(wake_text(&mut w2, streams, true).starts_with("DELTA 1 "));
        assert!(
            wake_text(&mut w2, streams, true).contains(" cells "),
            "every-frame re-emits cells on unchanged seq",
        );
    }

    /// R5 efficiency: an every-frame subscriber does NOT re-emit on a bare liveness
    /// TIMEOUT (`woke == false`) — only a genuine wake re-serializes the grid. This
    /// is what keeps an idle animation-mode subscriber from burning a full styled
    /// gather every 250ms tick.
    #[test]
    fn every_frame_does_not_reemit_on_timeout() {
        let term = Arc::new(Mutex::new(Terminal::new(2, 4)));
        crate::term_lock(&term).process(b"x");
        let streams = TargetStreams {
            cells: true,
            ..Default::default()
        };
        let mut w = Watch {
            non_coalesced: true,
            ..watch_on(1, &term)
        };
        // First (genuine) wake emits the frame and catches the watermark up.
        assert!(wake_text(&mut w, streams, true).starts_with("DELTA 1 "));
        // A TIMEOUT tick on the now-unchanged seq re-emits NOTHING (the efficiency
        // win); a genuine wake still would (proven by the test above).
        assert!(
            wake_text(&mut w, streams, false).is_empty(),
            "every-frame must not re-emit on a bare liveness timeout",
        );
    }

    /// A subscribe target on a session the store never knew — which is what a
    /// deregistered (pane-closed) session looks like to [`prune_closed`], since the
    /// only thing it asks is whether the local id still resolves.
    fn dead_target(local_id: u64, fan: &Arc<ByteFanout>) -> (Store, Vec<ResolvedTarget>) {
        let target: ResolvedTarget = (
            local_id,
            Arc::new(Mutex::new(Terminal::new(4, 8))),
            fan.clone(),
            Arc::new(Mutex::new(TurnLedger::default())),
            Arc::new(Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
        );
        (crate::session_store::new_store(), vec![target])
    }

    /// 4.3 — the DATA-LOSS regression test. A session that closes with output still
    /// queued must deliver that tail, and deliver it BEFORE `EVENT <sid> exited`:
    /// `exited` is the client's end-of-stream marker, so anything after it is
    /// unreachable and anything dropped before it is silent loss (the protocol
    /// promises `GAP` as the ONLY loss marker).
    ///
    /// The session here is already gone when [`push_loop`] starts, so the first
    /// liveness pass classifies it dead before ANY live per-watch pass can run for
    /// it — which means every byte in the output below came from [`Closing::drain`]
    /// and from nowhere else. That is the exact ordering the old `Vec::retain` broke:
    /// it dropped the `Watch`, and with it the `ByteSubscription` holding the queue.
    #[test]
    fn a_closed_session_delivers_its_byte_tail_before_exited() {
        let fan = Arc::new(ByteFanout::new());
        let (store, targets) = dead_target(11, &fan);
        let streams = TargetStreams {
            bytes: true,
            events: true,
            ..Default::default()
        };
        let registry = new_registry();
        let mut sink: Vec<u8> = Vec::new();

        // `push_loop` subscribes to the fan-out itself, so the burst must be teed
        // from another thread once that subscription exists — a burst teed before it
        // has no queue to land in. The 40ms delay lands inside the first 250ms wait.
        let feeder = {
            let fan = fan.clone();
            std::thread::spawn(move || {
                // Wait on the FACT, not a timer. The tee must land after push_loop
                // has subscribed (a burst teed earlier has no queue) but before its
                // single 250ms wait expires — a 40ms sleep raced both edges: a
                // descheduled feeder tees too late, a descheduled push_loop has not
                // subscribed yet. `subscriber_count` makes the ordering explicit and
                // removes the window entirely.
                while fan.subscriber_count() == 0 {
                    std::thread::sleep(Duration::from_millis(1));
                }
                fan.tee(&Arc::from(&b"tail-bytes"[..]));
            })
        };
        push_loop(
            &registry,
            &store,
            &targets,
            PushScopes {
                streams,
                instance: InstanceStreams::default(),
                adopt: AdoptScope::none(),
            },
            PushOptions::default(),
            &mut sink,
        );
        feeder.join().unwrap();

        let out = String::from_utf8_lossy(&sink).into_owned();
        let bytes_at = out.find("BYTES 11 10\ntail-bytes\n");
        let exited_at = out.find("EVENT 11 exited\n");
        assert!(
            bytes_at.is_some(),
            "the queued tail survived the close: {out:?}"
        );
        assert!(exited_at.is_some(), "the close was reported: {out:?}");
        assert!(
            bytes_at < exited_at,
            "the tail precedes the end-of-stream marker: {out:?}"
        );
    }

    /// NEGATIVE CONTROL for [`a_closed_session_delivers_its_byte_tail_before_exited`]
    /// — the house prove-and-catch shape: a guard that would pass with the bug in
    /// place proves nothing.
    ///
    /// Here the SAME dead watch is pruned twice over. Dropping the returned
    /// [`Closing`] instead of draining it is precisely what `Vec::retain` used to do,
    /// and it yields nothing at all; draining the identical watch yields the burst.
    /// So the queue really was still undelivered at prune time, and it really is
    /// `drain`'s by-value consumption that rescues it.
    #[test]
    fn a_dropped_closing_loses_the_tail_negative_control() {
        let fan = Arc::new(ByteFanout::new());
        let (store, _targets) = dead_target(12, &fan);
        let streams = TargetStreams {
            bytes: true,
            events: true,
            ..Default::default()
        };
        let term = Arc::new(Mutex::new(Terminal::new(4, 8)));
        // Two independent subscriptions on one fan-out get one queue each, so both
        // watches below hold the SAME undelivered burst.
        let (a, b) = (fan.subscribe(), fan.subscribe());
        fan.tee(&Arc::from(&b"tail"[..]));

        let mut dropped = vec![Watch {
            byte_sub: Some(a),
            ..watch_on(12, &term)
        }];
        let closing = prune_closed(&store, &mut dropped);
        assert!(
            dropped.is_empty(),
            "dead on the FIRST pass: no live per-watch drain can have run for it"
        );
        assert_eq!(
            closing.len(),
            1,
            "the dead watch is handed back, not dropped"
        );
        drop(closing); // exactly what `retain` did
        assert!(
            prune_closed(&store, &mut dropped).is_empty(),
            "nothing is left to re-drain: the tail is unrecoverable once dropped"
        );

        let mut drained = vec![Watch {
            byte_sub: Some(b),
            ..watch_on(12, &term)
        }];
        let frames = prune_closed(&store, &mut drained)
            .pop()
            .expect("same store, same liveness verdict")
            .drain(streams);
        let out = String::from_utf8_lossy(&frame_bytes(&frames)).into_owned();
        assert!(
            out.contains("BYTES 12 4\ntail\n"),
            "the identical watch, DRAINED, still carries the burst: {out:?}"
        );
    }

    /// The second half of 4.3: the "every watched session closed" exit must sit BELOW
    /// the closing drain, not above it. With `events` off there is no `exited` frame
    /// at all, so a client whose only stream is `bytes` would otherwise get a bare
    /// socket EOF with its last burst destroyed and no marker of any kind — and the
    /// protocol promises `GAP` as the only loss marker.
    #[test]
    fn the_last_watch_closing_does_not_short_circuit_its_own_tail() {
        let fan = Arc::new(ByteFanout::new());
        let (store, targets) = dead_target(13, &fan);
        let streams = TargetStreams {
            bytes: true,
            ..Default::default()
        };
        let registry = new_registry();
        let mut sink: Vec<u8> = Vec::new();
        let feeder = {
            let fan = fan.clone();
            std::thread::spawn(move || {
                // Wait on the FACT, not a timer. The tee must land after push_loop
                // has subscribed (a burst teed earlier has no queue) but before its
                // single 250ms wait expires — a 40ms sleep raced both edges: a
                // descheduled feeder tees too late, a descheduled push_loop has not
                // subscribed yet. `subscriber_count` makes the ordering explicit and
                // removes the window entirely.
                while fan.subscriber_count() == 0 {
                    std::thread::sleep(Duration::from_millis(1));
                }
                fan.tee(&Arc::from(&b"last"[..]));
            })
        };
        push_loop(
            &registry,
            &store,
            &targets,
            PushScopes {
                streams,
                instance: InstanceStreams::default(),
                adopt: AdoptScope::none(),
            },
            PushOptions::default(),
            &mut sink,
        );
        feeder.join().unwrap();
        let out = String::from_utf8_lossy(&sink).into_owned();
        assert!(
            out.contains("BYTES 13 4\nlast\n"),
            "the sole watch's tail is emitted before the loop returns: {out:?}"
        );
        assert!(
            !out.contains("EVENT 13 exited"),
            "`exited` belongs to the events stream, which was not requested: {out:?}"
        );
    }
}
