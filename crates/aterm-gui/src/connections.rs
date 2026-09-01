// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GUI CONNECTION core (design §1.4, §2.3, §4.1): the process-local record of
//! every connection this instance minted, the connect/disconnect acts over it,
//! the per-session role predicate the connection mark renders, and the close-time
//! source sweep.
//!
//! ## Why a record store exists at all
//!
//! The per-session [`EdgeTable`]s are THE authority record (design §1.4#2) — but
//! their bearer tokens are deliberately non-enumerable, and §1.4#3 forbids token
//! files and token env vars. So the tokens a connection minted live ONLY here, in
//! the process-local [`ConnectionRecord`] store, held for exactly two purposes:
//! DISSOLUTION (disconnect revokes the precise rows it minted, no more) and the
//! seamless-update handoff re-mint. Nothing here is an authority gate:
//! `decide_edge` over the destination's table remains the one enforcement point,
//! and dropping a record without revoking (or vice versa) can only ever LOSE
//! authority, never widen it.
//!
//! Ownership mirrors [`crate::proxy`]'s `PROXIES`: one process-wide table behind
//! a `OnceLock` (a process has exactly one connection fabric), with the mutating
//! helpers taking `&ConnectionStore` so tests run against a private store.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use aterm_session::{ConnectionKind, EdgeTable, EdgeToken, LaunchNonce, Op, SessionId};

use crate::session_edge_audit::{self, EdgeAction};
use crate::session_store::Store;

/// One live connection `src → dst` and the exact edge rows it minted into the
/// DESTINATION's table. The tokens are bearer secrets (redacted `Debug`); they
/// are held solely so `disconnect`/handoff can act on precisely these rows —
/// they are never enumerated, logged, or written out (design §1.4#3).
#[derive(Clone, Debug)]
pub struct ConnectionRecord {
    pub src: SessionId,
    pub dst: SessionId,
    pub tokens: Vec<(Op, EdgeToken)>,
}

impl ConnectionRecord {
    /// The [`ConnectionKind`] this record's op set spells, derived rather than
    /// stored so the record cannot drift from the rows it minted. `None` for an
    /// op set outside the closed kind vocabulary (unreachable through
    /// [`connect_in`], which mints solely from [`ConnectionKind::ops`]).
    // Read by the connect set-semantics check below; the menu/map stages (§2.3,
    // §5) render it — hence pub.
    #[must_use]
    pub fn kind(&self) -> Option<ConnectionKind> {
        let ops: Vec<Op> = self.tokens.iter().map(|(op, _)| *op).collect();
        [
            ConnectionKind::Pull,
            ConnectionKind::Push,
            ConnectionKind::Both,
        ]
        .into_iter()
        .find(|kind| {
            ops.len() == kind.ops().len()
                && kind.ops().iter().all(|op| ops.contains(op))
                && ops.iter().all(|op| kind.ops().contains(op))
        })
    }
}

/// The process's record of live connections — the `(src, dst)`-keyed map (one
/// record per direction; a peer pair `A⇆B` is two records) plus the §2.4 [v5]
/// FRESHNESS EPOCH. Shared between the UI thread (menu/drag acts), the
/// control thread (the §6 verbs), and the close-time sweep.
#[derive(Default)]
pub struct ConnectionTable {
    records: Mutex<HashMap<(SessionId, SessionId), ConnectionRecord>>,
    /// Monotonic revision, bumped by EVERY connection-authority change this
    /// table can observe (mint, revoke, close sweep — and, for the process
    /// singleton, the wire `grant`/`revoke` verbs via their repaint poke,
    /// which bypass the record store). The composed tab chrome joins this to
    /// its cache-epoch tuple so a menu/tooltip recomposes immediately after
    /// an authority act instead of serving the revoked state for up to the
    /// 30 s backstop ([`crate::session_chrome::CACHE_MAX_AGE_MS`]).
    revision: AtomicU64,
}

impl ConnectionTable {
    /// The record map, poison-recovered (the store holds no invariants a
    /// panicked holder could break — every entry is independently valid).
    pub(crate) fn records(
        &self,
    ) -> MutexGuard<'_, HashMap<(SessionId, SessionId), ConnectionRecord>> {
        self.records.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The current freshness epoch (§2.4). `Relaxed` on both sides: the value
    /// is a pure staleness hint — a reader that misses a just-landed bump
    /// recomposes one refresh later at worst, and the refresh funnel that
    /// FOLLOWS every bump (the `ConnectionsChanged` poke) already provides
    /// the happens-before for the facts themselves.
    pub(crate) fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    /// Bump the freshness epoch after an authority change.
    pub(crate) fn bump_revision(&self) {
        self.revision.fetch_add(1, Ordering::Relaxed);
    }
}

/// The shared handle to one [`ConnectionTable`].
pub type ConnectionStore = Arc<ConnectionTable>;

/// What a CONNECTED spawn's newborn is to its origin session (the §2.3 spawn
/// presets / the §6 `spawn connected=` argument). Direction only — the minted
/// connection is always [`ConnectionKind::Both`]:
///
/// * `Controlled` — the ORIGIN holds `both` over the newborn (origin drives).
/// * `Controller` — the NEWBORN holds `both` over the origin (newborn
///   supervises; its shell additionally receives `ATERM_OBSERVE_SESSION_ID`,
///   identity-only, `env_sanitize.rs`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ConnectedSpawnKind {
    Controlled,
    Controller,
}

/// Where a CONNECTED spawn places the newborn: a fresh window, or a tab beside
/// the origin. `Window` demands a GUI (`ERR headless`, design §1.4#7);
/// `Tab` is headless-legal (tabs are logical).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ConnectedSpawnPlace {
    Window,
    Tab,
}

