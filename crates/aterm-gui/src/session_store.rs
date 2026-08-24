// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-wide SESSION REGISTRY (design P1.1) — the additive index that makes
//! every live session resolvable by its stable [`SessionId`] (and by the
//! process-local `u64` id the GUI already routes `Wake`s with), WITHOUT moving
//! the GUI's `Vec<Session>` pane model.
//!
//! ## Why it lives here (not in `aterm-session`)
//!
//! A [`SessionHandle`] holds an `Arc<Mutex<Terminal>>` (an `aterm-core` type) and
//! an `Arc<SessionCtx>` (a `aterm-gui` type). `aterm-session` deliberately depends
//! on NEITHER (it is the headless policy/transport core), so the registry that
//! binds the live engine handle to the fabric identity has to live in the binary
//! that owns both. The IDENTITY (`SessionId`/`LaunchNonce`) and the AUTHORITY
//! (`EdgeTable`/`decide_edge`) it gates on are still the `aterm-session` types — we
//! only add the in-process index over them.
//!
//! ## Discipline (the one hard rule)
//!
//! The registry is read on the control thread to resolve a cross-session target;
//! the resolver CLONES the `(term, sink, sid, nonce, ...)` tuple OUT of the store
//! and DROPS the store guard BEFORE locking the target `Terminal` — exactly the
//! clone-then-release discipline `resolve_active` uses. The store lock is NEVER
//! held across a `Terminal` lock, so two agents driving each other (A→B, B→A)
//! cannot deadlock on the registry.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use aterm_core::terminal::Terminal;
use aterm_session::{LaunchNonce, SessionId};

use crate::SessionCtx;

/// A session's lifecycle as the registry observes it. A session stays READABLE
/// after its command exits (`Exited`) until the pane is torn down and it is
/// deregistered. `Spawning` is the brief pre-`Alive` window: the spawn path
/// registers a handle `Spawning` the instant its PTY + engine exist, and the
/// session's own PTY reader thread flips it to `Alive` (via `Wake::Ready`) on its
/// FIRST live iteration. A fast shell makes that window vanishingly short; a slow
/// shell stays `Spawning` (and addressable) until its reader confirms live.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionState {
    /// Registered, engine + PTY live, but the reader thread has not yet confirmed
    /// its first iteration — the brief pre-`Alive` window. Input is safe in this
    /// state (the PTY master + sink already exist; bytes buffer in the kernel).
    Spawning,
    /// Live: a reader thread is feeding the engine.
    Alive,
    /// The command exited; the engine is still readable until the pane closes.
    Exited,
}

impl SessionState {
    /// The stable wire token for the `sessions` verb.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SessionState::Spawning => "spawning",
            SessionState::Alive => "alive",
            SessionState::Exited => "exited",
        }
    }

    /// Inverse of [`Self::as_str`] for the handoff manifest round-trip; unknown ⇒
    /// `Spawning` (fail-safe: a restored session is addressable + input-safe).
    // LOAD-BEARING WIRE FORMAT — see `SessionHandoff` below. Currently reached
    // only from tests because `take_incoming` re-derives live state rather than
    // trusting the carried string, but these spellings ARE the wire.
    #[allow(dead_code)]
    #[must_use]
    pub fn from_str(s: &str) -> Self {
        match s {
            "alive" => SessionState::Alive,
            "exited" => SessionState::Exited,
            _ => SessionState::Spawning,
        }
    }
}

/// One session's ROUND-TRIPPABLE metadata — the projection of a [`SessionHandle`]
/// that must survive a seamless re-exec (the live `Arc`s + the raw `master` fd are
/// re-established by the new process, not serialized). See [`SessionHandoff`].
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct SessionRecord {
    pub local_id: u64,
    pub sid: String,
    pub parent: Option<String>,
    pub state: String,
    pub title: String,
    /// SEAMLESS SCREEN CARRY (schema-1 additive, `default` ⇒ absent tolerated):
    /// the session's engine screen at handoff time, so the post-update window
    /// repaints the exact pre-update content (prompt included) instead of
    /// booting blank over a live shell — the "looks dead after update" fix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<ScreenCarry>,
    /// USER metadata (session-metadata stage 1; schema-1 additive, absent
    /// tolerated): a sanitized projection of the operator-set title/
    /// description/icon/role/attention (`meta set …`) for manifest
    /// inspection. HONEST SCOPE: adoption re-seeds user meta from the LAYOUT
    /// sidecar's restore leaves (`seed_restored_user_meta`), not from these
    /// fields — they are a write-side record, kept in lockstep with the leaf
    /// carrier so external readers of the manifest see the same identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
}

/// The screen half of the seamless handoff: the checkpoint's scalar projection
/// rides the TOML manifest; the grid byte blobs (binary `Line` codec — the same
/// codec scrollback offload already trusts across process restarts) ride
/// nonce-stamped sidecar files next to the manifest. The modern two-phase reader
/// requires the declared schema and every semantics-bearing meta key; the explicit
/// one-release legacy protocol alone tolerates schema 0. Any parse/read failure
/// rejects the entire adoption before ownership transfers—never a blank partial adopt.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct ScreenCarry {
    /// Required by the modern two-phase protocol. `0` is accepted only by the
    /// explicit v0.52/v0.53 one-channel bridge.
    #[serde(default)]
    pub schema: u32,
    /// The checkpoint minus its grid blobs (`aterm_core::terminal::CheckpointMeta`),
    /// as a JSON string: TOML cannot represent the meta's `None` fields, and JSON's
    /// null/absent tolerance is exactly the forward-compatible posture the carry
    /// wants (the reader is always the newer build; unknown fields are ignored,
    /// missing ones defaulted).
    pub meta: String,
    /// Sidecar file holding the main grid's `serialize_lines` bytes.
    pub grid_file: String,
    /// Sidecar file holding the alt grid's bytes, when an alt grid existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_grid_file: Option<String>,
}

impl ScreenCarry {
    pub const SCHEMA: u32 = 1;
}

