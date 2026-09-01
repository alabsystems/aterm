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
        aterm_toml::to_string(self).map_err(|e| format!("serialize handoff: {e}"))
    }

    /// Parse a handoff blob; `None` on unreadable/incompatible input (fail-safe: the
    /// new process starts a fresh session rather than restoring a corrupt manifest).
    #[must_use]
    pub fn from_toml(s: &str) -> Option<Self> {
        let h: Self = aterm_toml::from_str(s).ok()?;
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

/// WHY a session left the registry — the `reason=` token of an `exits` row, a
/// `closing` timeline event and a `session-exited` push. The dead used to tell no
/// tale: a driver whose peer vanished learned it from `ERR no such session` and
/// could never ask whether the shell ended, a human clicked ✕, a sibling ran
/// `close`, or the window went away. Every close path now says which, and
/// `Unknown` is the honest answer for a path that did not (never a guess).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    /// The child ended on its own (reader EOF → `Wake::Exit`).
    ShellExit,
    /// A control-socket `close` retired it (`by=` names the caller's sid).
    CtlClose,
    /// A human closed the tab/pane (✕, Cmd-W, the tab menu).
    UiClose,
    /// The window hosting it was closed (the red button).
    WindowClose,
    /// The application quit. RESERVED — not produced today: `app-quit` is in
    /// the wire vocabulary, but no path constructs it — quit is `el.exit()` and
    /// the process, ledger included, goes with it, so nothing deregisters
    /// through the store. A quit path that tears sessions down through the
    /// store is where this gets its first use; until then the `exits` and
    /// `subscribe` entries say "reserved".
    #[allow(dead_code)]
    AppQuit,
    /// The close path did not say.
    Unknown,
}

impl ExitReason {
    /// Every reason, in wire order — the `exits` formatting test walks this so a
    /// variant added without a wire token cannot compile past it.
    #[cfg(test)]
    pub const ALL: [ExitReason; 6] = [
        ExitReason::ShellExit,
        ExitReason::CtlClose,
        ExitReason::UiClose,
        ExitReason::WindowClose,
        ExitReason::AppQuit,
        ExitReason::Unknown,
    ];

    /// The stable wire token (`reason=<token>`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ExitReason::ShellExit => "shell-exit",
            ExitReason::CtlClose => "ctl-close",
            ExitReason::UiClose => "ui-close",
            ExitReason::WindowClose => "window-close",
            ExitReason::AppQuit => "app-quit",
            ExitReason::Unknown => "unknown",
        }
    }
}

/// WHO retired a session — the `by=` token. A control-socket close names the
/// caller's own session; a UI/window close is the human at the keyboard; a path
/// that cannot say writes `-`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExitActor {
    /// The sid of the control connection that issued the close.
    Sid(String),
    /// The human, through the window chrome or a keyboard shortcut.
    Human,
    /// Not attributable on that path.
    Unknown,
}

impl ExitActor {
    /// The stable wire token (`by=<sid|human|->`).
    #[must_use]
    pub fn as_wire(&self) -> &str {
        match self {
            ExitActor::Sid(sid) => sid.as_str(),
            ExitActor::Human => "human",
            ExitActor::Unknown => "-",
        }
    }
}