/// The marker file (beside `aterm.toml`, the `kitty-log.toml` machine-owned
/// precedent) that latches the §1.4#8 FIRST-USE notice: present ⇒ the notice
/// has been shown once in this config lifetime.
const FIRST_USE_MARKER: &str = "connections-first-use";

/// Whether the §1.4#8 first-use notice should show NOW — true exactly once per
/// config lifetime, deciding and LATCHING atomically (`create_new`, so two
/// racing processes cannot both claim the first use). An absent config dir
/// (portable/sandboxed run) cannot latch; erring on SHOWING repeats a
/// disclosure, erring on skipping hides one — so an unlatchable state shows.
pub(crate) fn first_use_notice_should_show(config_path: Option<std::path::PathBuf>) -> bool {
    let Some(dir) = config_path.as_deref().and_then(std::path::Path::parent) else {
        return true;
    };
    // The config dir may not exist yet on a fresh install (the kitty-log
    // bootstrap precedent); best-effort — a failed create falls through to
    // `create_new`, whose error decides.
    let _ = std::fs::create_dir_all(dir);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(FIRST_USE_MARKER))
    {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
        // Unwritable dir: cannot latch — show (see above), and say why once.
        Err(e) => {
            aterm_log::warn!("connections first-use latch not persisted: {e}");
            true
        }
    }
}

/// The §1.4#8 / §2.4 first-use notice body: names the AUTHORITY the preset just
/// created (direction included — controlled vs controller invert who drives)
/// and the UNDO. Rendered through the transient notice card's
/// `"<marker> <title> — <detail>"` caption grammar (`notice.rs`).
pub(crate) fn first_use_notice_text(kind: ConnectedSpawnKind) -> String {
    let direction = match kind {
        ConnectedSpawnKind::Controlled => "this session can now type into and read the new one",
        ConnectedSpawnKind::Controller => "the new session can now type into and read this one",
    };
    format!(
        "\u{21c6} Session connection created \u{2014} {direction}, as at its keyboard; \
         Disconnect (tab menu, or the `disconnect` verb) undoes it"
    )
}

/// The §1.4#8 first-use notice body for a CONNECT confirmed from the card
/// (§3.3 drop popover / §2.5 sheet — same latch as the spawn presets, so the
/// disclosure shows exactly once whichever surface mints first). Direction is
/// named from the card's selection: which side can now act at whose keyboard.
pub(crate) fn first_use_connect_notice_text(src_drives: bool, dst_drives: bool) -> String {
    let direction = match (src_drives, dst_drives) {
        (true, true) => "each session can now act at the other's keyboard",
        (false, true) => "the peer session can now act at this one's keyboard",
        _ => "this session can now act at the peer's keyboard",
    };
    format!(
        "\u{21c6} Session connection created \u{2014} {direction}; \
         Disconnect (tab menu, or the `disconnect` verb) undoes it"
    )
}

/// What each direction of the pair `(a → b, b → a)` currently holds in the
/// LIVE edge tables — the configure card's prefill (design §2.5). Tables, not
/// the record store: a wire `grant` with no [`ConnectionRecord`] must prefill
/// honestly (the §4.1 mark rule). Ops fold to the closed kind vocabulary:
/// write-input/signal ⇒ the push half, read-screen ⇒ the pull half, both ⇒
/// `Both` — so a partial op set still spells the nearest kind rather than
/// hiding the connection.
pub(crate) fn pair_kinds(
    sessions: &Store,
    a: &SessionId,
    b: &SessionId,
) -> (Option<ConnectionKind>, Option<ConnectionKind>) {
    let mut fold = [(false, false), (false, false)]; // (pull, push) per direction
    for edge in all_edges(sessions) {
        let slot = if edge.src == *a && edge.dst == *b {
            0
        } else if edge.src == *b && edge.dst == *a {
            1
        } else {
            continue;
        };
        match edge.op {
            Op::ReadScreen => fold[slot].0 = true,
            Op::WriteInput | Op::Signal => fold[slot].1 = true,
            _ => {}
        }
    }
    let kind = |(pull, push)| match (pull, push) {
        (true, true) => Some(ConnectionKind::Both),
        (true, false) => Some(ConnectionKind::Pull),
        (false, true) => Some(ConnectionKind::Push),
        (false, false) => None,
    };
    (kind(fold[0]), kind(fold[1]))
}

/// The [`ConnectionKind`] a carried seamless-handoff op set spells, plus the
/// ops the re-mint must DROP (design §1.4#6 fail-soft). The rule is
/// NEVER-WIDEN: `pull` needs `read-screen`; `push` needs BOTH `write-input`
/// AND `signal` (a lone half — e.g. a wire `grant`ed `write-input` row — must
/// not be rounded up to the full human seat, because re-minting `Push` for it
/// would create a `signal` row the outgoing process never held). Everything
/// the chosen kind's op set does not cover is returned for the drop audit:
/// the lone push halves, the kinds a connection cannot spell (`config-write`
/// / `clipboard-write` / `derive-loop`, §1.4#2), and op strings this build
/// cannot parse. Dropping only loses authority; widening is unrepresentable.
pub(crate) fn carried_kind(ops: &[String]) -> (Option<ConnectionKind>, Vec<String>) {
    let has = |op: Op| ops.iter().any(|s| s == op.as_str());
    let pull = has(Op::ReadScreen);
    let push = has(Op::WriteInput) && has(Op::Signal);
    let kind = match (pull, push) {
        (true, true) => Some(ConnectionKind::Both),
        (true, false) => Some(ConnectionKind::Pull),
        (false, true) => Some(ConnectionKind::Push),
        (false, false) => None,
    };
    let covered: &[Op] = kind.map_or(&[], |k| k.ops());
    let dropped = ops
        .iter()
        .filter(|s| !covered.iter().any(|c| c.as_str() == s.as_str()))
        .cloned()
        .collect();
    (kind, dropped)
}

