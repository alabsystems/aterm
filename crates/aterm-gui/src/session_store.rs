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

use std::collections::HashMap;
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
    // retained scaffolding: only exercised by the handoff round-trip (spec-proven +
    // tested) which is not yet wired into the live GUI exec path.
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
    /// tolerated): the operator-set title/description/icon (`meta set …`) ride
    /// the handoff so a seamless re-exec re-materializes the session under the
    /// SAME operator-chosen identity — the OSC title above is the engine's, this
    /// is the user's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
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
}

// retained scaffolding: the seamless re-exec handoff manifest is spec-proven
// (`handoff_roundtrip_model`) + unit-tested, but not yet wired into the live GUI
// exec path; origin/main keeps evolving it, so annotate rather than delete.
#[allow(dead_code)]
impl SessionHandoff {
    /// Current schema of the handoff manifest.
    pub const SCHEMA: u32 = 1;

    /// Project a live [`SessionStore`] into the round-trippable manifest, in the
    /// store's stable `local_id` order (so restore preserves tab order).
    #[must_use]
    pub fn from_store(store: &SessionStore) -> Self {
        Self {
            schema: Self::SCHEMA,
            window: None,
            sessions: store
                .snapshot()
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
// retained scaffolding: the volatile fd/pid re-exec channel (RFC Rung 1b) is
// unit-tested but not yet wired into the live GUI exec path; origin/main keeps
// evolving it, so annotate rather than delete.
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

/// The process-wide registry. Keyed canonically by [`SessionId`]; a second index
/// bridges the GUI's `u64` ids to those sids. Both key spaces are mutated under
/// the one outer `RwLock`, so a register/deregister is atomic across them.
#[derive(Default)]
pub struct SessionStore {
    by_id: HashMap<SessionId, SessionHandle>,
    by_local: HashMap<u64, SessionId>,
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
        if !self.by_id.contains_key(&handle.sid) {
            handle
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .record("spawned", format!("state={}", handle.state.as_str()));
        }
        self.by_local.insert(handle.local_id, handle.sid.clone());
        self.by_id.insert(handle.sid.clone(), handle);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_session::EdgeTable;
    use aterm_session::sink::SinkWriter;

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

    fn handle_alive(local_id: u64, parent: Option<SessionId>) -> SessionHandle {
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
        store.register(handle_in_state(
            2,
            Some(root_sid.clone()),
            SessionState::Alive,
        ));
        store.register(handle_in_state(
            1,
            Some(root_sid.clone()),
            SessionState::Exited,
        ));
        store.set_title(1, "vim");

        let manifest = SessionHandoff::from_store(&store);
        assert_eq!(manifest.sessions.len(), 3, "every live session is packed");
        // The identity round-trip the model proves (no loss, no fabrication, ordered).
        assert!(
            manifest.roundtrips(),
            "serialize -> parse must be the identity"
        );

        // And explicitly: the parsed manifest re-materializes the exact set.
        let text = manifest.to_toml().unwrap();
        let restored = SessionHandoff::from_toml(&text).expect("parse");
        assert_eq!(restored, manifest);
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