/// One entry on the roster lifecycle journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterRecord {
    /// Store-monotonic, gap-free, starting at 1. `0` is the "nothing has ever
    /// happened" watermark a fresh consumer can seed to safely.
    pub seq: u64,
    /// The stable sid that entered or left.
    pub sid: String,
    /// The process-local id it was routed by (`local=` on an `exits` row — the
    /// number `sessions` listed it under while it lived).
    pub local_id: u64,
    /// Which way it moved.
    pub change: RosterChange,
    /// When, in milliseconds since the process epoch — the SAME monotonic clock
    /// the session timeline and turn ledger stamp with
    /// ([`crate::turn_ledger::now_ms`]), so an `exit` row aligns against a
    /// `timeline` row without a wall-clock conversion.
    pub t_ms: u64,
    /// Why it left. Meaningful iff `change == Exited`; a `Created` record carries
    /// the neutral [`ExitReason::Unknown`].
    pub reason: ExitReason,
    /// The child's exit status when the shell-exit path recovered one (see
    /// [`SessionStore::note_exit_code`]); `None` (`exit_code=-` on the wire) when
    /// it did not: the child was hung up by a close rather than exiting, died by
    /// signal, was not this process's to reap, or had not yet become reapable at
    /// either of the ledger's two non-blocking looks (`App::shell_exit_code`).
    pub exit_code: Option<i32>,
    /// Who retired it. Meaningful iff `change == Exited`; neutral
    /// [`ExitActor::Unknown`] on a `Created` record.
    pub actor: ExitActor,
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
    /// Exit statuses noted by the shell-exit path (`Wake::Exit`), keyed by local
    /// id and CONSUMED by the deregister that follows. A side table rather than a
    /// handle field because the handle is cloned out per control request and its
    /// constructors are many; the status is known for the short window between
    /// the reader's EOF and the pane's teardown (indefinitely under `--hold`),
    /// which is exactly the window this table spans.
    exit_codes: HashMap<u64, i32>,
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
        let journal = first.then(|| (handle.sid.as_str().to_string(), handle.local_id));
        self.by_local.insert(handle.local_id, handle.sid.clone());
        self.by_id.insert(handle.sid.clone(), handle);
        if let Some((sid, local_id)) = journal {
            self.record_roster(
                sid,
                local_id,
                RosterChange::Created,
                ExitReason::Unknown,
                None,
                ExitActor::Unknown,
            );
        }
    }

    /// Append one roster lifecycle record, evicting the oldest past
    /// [`ROSTER_JOURNAL_CAP`]. Private and `&mut self`, so the ONLY way to reach
    /// it is through a mutator that already holds the store's write lock.
    fn record_roster(
        &mut self,
        sid: String,
        local_id: u64,
        change: RosterChange,
        reason: ExitReason,
        exit_code: Option<i32>,
        actor: ExitActor,
    ) {
        self.roster_seq += 1;
        if self.roster.len() == ROSTER_JOURNAL_CAP {
            self.roster.pop_front();
        }
        self.roster.push_back(RosterRecord {
            seq: self.roster_seq,
            sid,
            local_id,
            change,
            t_ms: crate::turn_ledger::now_ms(),
            reason,
            exit_code,
            actor,
        });
    }

    /// Note the exited child's status for the session with local id `local_id`,
    /// ahead of its deregistration. The shell-exit path calls this the instant
    /// the status is answerable (`Wake::Exit`, before teardown reaps and discards
    /// it); the deregister that follows moves it onto the journal row. A `None`
    /// records nothing — the row then says `exit_code=-`, which is the truth.
    pub fn note_exit_code(&mut self, local_id: u64, code: Option<i32>) {
        if let Some(code) = code {
            self.exit_codes.insert(local_id, code);
        }
    }

    /// Deregister with NO attribution — the reason and actor are `Unknown`, except
    /// that a session whose shell already `Exited` is journalled as a shell exit
    /// (the store knows that much on its own). TEST-ONLY: every production close
    /// reaches the registry through `App::retire_session_registration`, which
    /// always passes the gesture's attribution to
    /// [`deregister_local_as`](Self::deregister_local_as); this bare form exists
    /// for the lifecycle fixtures, and its `cfg(test)` is what keeps a new
    /// production path from deregistering without saying why.
    #[cfg(test)]
    pub fn deregister_local(&mut self, local_id: u64) -> Option<SessionId> {
        self.deregister_local_as(local_id, ExitReason::Unknown, ExitActor::Unknown)
    }

    /// Deregister the session with process-local id `local_id`, removing it from
    /// BOTH key spaces atomically and returning its stable sid (so the caller can
    /// retire external artifacts keyed by it — e.g. the sibling-discovery graph
    /// entry). `None` if it is unknown — a late deregister mirrors the existing
    /// `is_active_session` miss.
    ///
    /// `reason`/`actor` say why and by whom (the exit ledger's tale). A caller
    /// that passes [`ExitReason::Unknown`] on a session whose shell has already
    /// `Exited` gets [`ExitReason::ShellExit`] — the state is the evidence; an
    /// explicit reason always wins (under `--hold` a human closes an exited pane
    /// and that close IS a `ui-close`). The `closing` timeline event is recorded
    /// only when the resolved reason is known, immediately before the death mark
    /// and BEFORE the handle leaves either index, so a holder of the (Arc-shared)
    /// timeline that outlives the registry entry reads why the session went
    /// before it reads that it is gone. Exactly one such holder exists on the
    /// wire: a live `subscribe @<sid> events` watch, whose final pass pushes the
    /// row as `EVENT <local> closing reason= by=` ahead of `EVENT <local> exited`.
    /// The `timeline` verb cannot be asked for it after the close — the sid
    /// stops resolving in this very write; only a request that resolved its ctx
    /// just before it can still read the row — and the journal row (`exits`) is
    /// where the same facts stay answerable afterwards.
    pub fn deregister_local_as(
        &mut self,
        local_id: u64,
        reason: ExitReason,
        actor: ExitActor,
    ) -> Option<SessionId> {
        // The status is consumed whether or not the handle is still here: a late
        // duplicate deregister must not leave a stale code to be pinned on a
        // session that later reuses the local id.
        let exit_code = self.exit_codes.remove(&local_id);
        let sid = self.by_local.remove(&local_id)?;
        // The death mark, written WHILE the handle is still registered: the
        // timeline is Arc-shared, so a holder that kept the ctx (pool teardown
        // races, a live subscribe watch) reads the `closing` row and the final
        // `state-change` from the same object — and this write precedes the
        // index removal in program order, not merely inside the same lock hold,
        // so "before it is gone" is literally the code's order too.
        let resolved = self.by_id.get(&sid).map(|h| {
            let reason = if reason == ExitReason::Unknown && h.state == SessionState::Exited {
                ExitReason::ShellExit
            } else {
                reason
            };
            let mut tl = h.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            if reason != ExitReason::Unknown {
                tl.record("closing", closing_payload(reason, &actor));
            }
            tl.record("state-change", "state=closed".to_string());
            reason
        });
        if let Some(reason) = resolved {
            self.by_id.remove(&sid);
            // Journalled ONLY when the handle was really there, so the journal
            // records exactly the set transitions `by_id` performed — a
            // late/duplicate deregister writes nothing, matching the `?` above.
            self.record_roster(
                sid.as_str().to_string(),
                local_id,
                RosterChange::Exited,
                reason,
                exit_code,
                actor,
            );
        }
        Some(sid)
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

/// The `closing` event's payload: `reason=<token> by=<sid|human|->`. The sid is
/// pct-encoded like every other free value on a timeline row (a sid is plain
/// ASCII, so this is the identity today — the encode is the one-line guarantee).
fn closing_payload(reason: ExitReason, actor: &ExitActor) -> String {
    format!(
        "reason={} by={}",
        reason.as_str(),
        crate::control::pct_encode(actor.as_wire())
    )
}

thread_local! {
    /// The attribution the CURRENT close gesture carries (main thread only — every
    /// deregistration happens there), read by the one deregister funnel in
    /// `App::retire_session_registration`. See [`CloseAttribution`].
    static CLOSE_ATTRIBUTION: std::cell::RefCell<Option<(ExitReason, ExitActor)>> =
        const { std::cell::RefCell::new(None) };
}

/// A scoped statement of WHY the sessions retired inside it are being retired.
///
/// Every close reaches the registry through one funnel
/// (`teardown_session` → `retire_session_registration` → `deregister_local_as`),
/// but the gesture that started it is several calls up: a control-socket `close`,
/// the red window button, a tab ✕, Cmd-W, the `tab close` verb. Threading a
/// reason argument down through each of those chains would touch every
/// intermediate signature in the two busiest files of the crate; a scope the
/// INITIATOR enters, read at the funnel, names the reason with one line per
/// gesture and nothing in between.
///
/// The OUTERMOST initiator wins: `enter` is a no-op when a scope is already open,
/// so a `close` verb that lands in the same `close_tab_at` a tab ✕ uses keeps
/// its `ctl-close` even though `close_tab_at` claims `ui-close` for the gestures
/// that reach it directly. Dropping the guard that opened the scope closes it;
/// a close that happens with no scope open is `Unknown`, never a stale one.
pub(crate) struct CloseAttribution {
    /// Whether THIS guard opened the scope (and so must close it).
    opened: bool,
}

impl CloseAttribution {
    /// Open the scope for the current gesture unless one is already open. Bind the
    /// result (`let _closing = …`): the attribution lasts exactly as long as it.
    #[must_use = "the attribution lasts exactly as long as this guard lives"]
    pub(crate) fn enter(reason: ExitReason, actor: ExitActor) -> Self {
        let opened = CLOSE_ATTRIBUTION.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                *slot = Some((reason, actor));
                true
            } else {
                false
            }
        });
        Self { opened }
    }
}