/// A fresh, empty connection store (tests run against a private one).
#[must_use]
pub fn new_connection_store() -> ConnectionStore {
    Arc::new(ConnectionTable::default())
}

/// The process-wide connection store: ONE per aterm process (the `PROXIES`
/// idiom, `proxy.rs` — a singleton avoids threading a handle through every
/// UI/control caller; correctness-wise a process has one connection fabric).
static CONNECTIONS: OnceLock<ConnectionStore> = OnceLock::new();

/// The process-wide [`ConnectionStore`] (lazily initialized, cloned Arc).
// The §6 `connect`/`disconnect` verbs and the menu/map stages resolve their
// records through this; in this stage only the close-time sweep reaches it.
#[must_use]
pub fn connections() -> ConnectionStore {
    CONNECTIONS.get_or_init(new_connection_store).clone()
}

/// Establish the connection `src → dst` of `kind`: mint through the ONE mint
/// path ([`EdgeTable::grant_connection`], design §1.4#2), record the minted
/// rows, and audit each (`session_edge`, §1.4#5). Returns `false` — nothing
/// minted, nothing recorded — for a self-loop (§1.5, fail closed).
///
/// DECLARATIVE set semantics (§2.5): the call means "the connection IS `kind`",
/// not "add edges". A same-kind re-connect is a no-op success (no token churn,
/// no audit noise); a different kind revokes the old rows and mints the new
/// ones under ONE hold of the destination's table lock — an atomic transition,
/// so no interleaved `decide_edge` ever sees excess authority (the old kind
/// gone AND the new kind live) or a spurious all-deny gap. Wire callers and
/// the UI sheet therefore produce identical transitions.
pub fn connect_in(
    conn: &ConnectionStore,
    src: &SessionId,
    dst: &SessionId,
    dst_edges: &Mutex<EdgeTable>,
    dst_nonce: &LaunchNonce,
    kind: ConnectionKind,
    origin: &str,
) -> bool {
    if src == dst {
        // Refused before any state is touched — `grant_connection` would also
        // refuse, but a self-loop must not even churn the record store.
        return false;
    }
    let key = (src.clone(), dst.clone());
    let mut records = conn.records();
    if records.get(&key).is_some_and(|r| r.kind() == Some(kind)) {
        // Set semantics: nothing changed, so the freshness epoch holds too.
        return true;
    }
    let old = records.remove(&key);
    // Revoke + mint under ONE table lock hold (the atomic transition).
    let (revoked, minted) = {
        let mut edges = dst_edges.lock().unwrap_or_else(|p| p.into_inner());
        let mut revoked = Vec::new();
        if let Some(old) = &old {
            for (op, tok) in &old.tokens {
                if edges.revoke(tok) {
                    revoked.push(*op);
                }
            }
        }
        // Non-empty: `kind.ops()` slices are non-empty and src != dst was
        // checked above (the only refusal `grant_connection` has).
        let minted = edges.grant_connection(src, dst, kind, dst_nonce);
        (revoked, minted)
    };
    records.insert(
        key,
        ConnectionRecord {
            src: src.clone(),
            dst: dst.clone(),
            tokens: minted.clone(),
        },
    );
    // Audit OFF both locks (the cmd_grant discipline: the logger sink has its
    // own mutex and must never nest inside the table lock).
    drop(records);
    conn.bump_revision();
    for op in revoked {
        session_edge_audit::emit(EdgeAction::Revoke, origin, src, dst, op.as_str());
    }
    for (op, _) in &minted {
        session_edge_audit::emit(EdgeAction::Grant, origin, src, dst, op.as_str());
    }
    true
}

/// Dissolve the connection `src → dst` in the process-wide store. See
/// [`disconnect_in`].
// Next stage: the §6 `disconnect` verb and the menu Disconnect land here.
#[allow(dead_code)]
pub fn disconnect(
    src: &SessionId,
    dst: &SessionId,
    dst_edges: &Mutex<EdgeTable>,
    origin: &str,
) -> bool {
    disconnect_in(&connections(), src, dst, dst_edges, origin)
}

/// Dissolve the connection `src → dst`: drop its record and revoke EXACTLY the
/// rows it minted (per-token [`EdgeTable::revoke`] — pair-precise, so a second
/// `src → dst'` connection from the same source is untouched, which a bare
/// `revoke_src` sweep could not promise). `false` — and no state touched — when
/// no such record exists (fail closed on an unknown pair). Rows already gone
/// (e.g. a wire `revoke src=` raced this) are not an error: the record removal
/// is the act, and only rows actually revoked here are audited.
pub fn disconnect_in(
    conn: &ConnectionStore,
    src: &SessionId,
    dst: &SessionId,
    dst_edges: &Mutex<EdgeTable>,
    origin: &str,
) -> bool {
    let removed = conn.records().remove(&(src.clone(), dst.clone()));
    let Some(rec) = removed else {
        return false;
    };
    let revoked: Vec<Op> = {
        let mut edges = dst_edges.lock().unwrap_or_else(|p| p.into_inner());
        rec.tokens
            .iter()
            .filter(|(_, tok)| edges.revoke(tok))
            .map(|(op, _)| *op)
            .collect()
    };
    conn.bump_revision();
    for op in revoked {
        session_edge_audit::emit(EdgeAction::Revoke, origin, &rec.src, &rec.dst, op.as_str());
    }
    true
}