/// Window-frame carry: the outgoing window's grid size and outer position, so
/// the post-update window reappears exactly where (and how big) the old one
/// was instead of at config defaults — the visible half of "seamless".
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct WindowCarry {
    /// Grid rows of the handed-off session's window.
    pub rows: u16,
    /// Grid cols.
    pub cols: u16,
    /// Outer window position (physical px), when known.
    #[serde(default)]
    pub outer_x: Option<i32>,
    #[serde(default)]
    pub outer_y: Option<i32>,
}

/// One carried connection edge — the TOKENLESS projection of a live
/// `(src, dst, op)` row (design §1.4#6). Plain strings on purpose: the wire
/// spellings ([`aterm_session::SessionId`]`::as_str` / [`aterm_session::Op`]
/// `::as_str`) ARE the manifest format, and an op the incoming build cannot
/// parse is dropped-but-audited at re-mint rather than failing the whole
/// adoption. The bearer token and launch nonce have no field here — the
/// §1.4#3 "no secrets at rest" FATAL is unrepresentable, not merely checked.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct ConnectionCarry {
    pub src: String,
    pub dst: String,
    pub op: String,
}

/// The SEAMLESS-RE-EXEC HANDOFF MANIFEST (proof-carrying DSU, RFC Rung 1a): the
/// serializable projection of the whole session set that the outgoing process writes
/// and the incoming (new) binary reads to re-materialize exactly the same tabs — no
/// session lost, none fabricated. The `handoff_roundtrip_model` (`aterm-spec`) proves
/// that obligation abstractly; [`SessionHandoff::roundtrips`] and the unit test below
/// bind it to this concrete serializer, so the modeled property is the one the
/// shipping code meets.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct SessionHandoff {
    pub schema: u32,
    pub sessions: Vec<SessionRecord>,
    /// Window-frame carry (schema-1 additive; absent in pre-carry manifests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowCarry>,
    /// CONNECTION CARRY (schema-1 additive, design §1.4#6; absent in
    /// pre-connections manifests): the tokenless `(src, dst, op)` triple of
    /// every live cross-session edge at handoff time — exactly what
    /// `EdgeTable::edges()` enumerates. The incoming process re-mints these
    /// through the one kind-bounded helper once its adopted sessions are
    /// registered (fresh tokens under fresh nonces); the old rows die with
    /// the old process. NEVER a token, NEVER a nonce (§1.4#3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connections: Vec<ConnectionCarry>,
}

// ⚠ LOAD-BEARING WIRE FORMAT — DO NOT "CLEAN UP" THIS SERIALIZATION.
//
// (The comment that used to sit here said this was "retained scaffolding … not
// yet wired into the live GUI exec path". That was false and dangerous: this
// type IS the shipping seamless update. `seamless::write_outgoing` writes it,
// `seamless::take_incoming` parses it, and `app_input`'s handoff worker sets it
// as the child's `ATERM_SEAMLESS_MANIFEST`.)
//
// The bytes this emits are hashed by the OUTGOING binary and read back by the
// INCOMING one, which is a DIFFERENT VERSION by definition. Changing the field
// set, the field order, the TOML spelling, or `ScreenCarry`'s JSON meta changes
// what the new binary sees. The adoption proof survives that only because both
// sides now hash the WIRE BYTES rather than a re-serialization
// (`seamless::layout_wire_digest` / `seamless::screen_wire_digest`) — keep it
// that way. Additive `#[serde(default, skip_serializing_if = "…")]` fields are
// the safe shape; anything else needs a protocol-shape bump.
#[allow(dead_code)]
impl SessionHandoff {
    /// Current schema of the handoff manifest.
    pub const SCHEMA: u32 = 1;

    /// Project a live [`SessionStore`] into the round-trippable manifest, in the
    /// store's stable `local_id` order (so restore preserves tab order).
    #[must_use]
    pub fn from_store(store: &SessionStore) -> Self {
        let handles = store.snapshot();
        // CONNECTION CARRY (design §1.4#6): every live edge row across every
        // registered session's table, projected TOKENLESS — the same
        // `EdgeTable::edges()` fold the `flows` verb aggregates (a row lives
        // only in its destination's table, so the concatenation has no
        // duplicates). Each table is a leaf lock (the meta-lock discipline
        // below — never a `Terminal` lock). Sorted `(src, dst, op)` so the
        // manifest bytes are stable for a given edge set.
        let mut connections: Vec<ConnectionCarry> = Vec::new();
        for h in &handles {
            let rows = {
                let edges = h.ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
                edges.edges()
            };
            connections.extend(rows.into_iter().map(|e| ConnectionCarry {
                src: e.src.as_str().to_string(),
                dst: e.dst.as_str().to_string(),
                op: e.op.as_str().to_string(),
            }));
        }
        connections.sort_by(|a, b| (&a.src, &a.dst, &a.op).cmp(&(&b.src, &b.dst, &b.op)));
        Self {
            schema: Self::SCHEMA,
            window: None,
            connections,
            sessions: handles
                .into_iter()
                .map(|h| {
                    // Project the operator metadata out of the shared ctx (the
                    // one copy; a leaf lock, taken after the store guard above
                    // already dropped — never across a Terminal lock).
                    let meta = h
                        .ctx
                        .meta
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .sanitized();
                    SessionRecord {
                        local_id: h.local_id,
                        sid: h.sid.as_str().to_string(),
                        parent: h.parent.as_ref().map(|p| p.as_str().to_string()),
                        state: h.state.as_str().to_string(),
                        title: h.title.clone(),
                        // The screen carry is attached by the seamless writer, which
                        // owns the sidecar files; the store projection has no engine.
                        screen: None,
                        user_title: meta.user_title.clone(),
                        description: meta.description.clone(),
                        icon: meta.icon.clone(),
                        role: meta.role.clone(),
                        attention: meta.attention.clone(),
                    }
                })
                .collect(),
        }
    }

    /// Serialize to the handoff blob (TOML — same format the updater's markers use).
    pub fn to_toml(&self) -> Result<String, String> {
        toml::to_string(self).map_err(|e| format!("serialize handoff: {e}"))
    }