impl Drop for CloseAttribution {
    fn drop(&mut self) {
        if self.opened {
            CLOSE_ATTRIBUTION.with(|slot| *slot.borrow_mut() = None);
        }
    }
}

/// The attribution of the close gesture in progress on this thread, or
/// `(Unknown, Unknown)` when none is open.
#[must_use]
pub(crate) fn current_close_attribution() -> (ExitReason, ExitActor) {
    CLOSE_ATTRIBUTION
        .with(|slot| slot.borrow().clone())
        .unwrap_or((ExitReason::Unknown, ExitActor::Unknown))
}

impl crate::App {
    /// Capture the exited child's status for the exit ledger. Called on the
    /// shell-exit path (`exit_session_logical`) — the one instant the status is
    /// answerable: the reader hit EOF, the child is a zombie or was just reaped
    /// by the status classifier, and teardown (which reaps and DISCARDS it) has
    /// not run. The deregister that follows moves the code onto the journal row.
    pub(crate) fn note_shell_exit_for_ledger(&mut self, session: u64) {
        let code = self.shell_exit_code(session);
        self.store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .note_exit_code(session, code);
    }

    /// The exited child's status code, from whichever party reaped it.
    ///
    /// `None` is the honest `exit_code=-`: the child died by signal, is not this
    /// process's to reap (an adopted session), or had not yet become a zombie at
    /// EITHER non-blocking look. That last one is a real window, not a
    /// hypothetical: the reader reports EOF the instant the child's last pty
    /// descriptor closes, and the kernel closes a dying process's descriptors
    /// before it retires the process, so the classifier's `WNOHANG` probe at
    /// `Wake::Exit` can find nothing to reap yet. This is the ONE bounded retry:
    /// the same non-blocking probe again, a few statements later in the SAME
    /// `Wake::Exit` dispatch (the classifier looks first, then the ledger), which
    /// in practice closes the window; a child still not reapable by then writes
    /// `-`, and the ledger says so rather than blocking the UI thread to find out.
    fn shell_exit_code(&self, session: u64) -> Option<i32> {
        use crate::session_status::{Outcome, Phase};
        // `tab_status` on (the default): `note_session_exit` already reaped the
        // child and published an EXACT lifecycle classification. Read the code
        // back from it — a second `waitpid` would answer ECHILD for a pid that is
        // no longer ours to wait for, and the classifier's answer is the same
        // status, not a guess. Only an `Exited` phase is that answer; any other
        // phase is a stale pre-exit observation and says nothing about the exit.
        if let Some(status) = self.session_status.status(session)
            && matches!(status.phase, Phase::Exited)
        {
            match status.last_outcome {
                Outcome::Success => return Some(0),
                Outcome::Failure { exit_code } => return Some(exit_code),
                // Killed by a signal: no code, and the child IS reaped.
                Outcome::Signal { .. } => return None,
                // The classifier's probe found nothing to reap (the window in
                // the doc above). Fall through to the retry: `child_reaped` is
                // still clear on this path, so the probe below is never a
                // second `waitpid` on a pid that was already collected.
                Outcome::None => {}
            }
        }
        // `tab_status` off, or the classifier's probe came up empty: nobody has
        // reaped yet, so the zombie — once it exists — still holds its status.
        // Take it with the same reap-and-latch the classifier uses —
        // `collect_exit_status` frees the pid, and a session that outlives the
        // reap (`--hold`) must never `killpg` a number the kernel has reissued.
        let pooled = self.pool.get(session)?;
        if pooled
            .child_reaped
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return None;
        }
        let collected = aterm_pty::collect_exit_status(pooled.pid);
        if collected.is_some() {
            pooled
                .child_reaped
                .store(true, std::sync::atomic::Ordering::Release);
        }
        match collected {
            Some(aterm_pty::ChildExit::Code(code)) => Some(code),
            Some(aterm_pty::ChildExit::Signal(_)) | None => None,
        }
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
        output_echo: Arc::new(crate::app_input::OutputEchoTracker::default()),
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
        fabric: crate::fabric::SessionFabric::default(),
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
                // A deregister of a session whose shell already `Exited` says
                // why before it says it is gone — the `closing` row is the
                // exit ledger's tale, recorded in the same store write as
                // the death mark (see `deregister_local_as`).
                (5, "closing", "reason=shell-exit by=-"),
                (6, "state-change", "state=closed"),
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