/// Dissolve the connection `src → dst` KIND-FILTERED (the §6 `disconnect …
/// kind=` verb; `None` = the whole connection). Returns `Some(revoked-count)`
/// when the pair was known (a record, or unrecorded rows actually swept),
/// `None` — nothing touched — otherwise (fail closed on an unknown pair).
///
/// Two mechanisms, reconciling record-precision with the §1.4#4 [v5] op filter:
///
/// * A RECORDED connection is dissolved per held token (the [`disconnect_in`]
///   pair-precise discipline), restricted to the filtered kind's ops; the
///   record keeps its surviving ops (a `Both` record minus `kind=pull` remains
///   a live `Push` record) or drops when none survive.
/// * WIRE-GRANTED rows with NO record (a `grant` mints no [`ConnectionRecord`])
///   fall back to the op-filtered source sweep
///   ([`EdgeTable::revoke_src_ops`]) — token hexes are non-enumerable, so the
///   sweep is the only dissolution path for them.
///
/// The record guard is held across the table op (the [`connect_in`] lock
/// discipline: conn-records mutex nests OUTSIDE the one dst table mutex);
/// audit runs off both locks.
pub fn disconnect_kind_in(
    conn: &ConnectionStore,
    src: &SessionId,
    dst: &SessionId,
    dst_edges: &Mutex<EdgeTable>,
    kind: Option<ConnectionKind>,
    origin: &str,
) -> Option<usize> {
    let key = (src.clone(), dst.clone());
    let filter = kind.map(|k| k.ops());
    let mut records = conn.records();
    if let Some(rec) = records.remove(&key) {
        // Pair-precise: revoke exactly the recorded tokens matching the filter.
        let (revoke_toks, keep_toks): (Vec<_>, Vec<_>) = rec
            .tokens
            .into_iter()
            .partition(|(op, _)| filter.is_none_or(|ops| ops.contains(op)));
        let revoked: Vec<Op> = {
            let mut edges = dst_edges.lock().unwrap_or_else(|p| p.into_inner());
            revoke_toks
                .iter()
                .filter(|(_, tok)| edges.revoke(tok))
                .map(|(op, _)| *op)
                .collect()
        };
        if !keep_toks.is_empty() {
            records.insert(
                key,
                ConnectionRecord {
                    src: src.clone(),
                    dst: dst.clone(),
                    tokens: keep_toks,
                },
            );
        }
        drop(records);
        // The record changed shape (removed or shrunk) even when zero rows
        // were live to revoke — the menu's facts may have, too.
        conn.bump_revision();
        let n = revoked.len();
        for op in revoked {
            session_edge_audit::emit(EdgeAction::Revoke, origin, src, dst, op.as_str());
        }
        return Some(n);
    }
    // No record: the op-filtered sweep fallback. One sweep (and one audit
    // event) per filtered op so the audit names what was actually removed;
    // the unfiltered form is one `op=*` event (the `revoke src=` convention).
    let removed: Vec<(&'static str, usize)> = match filter {
        None => {
            let n = {
                let mut edges = dst_edges.lock().unwrap_or_else(|p| p.into_inner());
                edges.revoke_src(src)
            };
            vec![("*", n)]
        }
        Some(ops) => {
            let mut edges = dst_edges.lock().unwrap_or_else(|p| p.into_inner());
            ops.iter()
                .map(|op| (op.as_str(), edges.revoke_src_ops(src, Some(&[*op]))))
                .collect()
        }
    };
    drop(records);
    let total: usize = removed.iter().map(|(_, n)| n).sum();
    if total == 0 {
        return None;
    }
    conn.bump_revision();
    for (op, n) in removed {
        if n > 0 {
            session_edge_audit::emit(EdgeAction::RevokeSrc, origin, src, dst, op);
        }
    }
    Some(total)
}

/// Every live edge row across EVERY registered session's table — the §6 `flows`
/// aggregation (Owner-gated at the dispatch; this is a pure collector). Takes
/// the ALREADY-RELEASED registry snapshot, then locks each table briefly (the
/// [`roles_in`] discipline: no store lock held across a table lock). A row
/// lives only in its destination's table, so the concatenation has no
/// duplicates; sorted by `(src, dst, op)` for a stable listing.
pub(crate) fn all_edges(sessions: &Store) -> Vec<aterm_session::Edge> {
    let handles = sessions
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .snapshot();
    let mut out: Vec<aterm_session::Edge> = Vec::new();
    for h in &handles {
        let rows = {
            let edges = h.ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
            edges.edges()
        };
        out.extend(rows);
    }
    out.sort_by(|a, b| {
        (a.src.as_str(), a.dst.as_str(), a.op.as_str()).cmp(&(
            b.src.as_str(),
            b.dst.as_str(),
            b.op.as_str(),
        ))
    });
    out
}

/// Distinct directed non-self-loop `(src → dst)` pairs across the live edge
/// tables — the ❯ status item's connections count (design §5.1). The SAME
/// [`all_edges`] fold the §5 map draws its arrows from (self-loops spell
/// nothing, §1.5), so the menu's number and the map it opens can never
/// disagree; tables not the record store, per the §4.1 honesty rule (a wire
/// `grant` with no [`ConnectionRecord`] still counts).
pub(crate) fn connection_count(sessions: &Store) -> usize {
    let mut pairs: HashSet<(SessionId, SessionId)> = HashSet::new();
    for edge in all_edges(sessions) {
        if edge.src != edge.dst {
            pairs.insert((edge.src, edge.dst));
        }
    }
    pairs.len()
}

/// One session's connection roles, for the mark (design §4): `outbound` ▲ = it
/// holds authority INTO some other session, `inbound` ▽ = some other session
/// holds authority over it, both = ⧗.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct SessionRoles {
    pub inbound: bool,
    pub outbound: bool,
}