    /// Parse a handoff blob; `None` on unreadable/incompatible input (fail-safe: the
    /// new process starts a fresh session rather than restoring a corrupt manifest).
    #[must_use]
    pub fn from_toml(s: &str) -> Option<Self> {
        let h: Self = toml::from_str(s).ok()?;
        (h.schema == Self::SCHEMA).then_some(h)
    }

    /// The concrete round-trip the `handoff_roundtrip_model` abstracts: serialize →
    /// parse yields an identical manifest (no session lost, none fabricated, order
    /// preserved). Returns `true` iff the round-trip is the identity.
    #[must_use]
    pub fn roundtrips(&self) -> bool {
        match self.to_toml() {
            Ok(text) => Self::from_toml(&text).as_ref() == Some(self),
            Err(_) => false,
        }
    }
}

/// The VOLATILE fd/pid channel for the seamless re-exec (RFC Rung 1b), kept SEPARATE
/// from [`SessionHandoff`] on purpose: a raw fd number + pid are process-lifetime
/// values, not serializable session state, and folding them into the manifest would
/// break the 1a round-trip identity proof (`session_handoff_roundtrips_the_whole_set`).
/// They ride an env string (`ATERM_SEAMLESS_FDS`) and JOIN the manifest on `local_id`.
///
/// Wire form: `"<local_id>=<fd>:<pid>,..."` (e.g. `"0=6:12345,1=7:12346"`). Encode/parse
/// are total + fail-safe: an unparseable entry is skipped (that tab cold-restarts) so a
/// corrupt/spoofed value can never fabricate a session — it can only fail closed.
// LOAD-BEARING: this is the LIVE `ATERM_SEAMLESS_FDS` channel. The handoff
// worker builds it (`app_input`), `seamless::write_outgoing` encodes it, and
// `seamless::take_incoming` decodes and joins it to the manifest on `local_id`.
#[allow(dead_code)]
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct HandoffFds {
    /// `(local_id, master fd number, child pid)` for each session handed across the exec.
    pub entries: Vec<(u64, i32, i32)>,
}

#[allow(dead_code)]
impl HandoffFds {
    /// Encode to the `ATERM_SEAMLESS_FDS` wire string.
    #[must_use]
    pub fn encode(&self) -> String {
        self.entries
            .iter()
            .map(|(lid, fd, pid)| format!("{lid}={fd}:{pid}"))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Parse the wire string; malformed entries are SKIPPED (fail-safe), never fabricated.
    #[must_use]
    pub fn decode(s: &str) -> Self {
        let entries = s
            .split(',')
            .filter(|e| !e.is_empty())
            .filter_map(|e| {
                let (lid, rest) = e.split_once('=')?;
                let (fd, pid) = rest.split_once(':')?;
                Some((lid.parse().ok()?, fd.parse().ok()?, pid.parse().ok()?))
            })
            .collect();
        Self { entries }
    }

    /// The `(fd, pid)` for a manifest record's `local_id`, if handed off. `None` ⇒ that
    /// session must cold-restart a fresh shell (never silently dropped).
    #[must_use]
    pub fn lookup(&self, local_id: u64) -> Option<(i32, i32)> {
        self.entries
            .iter()
            .find(|(lid, _, _)| *lid == local_id)
            .map(|(_, fd, pid)| (*fd, *pid))
    }
}

/// One registered session: its stable fabric identity, the process-local id the
/// GUI routes with, the live engine + sink handles, and its lifecycle/title.
///
/// `term`/`sink`/`ctx` are the SAME `Arc`s the owning `Session` holds, so a
/// cross-session read is literally `handle.term.lock()` — zero new data path,
/// fully live, zero-copy.
#[derive(Clone)]
pub struct SessionHandle {
    /// Stable, pid-free fabric identity (the canonical registry key).
    pub sid: SessionId,
    /// This launch's nonce — an edge binds to it so a restart under a reused id
    /// fails closed (confused-deputy safe). The cross-session gate reads the live
    /// `ctx.nonce` (same value); this mirror is recorded for cross-process restart
    /// safety per the design and for audit.
    #[allow(dead_code)]
    pub nonce: LaunchNonce,
    /// The process-local id the GUI's `Wake`/`Vec<Session>` routing uses.
    pub local_id: u64,
    /// The spawning session's `sid`, if any (the family tree; `None` for tab-0 /
    /// user-opened tabs).
    pub parent: Option<SessionId>,
    /// Lifecycle as the registry observes it.
    pub state: SessionState,
    /// The live window title (best-effort; updated on relabel).
    pub title: String,
    /// The live engine handle — shared with the owning `Session` (zero-copy read).
    pub term: Arc<Mutex<Terminal>>,
    /// This session's PTY master fd (for `signal`'s `tcgetpgrp`/`killpg`).
    pub master: i32,
    /// The per-session fabric context (sink + edge table + identity).
    pub ctx: Arc<SessionCtx>,
}

/// How many roster lifecycle records the store retains. Drop-oldest past this,
/// exactly like [`crate::turn_ledger::LEDGER_CAP`] and
/// [`crate::session_timeline::TIMELINE_CAP`] — a journal that grew without
/// bound would be a leak, and a consumer that falls further behind than this
/// has a documented, exact recovery path (see [`SessionStore::roster_since`]).
/// 512 membership changes is many hundreds of tab open/closes; a 4 Hz consumer
/// would have to miss ~2 minutes of continuous churn to fall out of it.
pub(crate) const ROSTER_JOURNAL_CAP: usize = 512;

/// A change to the registry's MEMBERSHIP — the only two things that can move
/// the live-session set. Deliberately NOT a state change: `Spawning → Alive →
/// Exited` all leave the session registered and readable, and the roster the
/// `sessions` stream reports is membership, not state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RosterChange {
    /// The sid entered the registry (first registration; a replace is not one).
    Created,
    /// The sid left the registry (`deregister_local`).
    Exited,
}