    /// The timeline of a handle's ctx, as `(kind, payload)` pairs, oldest-first.
    fn timeline_of(h: &SessionHandle) -> Vec<(&'static str, String)> {
        h.ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .since(None)
            .map(|e| (e.kind, e.payload.clone()))
            .collect()
    }

    /// THE EXIT LEDGER'S TALE: an attributed deregister journals WHY (reason),
    /// WHO (actor), WHEN (`t_ms`, on the timeline's clock) and the exit status
    /// the shell-exit path noted — and writes the `closing` event on the
    /// session's own timeline immediately before the death mark, so an `events`
    /// watch that outlives the registry entry reads the reason first.
    #[test]
    fn attributed_deregister_records_reason_actor_clock_and_exit_code() {
        let mut store = SessionStore::default();
        let h = handle(7, None);
        let sid = h.sid.as_str().to_string();
        store.register(h.clone());
        let before = crate::turn_ledger::now_ms();
        store.note_exit_code(7, Some(3));
        assert_eq!(
            store.deregister_local_as(7, ExitReason::CtlClose, ExitActor::Sid("s-caller".into())),
            Some(h.sid.clone())
        );
        let rec = store
            .roster_since(0)
            .find(|r| r.change == RosterChange::Exited)
            .expect("the exit was journalled")
            .clone();
        assert_eq!(rec.sid, sid);
        assert_eq!(rec.local_id, 7);
        assert_eq!(rec.reason, ExitReason::CtlClose);
        assert_eq!(rec.actor, ExitActor::Sid("s-caller".into()));
        assert_eq!(rec.exit_code, Some(3));
        assert!(
            rec.t_ms >= before && rec.t_ms <= crate::turn_ledger::now_ms(),
            "stamped on the process-epoch clock: {} not in [{before}, now]",
            rec.t_ms
        );
        // A `Created` row carries the neutral values, never an invented reason.
        let created = store
            .roster_since(0)
            .find(|r| r.change == RosterChange::Created)
            .expect("the birth was journalled");
        assert_eq!(
            (created.reason, created.exit_code, &created.actor),
            (ExitReason::Unknown, None, &ExitActor::Unknown)
        );
        // The session's timeline: spawned, then `closing` with the reason, THEN
        // the death mark — in that order.
        let tl = timeline_of(&h);
        assert_eq!(tl[0].0, "spawned");
        assert_eq!(
            &tl[1..],
            &[
                ("closing", "reason=ctl-close by=s-caller".to_string()),
                ("state-change", "state=closed".to_string()),
            ]
        );
    }