/// The §4.1 role predicate over the live edge tables — and ONLY the tables
/// (never lineage, never the record store: the tables are the authority record,
/// so a wire `grant` with no `ConnectionRecord` still marks honestly). Per
/// non-self-loop row `(src, dst, op)`: `dst` is inbound, `src` is outbound —
/// a row lives in its destination's table by construction, so a non-self-loop
/// src's appearance IS "src in any OTHER session's table". Self-loop rows spell
/// no role (§1.5). NO liveness qualifier: a row whose src is closed-but-unswept
/// still marks (the mark reports recorded authority, not traffic), which is
/// also what makes recompute cheap enough for the refresh funnel — one pass
/// over the rows, no store cross-checks.
///
/// Every table OWNER gets an entry (so callers can render "no mark" without a
/// missing-key case); row endpoints outside the owner set are added as found.
// Next stage: the marks/menu recompute (§4) reads this predicate (through
// [`roles_in`] on the live registry); tests drive it on synthetic tables.
#[allow(dead_code)]
pub fn roles<'a>(
    tables: impl IntoIterator<Item = (&'a SessionId, &'a EdgeTable)>,
) -> HashMap<SessionId, SessionRoles> {
    let mut out: HashMap<SessionId, SessionRoles> = HashMap::new();
    for (owner, table) in tables {
        out.entry(owner.clone()).or_default();
        fold_roles(&mut out, table.edges());
    }
    out
}

/// Fold one table's rows into the role map — the §4.1 predicate per row,
/// shared by [`roles`] and [`roles_in`] so the two can never drift.
fn fold_roles(out: &mut HashMap<SessionId, SessionRoles>, rows: Vec<aterm_session::Edge>) {
    for edge in rows {
        if edge.src == edge.dst {
            continue;
        }
        out.entry(edge.dst).or_default().inbound = true;
        out.entry(edge.src).or_default().outbound = true;
    }
}

/// The §4.1 fold over a registry snapshot: every owner an entry, every table's
/// rows folded. Takes the ALREADY-RELEASED snapshot so no store lock is held
/// while the per-session table locks are taken briefly (the registry's
/// clone-then-release discipline, `session_store.rs`).
fn fold_registry(
    handles: &[crate::session_store::SessionHandle],
) -> HashMap<SessionId, SessionRoles> {
    let mut out: HashMap<SessionId, SessionRoles> = HashMap::new();
    for h in handles {
        let rows = {
            let edges = h.ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
            edges.edges()
        };
        out.entry(h.sid.clone()).or_default();
        fold_roles(&mut out, rows);
    }
    out
}

/// [`roles`] over every registered session's table — the refresh-funnel entry
/// point.
// The §5 connection map reads this sid-keyed form; the tab-mark recompute
// consumes [`roles_by_local`] below.
#[allow(dead_code)]
pub fn roles_in(sessions: &Store) -> HashMap<SessionId, SessionRoles> {
    let handles = sessions
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .snapshot();
    fold_registry(&handles)
}

/// [`roles_in`] re-keyed by the registry's LOCAL id — the id the tab model
/// addresses sessions by — from ONE snapshot. Foreign sids (a wire-granted src
/// no local session owns) mark no tab and drop out of the re-key; the tables
/// they appear in still spell their owners' inbound role.
pub fn roles_by_local(sessions: &Store) -> HashMap<u64, SessionRoles> {
    let handles = sessions
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .snapshot();
    let by_sid = fold_registry(&handles);
    handles
        .iter()
        .filter_map(|h| by_sid.get(&h.sid).map(|roles| (h.local_id, *roles)))
        .collect()
}