/// One entry on the roster lifecycle journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterRecord {
    /// Store-monotonic, gap-free, starting at 1. `0` is the "nothing has ever
    /// happened" watermark a fresh consumer can seed to safely.
    pub seq: u64,
    /// The stable sid that entered or left.
    pub sid: String,
    /// Which way it moved.
    pub change: RosterChange,
}

/// The process-wide registry. Keyed canonically by [`SessionId`]; a second index
/// bridges the GUI's `u64` ids to those sids. Both key spaces are mutated under
/// the one outer `RwLock`, so a register/deregister is atomic across them.
///
/// ## The roster journal (why the third field exists)
///
/// The registry had NO monotonic lifecycle log, so every consumer of "which
/// sessions appeared or disappeared" had to REBUILD the whole live set and diff
/// it against its own copy. The `subscribe … sessions` push loop did that on
/// every 250 ms wake — a fresh `HashSet<String>` (one allocation per live
/// session, plus hashing) and two `difference` passes, per subscriber, forever,
/// whether or not anything had changed.
///
/// That is not only wasteful, it is LOSSY, and the push loop said so in its own
/// doc comment: a sibling that both spawns AND exits inside one 250 ms window
/// appears in neither snapshot and emits neither event. A snapshot diff can only
/// ever report the NET change between two instants; the events that cancelled
/// out are unrecoverable because nothing wrote them down.
///
/// So write them down. `roster` is a bounded, drop-oldest, strictly-increasing
/// journal appended under the SAME `&mut self` (hence the same write lock) that
/// mutates `by_id`, so a session can never be registered without its record or
/// vice versa. A consumer keeps a `u64` watermark: one compare against
/// [`roster_seq`](Self::roster_seq) answers "did anything change?" in O(1), and
/// [`roster_since`](Self::roster_since) yields exactly the records it has not
/// seen — including both halves of a spawn/exit pair that landed inside one
/// tick.
#[derive(Default)]
pub struct SessionStore {
    by_id: HashMap<SessionId, SessionHandle>,
    by_local: HashMap<u64, SessionId>,
    /// The bounded roster lifecycle journal, oldest-first (see the type doc).
    roster: VecDeque<RosterRecord>,
    /// Monotone source of journal seqs. Keeps counting past eviction, so a
    /// consumer's watermark stays meaningful even after the ring has rolled.
    roster_seq: u64,
}

/// Shared handle to the registry, cloned into the control thread alongside the
/// existing `ActiveHandle`.
pub type Store = Arc<RwLock<SessionStore>>;

/// A new, empty, shared registry.
#[must_use]
pub fn new_store() -> Store {
    Arc::new(RwLock::new(SessionStore::default()))
}

impl SessionStore {
    /// Register (or replace) a handle, wiring BOTH key spaces atomically. Replacing
    /// an existing `sid` (e.g. a relabel) keeps the `by_local` bridge consistent.
    /// A FIRST registration records the `spawned` event on the session's timeline
    /// (a replace does not — the session already lived; its birth is on record).
    pub fn register(&mut self, handle: SessionHandle) {
        let first = !self.by_id.contains_key(&handle.sid);
        if first {
            handle
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record("spawned", format!("state={}", handle.state.as_str()));
        }
        // JOURNAL the membership change under the SAME `&mut self` (== the same
        // write lock) that mutates the two indexes, so no reader can ever observe
        // a registry and a journal that disagree. A REPLACE is not a membership
        // change and records nothing — which is precisely the rule the old
        // set-diff enforced implicitly by comparing sets of sids.
        let journal = first.then(|| handle.sid.as_str().to_string());
        self.by_local.insert(handle.local_id, handle.sid.clone());
        self.by_id.insert(handle.sid.clone(), handle);
        if let Some(sid) = journal {
            self.record_roster(sid, RosterChange::Created);
        }
    }

    /// Append one roster lifecycle record, evicting the oldest past
    /// [`ROSTER_JOURNAL_CAP`]. Private and `&mut self`, so the ONLY way to reach
    /// it is through a mutator that already holds the store's write lock.
    fn record_roster(&mut self, sid: String, change: RosterChange) {
        self.roster_seq += 1;
        if self.roster.len() == ROSTER_JOURNAL_CAP {
            self.roster.pop_front();
        }
        self.roster.push_back(RosterRecord {
            seq: self.roster_seq,
            sid,
            change,
        });
    }

    /// Deregister the session with process-local id `local_id`, removing it from
    /// BOTH key spaces atomically and returning its stable sid (so the caller can
    /// retire external artifacts keyed by it — e.g. the sibling-discovery graph
    /// entry). `None` if it is unknown — a late deregister mirrors the existing
    /// `is_active_session` miss.
    pub fn deregister_local(&mut self, local_id: u64) -> Option<SessionId> {
        match self.by_local.remove(&local_id) {
            Some(sid) => {
                // The death mark: record it on the (shared, Arc-held) timeline
                // BEFORE the handle drops out of the registry, so a holder that
                // kept the ctx (pool teardown races, a live subscribe watch)
                // still reads an honest final event.
                if let Some(h) = self.by_id.remove(&sid) {
                    h.ctx
                        .timeline
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .record("state-change", "state=closed".to_string());
                    // Journalled ONLY when the handle was really there, so the
                    // journal records exactly the set transitions `by_id`
                    // performed — a late/duplicate deregister writes nothing,
                    // matching the `None` arm below.
                    self.record_roster(sid.as_str().to_string(), RosterChange::Exited);
                }
                Some(sid)
            }
            None => None,
        }
    }

    /// Mark the session's lifecycle state (e.g. `Exited` on `Wake::Exit`). A no-op
    /// if the id is unknown. An ACTUAL transition (not a same-state re-mark) is
    /// recorded on the session timeline as a `state-change` event.
    pub fn set_state(&mut self, local_id: u64, state: SessionState) {
        if let Some(sid) = self.by_local.get(&local_id)
            && let Some(h) = self.by_id.get_mut(sid)
            && h.state != state
        {
            h.state = state;
            h.ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record("state-change", format!("state={}", state.as_str()));
        }
    }