    /// An UNATTRIBUTED deregister of a session whose shell already `Exited` is a
    /// shell exit — the state is the evidence and the store knows it on its own.
    /// One that was still alive stays `Unknown` (no guess), and an unknown reason
    /// records NO `closing` event: the timeline says only what is known.
    #[test]
    fn unattributed_deregister_infers_shell_exit_from_the_exited_state_only() {
        let mut store = SessionStore::default();
        let exited = handle_in_state(1, None, SessionState::Exited);
        let alive = handle(2, None);
        store.register(exited.clone());
        store.register(alive.clone());
        store.deregister_local(1);
        store.deregister_local(2);
        let reasons: Vec<(String, ExitReason, ExitActor)> = store
            .roster_since(0)
            .filter(|r| r.change == RosterChange::Exited)
            .map(|r| (r.sid.clone(), r.reason, r.actor.clone()))
            .collect();
        assert_eq!(
            reasons,
            vec![
                (
                    exited.sid.as_str().to_string(),
                    ExitReason::ShellExit,
                    ExitActor::Unknown
                ),
                (
                    alive.sid.as_str().to_string(),
                    ExitReason::Unknown,
                    ExitActor::Unknown
                ),
            ]
        );
        assert!(
            timeline_of(&exited)
                .iter()
                .any(|(k, p)| *k == "closing" && p == "reason=shell-exit by=-"),
            "the inferred shell exit is a known reason: {:?}",
            timeline_of(&exited)
        );
        assert!(
            !timeline_of(&alive).iter().any(|(k, _)| *k == "closing"),
            "an unknown reason records no closing event: {:?}",
            timeline_of(&alive)
        );
    }