/// The close-time sweep (design §1.4#4) — a SECURITY OBLIGATION, not
/// bookkeeping: each edge row's nonce is the DESTINATION's, so a source's death
/// does not fail closed by nonce, and token hexes are non-enumerable, so this
/// sweep is the only dissolution path for the dead source's authority. Drops
/// every [`ConnectionRecord`] touching the closed session (either endpoint —
/// rows TOWARD it died with its own table), then `revoke_src(closing)` across
/// every surviving session's table, auditing one `revoke_src` event per table
/// actually swept (the wire `revoke src=` twin, origin `close`).
///
/// Called from `App::retire_session_registration` (every mid-run close path
/// funnels there) with the App's own store handle — the process singleton in
/// a real run; the final app-exit path needs no sweep, the whole process dies
/// with every table in it. Call AFTER `deregister_local`, with no store lock
/// held: this re-reads the registry for the survivors' tables (snapshot, then
/// lock each table with the store guard dropped — the registry's
/// clone-then-release discipline). Idempotent: a second sweep finds nothing
/// and emits nothing (though each still bumps the freshness epoch).
pub fn sweep_session_closed_in(conn: &ConnectionStore, closing: &SessionId, sessions: &Store) {
    conn.records()
        .retain(|(src, dst), _| src != closing && dst != closing);
    // Bump UNCONDITIONALLY (not just when a survivor table is swept): the
    // closing session's own table — and every row TOWARD it — died with its
    // deregistration, with no `revoke` call to observe. The peers' menus
    // listing those rows must still recompose (§2.4).
    conn.bump_revision();
    let survivors: Vec<(SessionId, Arc<crate::SessionCtx>)> = sessions
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .snapshot()
        .into_iter()
        .map(|h| (h.sid, h.ctx))
        .collect();
    for (sid, ctx) in survivors {
        if sid == *closing {
            // A caller that swept before deregistering (or a duplicate-close
            // race) must not "sweep" the dying session's own table.
            continue;
        }
        let removed = {
            let mut edges = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
            edges.revoke_src(closing)
        };
        if removed > 0 {
            // Audit OFF the table lock; `op=*` — the sweep removes every op
            // the source held (the `revoke src=` wire-form convention).
            session_edge_audit::emit(EdgeAction::RevokeSrc, "close", closing, &sid, "*");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_session::sink::SinkWriter;
    use aterm_session::{EdgeDecision, decide_edge};

    use crate::session_store::{SessionHandle, SessionState};

    fn ids() -> (SessionId, SessionId, LaunchNonce) {
        (
            SessionId::new("s-a"),
            SessionId::new("s-b"),
            LaunchNonce::from_bytes([7u8; 16]),
        )
    }

    /// A minimal registrable handle (the `session_store` test idiom): dead fds,
    /// fresh identity, empty edge table.
    fn handle(local_id: u64) -> SessionHandle {
        let sid = SessionId::generate();
        let nonce = LaunchNonce::generate();
        let ctx = Arc::new(crate::SessionCtx {
            sink: Arc::new(SinkWriter::new(-1)),
            edges: Mutex::new(EdgeTable::new()),
            self_id: sid.clone(),
            nonce,
            turn_lease: Mutex::new(None),
            cast: Arc::new(Mutex::new(crate::cast::CastRecorder::new(80, 24))),
            temporal: Arc::new(Mutex::new(crate::temporal::TemporalRecorder::new())),
            byte_fanout: Arc::new(crate::cast::ByteFanout::new()),
            turns: Arc::new(Mutex::new(crate::turn_ledger::TurnLedger::default())),
            meta: Mutex::new(crate::session_timeline::SessionMeta::default()),
            app_kitty: Mutex::new(crate::app_kitty::AppKittySlot::default()),
            timeline: Arc::new(Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
            fabric: crate::fabric::SessionFabric::default(),
        });
        SessionHandle {
            sid,
            nonce,
            local_id,
            parent: None,
            state: SessionState::Alive,
            title: format!("tab-{local_id}"),
            term: Arc::new(Mutex::new(aterm_core::terminal::Terminal::new(24, 80))),
            master: -1,
            ctx,
        }
    }

    /// §1.4#6 fail-soft grouping: a carried op set re-mints the LARGEST kind
    /// it fully proves and drops the rest — never widening. Every transition
    /// the vocabulary allows, plus the halves and strangers it must refuse.
    #[test]
    fn carried_kind_never_widens_and_drops_the_rest() {
        let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // The three whole kinds map back exactly, nothing dropped.
        assert_eq!(
            carried_kind(&s(&["read-screen"])),
            (Some(ConnectionKind::Pull), vec![])
        );
        assert_eq!(
            carried_kind(&s(&["signal", "write-input"])),
            (Some(ConnectionKind::Push), vec![])
        );
        assert_eq!(
            carried_kind(&s(&["read-screen", "signal", "write-input"])),
            (Some(ConnectionKind::Both), vec![])
        );
        // A lone push HALF is never rounded up to the full human seat.
        assert_eq!(
            carried_kind(&s(&["write-input"])),
            (None, s(&["write-input"]))
        );
        assert_eq!(
            carried_kind(&s(&["read-screen", "signal"])),
            (Some(ConnectionKind::Pull), s(&["signal"]))
        );
        // Ops a connection cannot spell (§1.4#2) and unknown strings drop.
        assert_eq!(
            carried_kind(&s(&["config-write", "read-screen", "not-an-op"])),
            (
                Some(ConnectionKind::Pull),
                s(&["config-write", "not-an-op"])
            )
        );
        assert_eq!(carried_kind(&[]), (None, vec![]));
    }

    #[test]
    fn connect_disconnect_round_trip_on_a_synthetic_table() {
        let conn = new_connection_store();
        let (a, b, nonce) = ids();
        let dst_edges = Mutex::new(EdgeTable::new());

        assert!(connect_in(
            &conn,
            &a,
            &b,
            &dst_edges,
            &nonce,
            ConnectionKind::Push,
            "test"
        ));
        let minted = {
            let records = conn.records();
            let rec = records
                .get(&(a.clone(), b.clone()))
                .expect("connect records the pair");
            assert_eq!(rec.kind(), Some(ConnectionKind::Push));
            rec.tokens.clone()
        };
        // Push minted exactly the human seat: input + signal, both live.
        assert_eq!(minted.len(), 2);
        for (op, tok) in &minted {
            assert!(decide_edge(&dst_edges.lock().unwrap(), tok, &b, *op, &nonce).is_permitted());
        }

        // Disconnect revokes exactly those rows and drops the record.
        assert!(disconnect_in(&conn, &a, &b, &dst_edges, "test"));
        assert!(conn.records().is_empty(), "record gone");
        assert!(dst_edges.lock().unwrap().is_empty(), "rows gone");
        for (op, tok) in &minted {
            assert_eq!(
                decide_edge(&dst_edges.lock().unwrap(), tok, &b, *op, &nonce),
                EdgeDecision::Deny
            );
        }
        // A second disconnect of the now-unknown pair fails closed.
        assert!(!disconnect_in(&conn, &a, &b, &dst_edges, "test"));
    }

    #[test]
    fn connect_refuses_a_self_loop_with_no_residue() {
        let conn = new_connection_store();
        let (a, _b, nonce) = ids();
        let dst_edges = Mutex::new(EdgeTable::new());
        for kind in [
            ConnectionKind::Pull,
            ConnectionKind::Push,
            ConnectionKind::Both,
        ] {
            assert!(
                !connect_in(&conn, &a, &a, &dst_edges, &nonce, kind, "test"),
                "{kind:?} self-loop must be refused"
            );
        }
        assert!(conn.records().is_empty(), "no record behind a refusal");
        assert!(
            dst_edges.lock().unwrap().is_empty(),
            "no row behind a refusal"
        );
    }

    #[test]
    fn reconnect_is_idempotent_on_same_kind_and_atomic_on_rekind() {
        let conn = new_connection_store();
        let (a, b, nonce) = ids();
        let dst_edges = Mutex::new(EdgeTable::new());

        assert!(connect_in(
            &conn,
            &a,
            &b,
            &dst_edges,
            &nonce,
            ConnectionKind::Pull,
            "test"
        ));
        let (pull_op, pull_tok) = conn.records()[&(a.clone(), b.clone())].tokens[0];

        // Same kind again: success with NO token churn (set semantics — the
        // state already holds, so the original token stays live).
        assert!(connect_in(
            &conn,
            &a,
            &b,
            &dst_edges,
            &nonce,
            ConnectionKind::Pull,
            "test"
        ));
        assert_eq!(dst_edges.lock().unwrap().len(), 1);
        assert!(
            decide_edge(&dst_edges.lock().unwrap(), &pull_tok, &b, pull_op, &nonce).is_permitted(),
            "idempotent re-connect must not rotate the token"
        );

        // A different kind is ONE atomic transition: the old rows are gone, the
        // new kind's rows (and only those) are live, one record remains.
        assert!(connect_in(
            &conn,
            &a,
            &b,
            &dst_edges,
            &nonce,
            ConnectionKind::Both,
            "test"
        ));
        assert_eq!(
            decide_edge(&dst_edges.lock().unwrap(), &pull_tok, &b, pull_op, &nonce),
            EdgeDecision::Deny,
            "the replaced kind's token must die in the transition"
        );
        let records = conn.records();
        assert_eq!(records.len(), 1);
        let rec = &records[&(a.clone(), b.clone())];
        assert_eq!(rec.kind(), Some(ConnectionKind::Both));
        assert_eq!(dst_edges.lock().unwrap().len(), 3);
        for (op, tok) in &rec.tokens {
            assert!(decide_edge(&dst_edges.lock().unwrap(), tok, &b, *op, &nonce).is_permitted());
        }
    }

    #[test]
    fn disconnect_kind_filters_a_recorded_connection_pair_precisely() {
        let conn = new_connection_store();
        let (a, b, nonce) = ids();
        let dst_edges = Mutex::new(EdgeTable::new());
        assert!(connect_in(
            &conn,
            &a,
            &b,
            &dst_edges,
            &nonce,
            ConnectionKind::Both,
            "test"
        ));

        // kind=pull revokes ONLY the read row; the record survives as Push.
        assert_eq!(
            disconnect_kind_in(
                &conn,
                &a,
                &b,
                &dst_edges,
                Some(ConnectionKind::Pull),
                "test"
            ),
            Some(1)
        );
        let ops: Vec<Op> = dst_edges
            .lock()
            .unwrap()
            .edges()
            .iter()
            .map(|e| e.op)
            .collect();
        assert_eq!(ops.len(), 2);
        assert!(!ops.contains(&Op::ReadScreen), "pull half gone: {ops:?}");
        assert!(ops.contains(&Op::WriteInput) && ops.contains(&Op::Signal));
        assert_eq!(
            conn.records()[&(a.clone(), b.clone())].kind(),
            Some(ConnectionKind::Push),
            "the record keeps its surviving ops"
        );

        // Filtering the already-gone half is a known-pair no-op (Some(0)).
        assert_eq!(
            disconnect_kind_in(
                &conn,
                &a,
                &b,
                &dst_edges,
                Some(ConnectionKind::Pull),
                "test"
            ),
            Some(0)
        );

        // The unfiltered form dissolves the remainder and drops the record.
        assert_eq!(
            disconnect_kind_in(&conn, &a, &b, &dst_edges, None, "test"),
            Some(2)
        );
        assert!(conn.records().is_empty());
        assert!(dst_edges.lock().unwrap().is_empty());
        // An unknown pair now fails closed.
        assert_eq!(
            disconnect_kind_in(&conn, &a, &b, &dst_edges, None, "test"),
            None
        );
    }

    #[test]
    fn disconnect_kind_sweeps_unrecorded_wire_grants_op_filtered() {
        let conn = new_connection_store();
        let (a, b, nonce) = ids();
        let dst_edges = Mutex::new(EdgeTable::new());
        // Wire `grant` rows: minted straight into the table, NO record.
        {
            let mut edges = dst_edges.lock().unwrap();
            let _ = edges.grant(a.clone(), b.clone(), Op::ReadScreen, nonce);
            let _ = edges.grant(a.clone(), b.clone(), Op::WriteInput, nonce);
            let _ = edges.grant(a.clone(), b.clone(), Op::Signal, nonce);
        }

        // The push half sweeps by (src, op); pull survives.
        assert_eq!(
            disconnect_kind_in(
                &conn,
                &a,
                &b,
                &dst_edges,
                Some(ConnectionKind::Push),
                "test"
            ),
            Some(2)
        );
        let ops: Vec<Op> = dst_edges
            .lock()
            .unwrap()
            .edges()
            .iter()
            .map(|e| e.op)
            .collect();
        assert_eq!(ops, vec![Op::ReadScreen], "only pull remains: {ops:?}");

        // Unfiltered sweeps the rest; a second call fails closed (None).
        assert_eq!(
            disconnect_kind_in(&conn, &a, &b, &dst_edges, None, "test"),
            Some(1)
        );
        assert_eq!(
            disconnect_kind_in(&conn, &a, &b, &dst_edges, None, "test"),
            None
        );
    }

    #[test]
    fn roles_computes_the_mark_predicate_per_scenario() {
        let (a, b, nonce) = ids();

        // No connections: every owner present, no roles.
        let (ta, tb) = (EdgeTable::new(), EdgeTable::new());
        let r = roles([(&a, &ta), (&b, &tb)]);
        assert_eq!(r.len(), 2);
        assert_eq!(r[&a], SessionRoles::default());
        assert_eq!(r[&b], SessionRoles::default());

        // One-way A → B (rows live in B's — the destination's — table).
        let ta = EdgeTable::new();
        let mut tb = EdgeTable::new();
        let _ = tb.grant_connection(&a, &b, ConnectionKind::Pull, &nonce);
        let r = roles([(&a, &ta), (&b, &tb)]);
        assert_eq!(
            r[&a],
            SessionRoles {
                inbound: false,
                outbound: true
            }
        );
        assert_eq!(
            r[&b],
            SessionRoles {
                inbound: true,
                outbound: false
            }
        );

        // Peer pair A ⇆ B: two independent rows, both sessions both roles.
        let mut ta = EdgeTable::new();
        let mut tb = EdgeTable::new();
        let _ = tb.grant_connection(&a, &b, ConnectionKind::Push, &nonce);
        let _ = ta.grant_connection(&b, &a, ConnectionKind::Push, &nonce);
        let r = roles([(&a, &ta), (&b, &tb)]);
        for sid in [&a, &b] {
            assert_eq!(
                r[sid],
                SessionRoles {
                    inbound: true,
                    outbound: true
                }
            );
        }

        // Self-loop-only: the row (A, A, op) spells NO role (§1.5).
        let mut ta = EdgeTable::new();
        let _ = ta.grant(a.clone(), a.clone(), Op::WriteInput, nonce);
        let r = roles([(&a, &ta)]);
        assert_eq!(r[&a], SessionRoles::default());

        // Foreign src (a wire-granted src that is no registered table owner):
        // the owner marks inbound ONLY, and the foreign src still reports
        // outbound — no liveness qualifier.
        let x = SessionId::new("s-foreign");
        let mut tb = EdgeTable::new();
        let _ = tb.grant(x.clone(), b.clone(), Op::WriteInput, nonce);
        let r = roles([(&b, &tb)]);
        assert_eq!(
            r[&b],
            SessionRoles {
                inbound: true,
                outbound: false
            }
        );
        assert_eq!(
            r[&x],
            SessionRoles {
                inbound: false,
                outbound: true
            }
        );
    }

    #[test]
    fn first_use_notice_latches_once_per_config_lifetime() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-test-conn-first-use-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cfg = Some(dir.join("aterm.toml"));
        // First UI connect: shows AND latches (marker file appears)…
        assert!(first_use_notice_should_show(cfg.clone()));
        assert!(dir.join(FIRST_USE_MARKER).exists());
        // …every later connect in this config lifetime is quiet.
        assert!(!first_use_notice_should_show(cfg.clone()));
        assert!(!first_use_notice_should_show(cfg));
        let _ = std::fs::remove_dir_all(&dir);
        // No config dir: cannot latch — a disclosure is never silently skipped.
        assert!(first_use_notice_should_show(None));
    }

    #[test]
    fn first_use_notice_names_the_authority_direction_and_the_undo() {
        let controlled = first_use_notice_text(ConnectedSpawnKind::Controlled);
        let controller = first_use_notice_text(ConnectedSpawnKind::Controller);
        for text in [&controlled, &controller] {
            assert!(text.contains("Session connection"), "{text}");
            assert!(text.contains("Disconnect"), "undo named: {text}");
        }
        // The two presets INVERT who drives; the notice must not blur that.
        assert_ne!(controlled, controller);
        assert!(
            controlled.contains("this session can now type into"),
            "{controlled}"
        );
        assert!(
            controller.contains("the new session can now type into"),
            "{controller}"
        );
    }

    #[test]
    fn close_time_sweep_dissolves_records_and_survivor_rows() {
        let conn = new_connection_store();
        let store = crate::session_store::new_store();
        let (ha, hb) = (handle(1), handle(2));
        let (a, b) = (ha.sid.clone(), hb.sid.clone());
        let (actx, bctx) = (ha.ctx.clone(), hb.ctx.clone());
        {
            let mut s = store.write().unwrap();
            s.register(ha);
            s.register(hb);
        }

        // A pushes into B (rows in B's table) AND B pulls A (rows in A's
        // table) — so closing A exercises both record directions at once.
        assert!(connect_in(
            &conn,
            &a,
            &b,
            &bctx.edges,
            &bctx.nonce,
            ConnectionKind::Push,
            "test"
        ));
        assert!(connect_in(
            &conn,
            &b,
            &a,
            &actx.edges,
            &actx.nonce,
            ConnectionKind::Pull,
            "test"
        ));
        let push_tokens = conn.records()[&(a.clone(), b.clone())].tokens.clone();

        // Close A: deregister FIRST, then sweep (the
        // `retire_session_registration` order — the sweep re-reads the
        // registry and must see only the survivors).
        assert_eq!(store.write().unwrap().deregister_local(1), Some(a.clone()));
        sweep_session_closed_in(&conn, &a, &store);

        // Every record touching A is gone — A as src AND A as dst.
        assert!(conn.records().is_empty());
        // The surviving table holds none of A's rows; the minted tokens deny.
        assert!(bctx.edges.lock().unwrap().is_empty());
        for (op, tok) in &push_tokens {
            assert_eq!(
                decide_edge(&bctx.edges.lock().unwrap(), tok, &b, *op, &bctx.nonce),
                EdgeDecision::Deny
            );
        }
        // The survivor's mark clears with the sweep; the closed sid is gone.
        let r = roles_in(&store);
        assert_eq!(r[&b], SessionRoles::default());
        assert!(!r.contains_key(&a));
        // Idempotent: a duplicate-close sweep finds nothing to do.
        sweep_session_closed_in(&conn, &a, &store);
        assert!(conn.records().is_empty());
    }
}