    /// Confirm a session's reader thread is live: transition `Spawning → Alive`.
    /// MONOTONIC + fail-safe — only a still-`Spawning` handle flips. A handle that
    /// already raced to `Exited` (an instant-exit shell whose `Wake::Exit` landed
    /// first) is NOT resurrected, and an already-`Alive` handle (a duplicate/late
    /// readiness signal) is left untouched. Returns `true` IFF this call performed
    /// the transition; an unknown id or any non-`Spawning` state returns `false`.
    /// Idempotent: a second `Wake::Ready` for the same session is a cheap no-op.
    pub fn mark_alive(&mut self, local_id: u64) -> bool {
        if let Some(sid) = self.by_local.get(&local_id)
            && let Some(h) = self.by_id.get_mut(sid)
            && h.state == SessionState::Spawning
        {
            h.state = SessionState::Alive;
            h.ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record("state-change", "state=alive".to_string());
            return true;
        }
        false
    }

    /// Update the live title for a session (best-effort, on relabel). Takes `&str`
    /// (the caller no longer allocates a `String` per redraw) and only mutates on an
    /// ACTUAL change, so a no-op relabel reuses the existing buffer. No-op if unknown.
    /// An actual change is also recorded on the session timeline (`title-change`,
    /// pct-encoded) — the change-gate above it is what keeps the ring at human/
    /// program relabel rate, never the redraw rate.
    pub fn set_title(&mut self, local_id: u64, title: &str) {
        if let Some(sid) = self.by_local.get(&local_id)
            && let Some(h) = self.by_id.get_mut(sid)
            && h.title != title
        {
            h.title.clear();
            h.title.push_str(title);
            h.ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record(
                    "title-change",
                    format!("title={}", crate::control::pct_encode(title)),
                );
        }
    }

    /// Look up a handle by its stable [`SessionId`]. Total + fail-closed: an
    /// unknown id returns `None`.
    #[must_use]
    pub fn by_sid(&self, sid: &SessionId) -> Option<&SessionHandle> {
        self.by_id.get(sid)
    }

    /// Look up a handle by its process-local `u64` id. Total + fail-closed.
    #[must_use]
    pub fn by_local(&self, local_id: u64) -> Option<&SessionHandle> {
        self.by_local
            .get(&local_id)
            .and_then(|sid| self.by_id.get(sid))
    }

    /// Number of registered sessions.
    #[must_use]
    #[allow(dead_code)] // used by tests + the forward-compat subscribe cap (P1.3)
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    #[allow(dead_code)] // used by tests
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// A snapshot of every registered handle, for the `sessions` verb. Cloned so
    /// the caller can drop the store guard before formatting (and never holds it
    /// across a `Terminal` lock). Sorted by `local_id` for a stable listing.
    #[must_use]
    pub fn snapshot(&self) -> Vec<SessionHandle> {
        let mut v: Vec<SessionHandle> = self.by_id.values().cloned().collect();
        v.sort_by_key(|h| h.local_id);
        v
    }

    /// The set of live session sids — a LIGHT read for the subscribe `sessions`
    /// lifecycle diff, which runs every wake (incl. idle 250ms ticks) for a sessions
    /// subscriber. Unlike [`snapshot`](Self::snapshot) it clones only the sid strings
    /// (not whole handles) and does not sort — a set needs neither.
    #[must_use]
    pub fn live_sids(&self) -> std::collections::HashSet<String> {
        self.by_id
            .values()
            .map(|h| h.sid.as_str().to_string())
            .collect()
    }

    /// The roster journal's HIGH-WATER: the seq of the newest membership change,
    /// or `0` when the registry has never gained or lost a session.
    ///
    /// This is the whole point of the journal for an idle consumer: one integer
    /// compare against its own watermark replaces building a `HashSet<String>`
    /// of every live sid and running two set differences over it — per
    /// subscriber, per 250 ms wake, forever.
    #[must_use]
    pub fn roster_seq(&self) -> u64 {
        self.roster_seq
    }

    /// The LOWEST retained journal seq, or `None` when nothing is retained.
    ///
    /// A consumer whose watermark `w` satisfies `w + 1 < low_seq` has fallen
    /// past the drop-oldest window: records it never saw are gone, so a delta
    /// replay would silently skip them. That consumer must REBUILD — and the
    /// recovery path is the whole-set diff this journal replaced, which is still
    /// exactly right for the job, just no longer paid on every tick.
    #[must_use]
    pub fn roster_low_seq(&self) -> Option<u64> {
        self.roster.front().map(|r| r.seq)
    }

    /// Journal records with `seq > after`, oldest-first.
    ///
    /// Seqs are minted by one counter under the write lock and pushed to the
    /// back, so they are strictly increasing across the deque and
    /// `partition_point` seeks to the first unseen record in O(log n); the walk
    /// that follows is O(records the caller actually missed). A watermark below
    /// the retained low-water yields everything retained — callers must test
    /// [`roster_low_seq`](Self::roster_low_seq) first if they need to know that
    /// the prefix was lossy (see its doc).
    pub fn roster_since(&self, after: u64) -> impl Iterator<Item = &RosterRecord> {
        let start = self.roster.partition_point(|r| r.seq <= after);
        self.roster.range(start..)
    }

    /// Every registered handle, BY REFERENCE and in no particular order.
    ///
    /// The zero-allocation read: unlike [`snapshot`](Self::snapshot) it clones
    /// nothing and does not sort, so a caller that only wants to pick out the
    /// handles matching a predicate (a `@*` subscription adopting the sessions
    /// it is not yet watching) pays for what it takes and nothing else. Order is
    /// unspecified BECAUSE it is `HashMap` order — a caller that needs a stable
    /// listing wants `snapshot`, and saying so here keeps the two from being
    /// confused.
    pub fn live_handles(&self) -> impl Iterator<Item = &SessionHandle> {
        self.by_id.values()
    }
}