    /// An EXPLICIT reason outranks the `Exited` state: under `--hold` a human
    /// closes a pane whose shell ended minutes ago, and that close is a
    /// `ui-close` by the human, not a re-reported shell exit.
    #[test]
    fn an_explicit_reason_outranks_the_exited_state() {
        let mut store = SessionStore::default();
        store.register(handle_in_state(4, None, SessionState::Exited));
        store.deregister_local_as(4, ExitReason::UiClose, ExitActor::Human);
        let rec = store
            .roster_since(0)
            .find(|r| r.change == RosterChange::Exited)
            .unwrap();
        assert_eq!(
            (rec.reason, &rec.actor),
            (ExitReason::UiClose, &ExitActor::Human)
        );
    }

    /// A noted exit code is consumed by the deregister that follows and never
    /// bleeds onto a later session that reuses the local id; a `None` note
    /// records nothing (the row honestly says `-`).
    #[test]
    fn a_noted_exit_code_is_consumed_exactly_once() {
        let mut store = SessionStore::default();
        store.register(handle(9, None));
        store.note_exit_code(9, None);
        store.deregister_local(9);
        store.register(handle(9, None));
        store.note_exit_code(9, Some(7));
        store.deregister_local(9);
        store.register(handle(9, None));
        store.deregister_local(9);
        let codes: Vec<Option<i32>> = store
            .roster_since(0)
            .filter(|r| r.change == RosterChange::Exited)
            .map(|r| r.exit_code)
            .collect();
        assert_eq!(codes, vec![None, Some(7), None]);
    }

    /// The ledger stays BOUNDED with the richer rows: past the cap the oldest
    /// exits are dropped, the newest `ROSTER_JOURNAL_CAP` survive intact (their
    /// attribution included), and `since=` keyed on a journal seq yields exactly
    /// the newer rows — the cursor the `exits` verb pages with.
    #[test]
    fn attributed_exits_stay_bounded_and_page_by_seq() {
        let mut store = SessionStore::default();
        let n = ROSTER_JOURNAL_CAP as u64 + 5;
        for i in 0..n {
            store.register(handle(i, None));
            store.note_exit_code(i, Some(i32::try_from(i % 256).unwrap()));
            store.deregister_local_as(i, ExitReason::CtlClose, ExitActor::Sid(format!("s-{i}")));
        }
        let exits: Vec<&RosterRecord> = store
            .roster_since(0)
            .filter(|r| r.change == RosterChange::Exited)
            .collect();
        // Two rows per session; the retained window holds the newest CAP rows,
        // half of which are exits.
        assert_eq!(exits.len(), ROSTER_JOURNAL_CAP / 2);
        let newest = exits.last().unwrap();
        assert_eq!(newest.seq, store.roster_seq());
        assert_eq!(newest.actor, ExitActor::Sid(format!("s-{}", n - 1)));
        assert_eq!(
            newest.exit_code,
            Some(i32::try_from((n - 1) % 256).unwrap())
        );
        // `since=<seq of the third-newest exit>` → exactly the two newer exits.
        let third = exits[exits.len() - 3].seq;
        let newer: Vec<u64> = store
            .roster_since(third)
            .filter(|r| r.change == RosterChange::Exited)
            .map(|r| r.seq)
            .collect();
        assert_eq!(newer, vec![exits[exits.len() - 2].seq, newest.seq]);
        assert_eq!(store.roster_since(newest.seq).count(), 0, "caught up");
    }

    /// The close-attribution scope: absent → `Unknown`; the OUTERMOST initiator
    /// wins over an inner claim (a `close` verb reaching the tab ✕'s
    /// `close_tab_at` keeps `ctl-close`); and dropping the opener restores
    /// `Unknown` — never a stale attribution for the next unrelated close.
    #[test]
    fn close_attribution_scope_is_outermost_wins_and_restores() {
        assert_eq!(
            current_close_attribution(),
            (ExitReason::Unknown, ExitActor::Unknown)
        );
        {
            let _outer =
                CloseAttribution::enter(ExitReason::CtlClose, ExitActor::Sid("s-a".into()));
            {
                let _inner = CloseAttribution::enter(ExitReason::UiClose, ExitActor::Human);
                assert_eq!(
                    current_close_attribution(),
                    (ExitReason::CtlClose, ExitActor::Sid("s-a".into())),
                    "the inner claim does not override the initiator"
                );
            }
            assert_eq!(
                current_close_attribution().0,
                ExitReason::CtlClose,
                "dropping the inner (non-opener) guard leaves the scope open"
            );
        }
        assert_eq!(
            current_close_attribution(),
            (ExitReason::Unknown, ExitActor::Unknown),
            "dropping the opener closes the scope"
        );
    }

    /// Every reason has a distinct wire token and the actor's tokens are the
    /// documented three — the vocabulary the `exits`/`subscribe` docs promise.
    #[test]
    fn exit_reason_and_actor_wire_tokens() {
        let tokens: Vec<&str> = ExitReason::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(
            tokens,
            [
                "shell-exit",
                "ctl-close",
                "ui-close",
                "window-close",
                "app-quit",
                "unknown"
            ]
        );
        assert_eq!(ExitActor::Sid("s-z".into()).as_wire(), "s-z");
        assert_eq!(ExitActor::Human.as_wire(), "human");
        assert_eq!(ExitActor::Unknown.as_wire(), "-");
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