/// A fully-formed, `Alive` [`SessionHandle`] with a fresh sid, a headless
/// engine and a real (fd-less) [`SessionCtx`] — TEST ONLY.
///
/// Hoisted out of this file's `mod tests` so the SUBSCRIBE tests can build a
/// genuinely registered session and drive the roster journal through the real
/// `register`/`deregister_local` mutators. A hand-rolled second fixture over
/// there would be a copy of this one that could silently drift from the real
/// handle shape, which is the failure mode a lifecycle test can least afford.
#[cfg(test)]
pub(crate) fn test_handle(local_id: u64) -> SessionHandle {
    handle_alive(local_id, None)
}

#[cfg(test)]
fn handle_alive(local_id: u64, parent: Option<SessionId>) -> SessionHandle {
    use aterm_session::EdgeTable;
    use aterm_session::sink::SinkWriter;
    let sid = SessionId::generate();
    let nonce = LaunchNonce::generate();
    let ctx = Arc::new(SessionCtx {
        sink: Arc::new(SinkWriter::new(-1)),
        edges: Mutex::new(EdgeTable::new()),
        self_id: sid.clone(),
        nonce,
        turn_lease: Mutex::new(None),
        cast: Arc::new(Mutex::new(crate::cast::CastRecorder::new(80, 24))),
        temporal: Arc::new(Mutex::new(crate::temporal::TemporalRecorder::new())),
        byte_fanout: Arc::new(crate::cast::ByteFanout::new()),
        turns: Arc::new(std::sync::Mutex::new(
            crate::turn_ledger::TurnLedger::default(),
        )),
        meta: std::sync::Mutex::new(crate::session_timeline::SessionMeta::default()),
        app_kitty: std::sync::Mutex::new(crate::app_kitty::AppKittySlot::default()),
        timeline: Arc::new(std::sync::Mutex::new(
            crate::session_timeline::SessionTimeline::default(),
        )),
    });
    SessionHandle {
        sid,
        nonce,
        local_id,
        parent,
        state: SessionState::Alive,
        title: format!("tab-{local_id}"),
        term: Arc::new(Mutex::new(Terminal::new(24, 80))),
        master: -1,
        ctx,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(local_id: u64, parent: Option<SessionId>) -> SessionHandle {
        handle_in_state(local_id, parent, SessionState::Alive)
    }

    fn handle_in_state(
        local_id: u64,
        parent: Option<SessionId>,
        state: SessionState,
    ) -> SessionHandle {
        let mut h = handle_alive(local_id, parent);
        h.state = state;
        h
    }

    #[test]
    fn register_indexes_both_key_spaces_and_deregister_clears_both() {
        let mut store = SessionStore::default();
        let h = handle(7, None);
        let sid = h.sid.clone();
        store.register(h);

        assert_eq!(store.len(), 1);
        assert!(store.by_sid(&sid).is_some(), "resolvable by sid");
        assert!(store.by_local(7).is_some(), "resolvable by local id");
        assert_eq!(store.by_local(7).unwrap().sid, sid, "both keys agree");

        // Deregister clears BOTH key spaces atomically and yields the stable sid
        // (the key external artifacts — e.g. graph entries — are retired by).
        assert_eq!(store.deregister_local(7), Some(sid.clone()));
        assert!(store.by_sid(&sid).is_none(), "sid index cleared");
        assert!(store.by_local(7).is_none(), "local index cleared");
        assert!(store.is_empty());
        // A second deregister is a no-op (late/duplicate close).
        assert_eq!(store.deregister_local(7), None);
    }

    #[test]
    fn unknown_lookup_is_fail_closed_none() {
        let store = SessionStore::default();
        assert!(store.by_local(999).is_none());
        assert!(store.by_sid(&SessionId::new("s-nope")).is_none());
    }

    #[test]
    fn snapshot_is_sorted_by_local_id_and_carries_parent() {
        let mut store = SessionStore::default();
        let root = handle(0, None);
        let root_sid = root.sid.clone();
        store.register(root);
        store.register(handle(2, Some(root_sid.clone())));
        store.register(handle(1, Some(root_sid.clone())));

        let snap = store.snapshot();
        assert_eq!(
            snap.iter().map(|h| h.local_id).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(snap[0].parent, None, "root has no parent");
        assert_eq!(
            snap[1].parent.as_ref(),
            Some(&root_sid),
            "child links to root"
        );
    }

    /// RFC Rung 1a — the CONCRETE binding of `handoff_roundtrip_model`: the real
    /// `SessionHandoff` serializer round-trips a multi-session store EXACTLY — no
    /// session lost, none fabricated, order + parent links preserved. This is what
    /// makes the ty proof about shipping code, not a toy.
    #[test]
    fn session_handoff_roundtrips_the_whole_set() {
        let mut store = SessionStore::default();
        let root = handle(0, None);
        let root_sid = root.sid.clone();
        store.register(root);
        let peer = handle_in_state(2, Some(root_sid.clone()), SessionState::Alive);
        let peer_sid = peer.sid.clone();
        let peer_ctx = peer.ctx.clone();
        store.register(peer);
        store.register(handle_in_state(
            1,
            Some(root_sid.clone()),
            SessionState::Exited,
        ));
        store.set_title(1, "vim");
        // A live connection root → peer (`both` = pull + push, three rows in the
        // PEER's table) must ride the manifest as TOKENLESS triples (§1.4#6).
        {
            let mut edges = peer_ctx.edges.lock().unwrap();
            let minted = edges.grant_connection(
                &root_sid,
                &peer_sid,
                aterm_session::ConnectionKind::Both,
                &peer_ctx.nonce,
            );
            assert_eq!(minted.len(), 3, "both mints its exact op set");
        }

        let manifest = SessionHandoff::from_store(&store);
        assert_eq!(manifest.sessions.len(), 3, "every live session is packed");
        // The carry is exactly the `edges()` enumeration, `(src, dst, op)`-sorted
        // wire spellings — and nothing else (no token field EXISTS to leak).
        let carry = |op: &str| ConnectionCarry {
            src: root_sid.as_str().to_string(),
            dst: peer_sid.as_str().to_string(),
            op: op.to_string(),
        };
        assert_eq!(
            manifest.connections,
            vec![carry("read-screen"), carry("signal"), carry("write-input")],
            "the three rows one `both` mints, tokenless and sorted"
        );
        // The identity round-trip the model proves (no loss, no fabrication, ordered).
        assert!(
            manifest.roundtrips(),
            "serialize -> parse must be the identity"
        );

        // And explicitly: the parsed manifest re-materializes the exact set.
        let text = manifest.to_toml().unwrap();
        // §1.4#3 REDACTION PROOF on the printed manifest: an `EdgeToken` is 32
        // bytes = 64 hex chars; no run of that width may appear anywhere in the
        // wire (sids are 20 hex, so this scan cannot false-positive on them).
        assert!(
            !text
                .as_bytes()
                .windows(64)
                .any(|w| w.iter().all(u8::is_ascii_hexdigit)),
            "no bearer-token-width hex blob may ride the manifest: {text}"
        );
        let restored = SessionHandoff::from_toml(&text).expect("parse");
        assert_eq!(restored, manifest);
        assert_eq!(
            restored.connections, manifest.connections,
            "the tokenless triples round-trip verbatim"
        );
        assert_eq!(
            restored
                .sessions
                .iter()
                .map(|r| r.local_id)
                .collect::<Vec<_>>(),
            vec![0, 1, 2],
            "tab order preserved"
        );
        assert_eq!(
            restored.sessions[1].parent.as_deref(),
            Some(root_sid.as_str())
        );
        assert_eq!(restored.sessions[1].state, "exited");
        assert_eq!(restored.sessions[1].title, "vim");
        // SessionState survives the string round-trip.
        assert_eq!(
            SessionState::from_str(&restored.sessions[1].state),
            SessionState::Exited
        );

        // Fail-safe: a corrupt / wrong-schema blob parses to None (fresh start), never panics.
        assert!(SessionHandoff::from_toml("not = [valid").is_none());
        assert!(SessionHandoff::from_toml("schema = 999\nsessions = []").is_none());
    }

    /// RFC Rung 1b — the fd/pid channel round-trips and is fail-safe. It is SEPARATE
    /// from the manifest (so the 1a proof holds); malformed entries are skipped, never
    /// fabricated, so a corrupt/spoofed `ATERM_SEAMLESS_FDS` fails closed.
    #[test]
    fn handoff_fds_encode_decode_roundtrips_and_fails_safe() {
        let fds = HandoffFds {
            entries: vec![(0, 6, 12345), (1, 7, 12346), (2, 9, 12347)],
        };
        let wire = fds.encode();
        assert_eq!(wire, "0=6:12345,1=7:12346,2=9:12347");
        assert_eq!(
            HandoffFds::decode(&wire),
            fds,
            "encode -> decode is the identity"
        );
        assert_eq!(fds.lookup(1), Some((7, 12346)));
        assert_eq!(
            fds.lookup(99),
            None,
            "an unlisted session has no fd (cold-restarts)"
        );

        // Fail-safe parsing: garbage / partial entries are dropped, not fabricated.
        let mixed = HandoffFds::decode("0=6:100,junk,1=7,2=x:y,3=8:200,");
        assert_eq!(
            mixed.entries,
            vec![(0, 6, 100), (3, 8, 200)],
            "only well-formed entries survive"
        );
        assert_eq!(HandoffFds::decode(""), HandoffFds::default());
    }

    #[test]
    fn spawning_session_is_registered_addressable_and_becomes_alive() {
        // The async-spawn path: a session is registered `Spawning` (engine + PTY
        // live, reader not yet confirmed) and stays fully addressable by BOTH keys
        // throughout. Its reader's first iteration flips it `Alive` via `mark_alive`.
        let mut store = SessionStore::default();
        let h = handle_in_state(5, None, SessionState::Spawning);
        let sid = h.sid.clone();
        store.register(h);

        // Addressable + observably `Spawning` the whole pre-Alive window.
        assert_eq!(store.len(), 1);
        assert_eq!(store.by_local(5).unwrap().state, SessionState::Spawning);
        assert_eq!(store.by_sid(&sid).unwrap().state, SessionState::Spawning);
        assert_eq!(store.by_local(5).unwrap().sid, sid, "both keys agree");

        // Reader confirms live: Spawning -> Alive, reported as the transitioning call.
        assert!(
            store.mark_alive(5),
            "first readiness performs the transition"
        );
        assert_eq!(store.by_local(5).unwrap().state, SessionState::Alive);
        assert_eq!(
            store.by_sid(&sid).unwrap().state,
            SessionState::Alive,
            "the transition is visible via BOTH key spaces (one handle)"
        );
        // Still fully addressable after the transition.
        assert_eq!(store.by_local(5).unwrap().sid, sid);
    }

    #[test]
    fn mark_alive_is_monotonic_idempotent_and_fail_safe() {
        let mut store = SessionStore::default();
        store.register(handle_in_state(8, None, SessionState::Spawning));

        // A SECOND readiness signal (duplicate/late `Wake::Ready`) is a no-op.
        assert!(store.mark_alive(8));
        assert!(!store.mark_alive(8), "already Alive: no second transition");
        assert_eq!(store.by_local(8).unwrap().state, SessionState::Alive);

        // An instant-exit shell whose `Wake::Exit` landed first: a stray late
        // readiness must NOT resurrect an Exited handle.
        store.register(handle_in_state(9, None, SessionState::Exited));
        assert!(!store.mark_alive(9), "Exited never flips back to Alive");
        assert_eq!(store.by_local(9).unwrap().state, SessionState::Exited);

        // Unknown id is a fail-closed no-op, never a panic.
        assert!(!store.mark_alive(404));
    }

    /// SESSION-METADATA stage 1 — the store mutators are the lifecycle RECORD
    /// SITES: register (first time only) records `spawned`, `mark_alive` and an
    /// actual `set_state` transition record `state-change`, an actual `set_title`
    /// records `title-change` (pct-encoded), and deregistration records the
    /// closing `state-change` on the (Arc-shared) timeline. No-op re-marks record
    /// NOTHING, so the ring fills at lifecycle rate, never call rate.
    #[test]
    fn store_mutators_record_ordered_timeline_events() {
        let mut store = SessionStore::default();
        let h = handle_in_state(4, None, SessionState::Spawning);
        let timeline = h.ctx.timeline.clone();
        store.register(h.clone());
        // Re-registering the same sid (a relabel/replace) is NOT a second birth.
        store.register(h);
        assert!(store.mark_alive(4));
        assert!(!store.mark_alive(4), "no-op re-mark records nothing");
        store.set_title(4, "vim main.rs");
        store.set_title(4, "vim main.rs"); // unchanged: nothing recorded
        store.set_state(4, SessionState::Exited);
        store.set_state(4, SessionState::Exited); // unchanged: nothing recorded
        store.deregister_local(4);

        let tl = timeline.lock().unwrap();
        let got: Vec<(u64, &str, &str)> = tl
            .since(None)
            .map(|e| (e.id, e.kind, e.payload.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (1, "spawned", "state=spawning"),
                (2, "state-change", "state=alive"),
                (3, "title-change", "title=vim%20main.rs"),
                (4, "state-change", "state=exited"),
                (5, "state-change", "state=closed"),
            ],
            "one ordered, monotonic-id event per ACTUAL lifecycle change"
        );
    }

    /// The journal records exactly the MEMBERSHIP transitions `by_id` performs —
    /// one `Created` per FIRST registration (never a replace), one `Exited` per
    /// registration that was really removed (never a late/duplicate deregister)
    /// — with gap-free, strictly increasing seqs.
    ///
    /// The differential half is the load-bearing one: replaying the journal from
    /// seq 0 must reconstruct the SAME live set `live_sids()` reports. That is
    /// what makes a delta-driven consumer equivalent to the whole-set diff it
    /// replaces, and it is checked after every step, not just at the end.
    #[test]
    fn roster_journal_mirrors_membership_exactly() {
        let mut store = SessionStore::default();
        assert_eq!(store.roster_seq(), 0, "a virgin store has no history");
        assert_eq!(store.roster_low_seq(), None);
        assert_eq!(store.roster_since(0).count(), 0);

        let replay = |st: &SessionStore| -> std::collections::HashSet<String> {
            let mut set = std::collections::HashSet::new();
            for r in st.roster_since(0) {
                match r.change {
                    RosterChange::Created => {
                        set.insert(r.sid.clone());
                    }
                    RosterChange::Exited => {
                        set.remove(&r.sid);
                    }
                }
            }
            set
        };

        let a = handle(1, None);
        let b = handle(2, None);
        let (sid_a, sid_b) = (a.sid.as_str().to_string(), b.sid.as_str().to_string());
        store.register(a.clone());
        assert_eq!(replay(&store), store.live_sids());
        // A REPLACE of the same sid is not a membership change.
        store.register(a);
        assert_eq!(store.roster_seq(), 1, "a replace journals nothing");
        store.register(b);
        assert_eq!(replay(&store), store.live_sids());
        // A late/duplicate deregister journals nothing either.
        store.deregister_local(1);
        assert_eq!(store.deregister_local(1), None);
        assert_eq!(
            store.roster_seq(),
            3,
            "the duplicate deregister journalled nothing"
        );
        assert_eq!(replay(&store), store.live_sids());

        let got: Vec<(u64, &str, RosterChange)> = store
            .roster_since(0)
            .map(|r| (r.seq, r.sid.as_str(), r.change))
            .collect();
        assert_eq!(
            got,
            vec![
                (1, sid_a.as_str(), RosterChange::Created),
                (2, sid_b.as_str(), RosterChange::Created),
                (3, sid_a.as_str(), RosterChange::Exited),
            ]
        );
        // `since` is a suffix keyed on the watermark, at every position.
        assert_eq!(store.roster_since(1).count(), 2);
        assert_eq!(store.roster_since(3).count(), 0);
        assert_eq!(store.roster_since(99).count(), 0, "past the high = nothing");
        assert_eq!(store.roster_low_seq(), Some(1));
    }

    /// The journal is BOUNDED and says so honestly: past the cap it drops
    /// oldest, `roster_seq` keeps counting (so a watermark stays meaningful),
    /// and `roster_low_seq` rises to advertise exactly where the retained window
    /// now starts — which is the signal a fallen-behind consumer needs in order
    /// to know it must rebuild rather than replay a lossy delta.
    #[test]
    fn roster_journal_is_bounded_and_advertises_its_low_water() {
        let mut store = SessionStore::default();
        // Two membership changes per iteration (register + deregister), so the
        // journal fills well past the cap.
        for i in 0..(ROSTER_JOURNAL_CAP as u64) {
            store.register(handle(i, None));
            store.deregister_local(i);
        }
        let total = 2 * ROSTER_JOURNAL_CAP as u64;
        assert_eq!(
            store.roster_seq(),
            total,
            "seqs keep counting past eviction"
        );
        assert_eq!(
            store.roster_since(0).count(),
            ROSTER_JOURNAL_CAP,
            "retention is capped"
        );
        assert_eq!(
            store.roster_low_seq(),
            Some(total - ROSTER_JOURNAL_CAP as u64 + 1),
            "the low-water names the oldest retained seq"
        );
        assert!(
            store.is_empty(),
            "every session registered was deregistered"
        );
    }

    #[test]
    fn set_state_and_title_mutate_in_place() {
        let mut store = SessionStore::default();
        store.register(handle(3, None));
        store.set_state(3, SessionState::Exited);
        store.set_title(3, "renamed");
        let h = store.by_local(3).unwrap();
        assert_eq!(h.state, SessionState::Exited);
        assert_eq!(h.title, "renamed");
        // Unknown ids are no-ops, not panics.
        store.set_state(99, SessionState::Exited);
        store.set_title(99, "x");
    }
}
