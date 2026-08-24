// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session-graph + capability-authority verbs: `sessions`, `family`, `ready`,
//! `cast`, `edges`/`grants`, `grant`, `revoke`, `whoami`. Moved verbatim from
//! `control.rs` (behavior-preserving). The `Scope` authority enum + the
//! proxy-forward / cross-session auth cluster stay in `control.rs`; this module
//! reaches `Scope` and the shared JSON helpers via `super::`.

use std::sync::{Arc, Mutex};

use aterm_core::terminal::Terminal;
use aterm_session::{ConnectionKind, EdgeToken, Op, SessionId};
use winit::event_loop::EventLoopProxy;

use super::{Scope, json_ok, json_str_field, pct_encode};
use crate::Wake;
use crate::session_edge_audit::{self, EdgeAction};
use crate::session_store::Store;
use crate::session_timeline::{
    MetaEdit, MetaField, MetaWriteError, apply_meta_value, write_session_meta,
};
use crate::{SessionCtx, term_lock};

/// `sessions` -> list the process-wide registry: `OK <n>\n` then one line per
/// session, sorted by local id: `<local> <sid> <parent|-> <state> <title> meta=<1|0>`.
/// On a single-session window this is exactly one line == the lone session (the
/// zero-regression base case). The store snapshot is cloned out before formatting,
/// so this never holds the registry lock across a `Terminal` lock.
///
/// `meta=<1|0>` (session-metadata stage 1) is a TRAILING additive token: `1` iff
/// any USER metadata (`meta set title|description|icon|role|attention`) is set, so a fleet
/// driver knows which sessions to `@<sid> meta` without N round-trips. Safe to
/// append: the title token before it is pct-encoded (never contains a space) and
/// the one shipping parser (aterm-ctl `ls`) prints the line verbatim, keying only
/// on the sid field — verified tolerant of trailing tokens.
pub(crate) fn cmd_sessions(_self_ctx: &SessionCtx, store: &Store) -> String {
    cmd_sessions_store(store)
}

/// Context-free fleet projection used by Owner meta dispatch when no terminal
/// exists. The wire bytes are identical to [`cmd_sessions`].
pub(crate) fn cmd_sessions_store(store: &Store) -> String {
    let snapshot = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.snapshot()
    };
    let mut out = format!("OK {}\n", snapshot.len());
    for h in &snapshot {
        let parent = h
            .parent
            .as_ref()
            .map_or("-", aterm_session::SessionId::as_str);
        let title = pct_encode(&h.title);
        let has_meta = u8::from(
            h.ctx
                .meta
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .any_set(),
        );
        out.push_str(&format!(
            "{} {} {} {} {} meta={has_meta}\n",
            h.local_id,
            h.sid.as_str(),
            parent,
            h.state.as_str(),
            title,
        ));
    }
    out
}

/// `who` -> the PRESENCE readout for the whole instance: one line per session
/// naming who is DRIVING it and how many peers are WATCHING it, so a human or an
/// orchestrator can always see the hand and the eye on a fleet. Framed `OK <n>`
/// then one `<local> <sid> driving=<turn-id|-> watchers=<n> turns=<n> <state>`
/// line per session (sorted by local id, like `sessions`). `driving` is the
/// session's live TURN LEASE (`Some(id)` while a `turn` is mid-flight, `-` when
/// idle — the one-driver-per-session mutex); `watchers` is the live `subscribe`
/// count on that session; `turns` is its ledger depth. Owner-only + self-scoped
/// like `sessions` (a fleet-wide readout, not a per-target query). Read-side
/// (pure observation of lease + subscriber + ledger state).
///
/// NOTE: `watchers` counts every live `subscribe` registration, which includes a
/// session's OWN in-flight `turn` (it registers a settle-watcher while running),
/// so a session being driven reads `driving=<id> watchers>=1`. That is honest —
/// the driver is watching for the reply — not double-counting an external peer.
pub(crate) fn cmd_who(store: &Store, subscribers: &crate::subscribe::Subscribers) -> String {
    let snapshot = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.snapshot()
    };
    let subs = subscribers.lock().unwrap_or_else(|p| p.into_inner());
    let mut out = format!("OK {}\n", snapshot.len());
    for h in &snapshot {
        let driving = h
            .ctx
            .turn_lease
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|l| l.driving_token(crate::metrics::now_us()))
            .unwrap_or_else(|| "-".to_string());
        let watchers = subs.watchers(h.local_id);
        let turns = h.ctx.turns.lock().unwrap_or_else(|p| p.into_inner()).len();
        out.push_str(&format!(
            "{} {} driving={driving} watchers={watchers} turns={turns} {}\n",
            h.local_id,
            h.sid.as_str(),
            h.state.as_str(),
        ));
    }
    out
}

/// Process-wide auto-holder tag mint for a `lease acquire` with no explicit
/// `holder=`: a unique `drv-<n>` so an unnamed driver still gets a distinct token
/// to renew/release with.
static NEXT_LEASE_TAG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Default / max cooperative-lease TTL (ms). Short by default so a crashed driver
/// frees the coordination slot quickly; capped so a hold cannot wedge it for long.
const LEASE_TTL_DEFAULT_MS: u64 = 30_000;
const LEASE_TTL_MAX_MS: u64 = 600_000;

/// `lease [status] | lease acquire [ttl=<ms>] [holder=<name>] | lease release
/// [holder=<name>] [force]` — the explicit COOPERATIVE drive lease for raw
/// (non-`turn`) drivers. One holder at a time, TTL-expiring, surfaced in `who` as
/// `driving=lease:<holder>`. It is mutually exclusive with any live lease (a
/// different holder's `acquire`, and a `turn`, are refused while it is held) and
/// self-expiring, but it does NOT hard-block raw `send`/`key`/`feed` — those stay
/// governed by the `turn` lease. It is the coordination signal cooperating agents
/// check before driving; `turn` remains the HARD arbitration primitive.
pub(crate) fn cmd_lease(ctx: &SessionCtx, rest: &str) -> String {
    let mut toks = rest.split_whitespace();
    match toks.next().unwrap_or("status") {
        "status" => lease_status(ctx),
        "acquire" => lease_acquire(ctx, toks),
        "release" => lease_release(ctx, toks),
        other => {
            format!("ERR lease: unknown subcommand '{other}' (status | acquire | release)\n")
        }
    }
}

/// `lease status` (or bare `lease`): report the session's current lease — a live
/// `turn` (`turn=<id>`), a live cooperative hold (`holder=<h> expires_in_ms=<n>`),
/// or `none` (idle, incl. a lapsed cooperative lease).
fn lease_status(ctx: &SessionCtx) -> String {
    let now = crate::metrics::now_us();
    let lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
    match lease.as_ref() {
        Some(crate::Lease::Turn(id)) => format!("OK lease turn={id}\n"),
        Some(crate::Lease::Drive { holder, expires_us }) if *expires_us > now => {
            format!(
                "OK lease holder={holder} expires_in_ms={}\n",
                (*expires_us - now) / 1000
            )
        }
        _ => "OK lease none\n".to_string(),
    }
}

/// `lease acquire [ttl=<ms>] [holder=<name>]`: take (or, for the same holder,
/// renew) the cooperative lease. Refuses a live `turn` or a DIFFERENT holder's
/// live lease; steals a lapsed one. Returns the granted `holder` so an unnamed
/// caller learns the auto-tag it must release with.
fn lease_acquire<'a>(ctx: &SessionCtx, args: impl Iterator<Item = &'a str>) -> String {
    let mut ttl_ms = LEASE_TTL_DEFAULT_MS;
    let mut holder: Option<String> = None;
    for t in args {
        if let Some(v) = t.strip_prefix("ttl=") {
            match v.parse::<u64>() {
                Ok(n) if n >= 1 => ttl_ms = n.min(LEASE_TTL_MAX_MS),
                _ => return format!("ERR lease acquire: bad ttl '{t}' (1..={LEASE_TTL_MAX_MS})\n"),
            }
        } else if let Some(v) = t.strip_prefix("holder=") {
            if v.is_empty() || v.len() > 64 || !v.chars().all(|c| c.is_ascii_graphic()) {
                return "ERR lease acquire: holder must be 1..=64 printable ASCII chars\n"
                    .to_string();
            }
            holder = Some(v.to_string());
        } else {
            return format!("ERR lease acquire: unknown arg '{t}' (ttl=<ms> holder=<name>)\n");
        }
    }
    let holder = holder.unwrap_or_else(|| {
        let n = NEXT_LEASE_TAG.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        format!("drv-{n}")
    });
    let now = crate::metrics::now_us();
    let mut lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(held) = lease.as_ref().filter(|h| h.is_live(now)) {
        match held {
            crate::Lease::Turn(id) => return format!("ERR busy turn={id}\n"),
            crate::Lease::Drive {
                holder: h,
                expires_us,
            } if *h != holder => {
                return format!(
                    "ERR lease held holder={h} expires_in_ms={}\n",
                    (*expires_us - now) / 1000
                );
            }
            crate::Lease::Drive { .. } => {} // same holder — renew below
        }
    }
    *lease = Some(crate::Lease::Drive {
        holder: holder.clone(),
        expires_us: now + ttl_ms.saturating_mul(1000),
    });
    format!("OK lease acquired holder={holder} ttl_ms={ttl_ms} expires_in_ms={ttl_ms}\n")
}

/// `lease release [holder=<name>] [force]`: drop the cooperative lease. The holder
/// releases its own (name must match); `force` steals any cooperative hold; a
/// lapsed lease releases freely. A `turn`'s hard lease is owned by its own drop
/// guard and is never released here.
fn lease_release<'a>(ctx: &SessionCtx, args: impl Iterator<Item = &'a str>) -> String {
    let mut holder: Option<String> = None;
    let mut force = false;
    for t in args {
        if let Some(v) = t.strip_prefix("holder=") {
            holder = Some(v.to_string());
        } else if t == "force" {
            force = true;
        } else {
            return format!("ERR lease release: unknown arg '{t}' (holder=<name> force)\n");
        }
    }
    /// The action decided under an immutable read, applied after the borrow ends.
    enum Act {
        None,
        Release,
        Refuse(String),
    }
    let now = crate::metrics::now_us();
    let mut lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
    let act = match lease.as_ref() {
        None => Act::None,
        Some(crate::Lease::Turn(id)) => {
            if force {
                // Force-PREEMPT a wedged Turn lease: the crash-recovery escape hatch
                // for a turn whose driver crashed/disconnected. The synchronous serve
                // loop cannot detect the dead client until cmd_turn returns (up to its
                // timeout), so without this a crashed driver wedges every other writer.
                // Safe: cmd_turn's LeaseGuard now clears ONLY its own turn id, so a
                // fresh turn acquiring the freed slot is not stomped when the wedged
                // one finally returns. A deliberate operator override, like `signal`.
                Act::Release
            } else {
                Act::Refuse(format!(
                    "ERR busy turn={id} (a turn releases its own lease; use `lease release force` to preempt a wedged turn)\n"
                ))
            }
        }
        Some(crate::Lease::Drive {
            holder: h,
            expires_us,
        }) => {
            let live = *expires_us > now;
            if !live || force || holder.as_deref() == Some(h.as_str()) {
                Act::Release
            } else {
                Act::Refuse(format!(
                    "ERR lease held by {h} (pass holder={h} or force)\n"
                ))
            }
        }
    };
    match act {
        Act::None => "OK lease none\n".to_string(),
        Act::Release => {
            *lease = None;
            "OK lease released\n".to_string()
        }
        Act::Refuse(e) => e,
    }
}

/// `edges` / `grants` -> list this session's INBOUND capability edges (the rows
/// of its [`EdgeTable`]), the query face of the authority data `grant`/`revoke`
/// mint and remove (which had zero read surface before).
///
/// Header `OK <n>\n`, then one line per edge: `<src> <dst> <op>` where `<op>` is
/// the wire op token (`read-screen`/`write-input`/`signal`/`derive-loop`) and
/// `<dst>` is always THIS session's id (every row in the table targets it). The
/// bearer TOKEN is DELIBERATELY never emitted — it is the unforgeable secret; an
/// agent enumerates WHO may reach this session for WHAT, not the secrets. Sorted
/// by `(src, op)` for a stable listing. Cross-session (`@<sel>`) reads a sibling's
/// table through the same `@<selector>` resolution + `ReadScreen` gate.
pub(crate) fn cmd_edges(ctx: &SessionCtx) -> String {
    let mut edges = {
        let tbl = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
        tbl.edges()
    };
    edges.sort_by(|a, b| (a.src.as_str(), a.op.as_str()).cmp(&(b.src.as_str(), b.op.as_str())));
    let mut out = format!("OK {}\n", edges.len());
    for e in &edges {
        out.push_str(&format!(
            "{} {} {}\n",
            e.src.as_str(),
            e.dst.as_str(),
            e.op.as_str()
        ));
    }
    out
}

/// `edges --json` / `grants --json` -> `{"edges":[{"src":"..","dst":"..",
/// "op":".."}],"dst":"<self>"}`. The SAME edges `cmd_edges` lists (sorted, no
/// token), as a structured object an agent can consume without line-splitting.
pub(crate) fn cmd_edges_json(ctx: &SessionCtx) -> String {
    let (self_id, mut edges) = {
        let tbl = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
        (ctx.self_id.clone(), tbl.edges())
    };
    edges.sort_by(|a, b| (a.src.as_str(), a.op.as_str()).cmp(&(b.src.as_str(), b.op.as_str())));
    let items: Vec<String> = edges
        .iter()
        .map(|e| {
            format!(
                "{{{},{},{}}}",
                json_str_field("src", e.src.as_str()),
                json_str_field("dst", e.dst.as_str()),
                json_str_field("op", e.op.as_str()),
            )
        })
        .collect();
    json_ok(&format!(
        "{{\"edges\":[{}],{}}}",
        items.join(","),
        json_str_field("dst", self_id.as_str()),
    ))
}

/// `family [<sid>]` -> the session HIERARCHY for a target: its parent and its
/// direct children, from the registry's `parent` links (only a flat `sessions`
/// list was queryable before). Framed `OK <n>\n` + n lines (self/parent/child).
///
/// With NO argument the target is the RESOLVED session (`@<sel>` or self); with an
/// explicit `<sid>` argument the target is that session id (so an Owner can walk
/// the tree from any node without re-addressing). Header `OK\n`, then:
///   `self <sid> <state> <title>`
///   `parent <sid|-> ...`            (one line; `-` sid when the node is a root)
///   `child <sid> <state> <title>`  (zero or more, sorted by local id)
/// Titles are percent-encoded (single space-free tokens), matching `sessions`.
/// An unknown target id yields `ERR no such session\n` (fail-closed). An EXPLICIT
/// `<sid>` argument is Owner-only (a scoped Edge gets `ERR denied`); the no-arg
/// form is scoped to the already-gated resolved session.
///
/// For OWNER scope only, session-connection DISCOVERY rows follow the child
/// rows (design §6): `pushes` / `pushed-by` / `pulls` / `pulled-by`, one per
/// connected peer, in the same `<kind> <sid> <state> <title>` shape. Direction
/// per peer is classified from the edge ops (`write-input` present ⇒ pushes,
/// else pulls); outbound rows scan OTHER sessions' tables, which is exactly why
/// a scoped edge gets NONE of these rows (the `sessions`-is-Owner-only
/// disclosure rationale — a scoped caller reads its own inbound table via
/// `edges`). `parent`/`child` stay lineage, untouched.
pub(crate) fn cmd_family(ctx: &SessionCtx, store: &Store, scope: Scope, rest: &str) -> String {
    // Target sid: an explicit argument (Owner-only — arbitrary-node enumeration),
    // else the resolved session's own id (already gated by the dispatch).
    let target_sid = match rest.trim() {
        "" => ctx.self_id.clone(),
        s => {
            if scope != Scope::Owner {
                return "ERR denied\n".to_string();
            }
            SessionId::new(s)
        }
    };
    let snapshot = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.snapshot()
    };
    let Some(node) = snapshot.iter().find(|h| h.sid == target_sid) else {
        return "ERR no such session\n".to_string();
    };
    let line = |kind: &str, h: &crate::session_store::SessionHandle| {
        format!(
            "{kind} {} {} {}\n",
            h.sid.as_str(),
            h.state.as_str(),
            pct_encode(&h.title),
        )
    };
    // Build the body first so the header can carry a COUNT, matching every other
    // multi-line verb (`OK <n>` + n lines) — a bare `OK` made the aterm-ctl client
    // treat family as a single-line status reply and DROP the whole hierarchy.
    let mut body = String::new();
    body.push_str(&line("self", node));
    // Parent row: the parent sid + its live state/title if still registered, else
    // a bare `-` (root, or a parent that has since deregistered).
    match node.parent.as_ref() {
        Some(psid) => match snapshot.iter().find(|h| h.sid == *psid) {
            Some(ph) => body.push_str(&line("parent", ph)),
            None => body.push_str(&format!("parent {} unknown -\n", psid.as_str())),
        },
        None => body.push_str("parent - - -\n"),
    }
    // Direct children: every registered session whose parent is this node, sorted
    // by local id (snapshot is already local-id sorted).
    for h in snapshot
        .iter()
        .filter(|h| h.parent.as_ref() == Some(&target_sid))
    {
        body.push_str(&line("child", h));
    }
    // Session-connection rows, OWNER ONLY (see the doc above). Each peer's
    // table is locked briefly against the already-released snapshot (the
    // connections-module discipline: no store lock across a table lock).
    if scope == Scope::Owner {
        // OUTBOUND (this node → peer): rows live in each PEER's table with
        // src == target. Snapshot order keeps peers local-id sorted.
        let (mut pushes, mut pulls) = (Vec::new(), Vec::new());
        for h in &snapshot {
            if h.sid == target_sid {
                continue;
            }
            let rows = {
                let edges = h.ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
                edges.edges()
            };
            let ops: Vec<Op> = rows
                .iter()
                .filter(|e| e.src == target_sid && e.dst == h.sid)
                .map(|e| e.op)
                .collect();
            if ops.is_empty() {
                continue;
            }
            if ops.contains(&Op::WriteInput) {
                pushes.push(h);
            } else {
                pulls.push(h);
            }
        }
        // INBOUND (peer → this node): rows in the TARGET's own table, grouped
        // by src. A src may be foreign (wire-granted, never registered): its
        // row still prints, `unknown -` for state/title (the absent-parent
        // idiom), sorted by sid for a stable listing.
        let mut inbound: Vec<(SessionId, bool)> = Vec::new();
        {
            let rows = {
                let edges = node.ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
                edges.edges()
            };
            for e in rows {
                if e.src == e.dst || e.dst != target_sid {
                    continue;
                }
                let write = e.op == Op::WriteInput;
                match inbound.iter_mut().find(|(s, _)| *s == e.src) {
                    Some((_, w)) => *w |= write,
                    None => inbound.push((e.src, write)),
                }
            }
            inbound.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        }
        let peer_line = |kind: &str, sid: &SessionId| {
            snapshot.iter().find(|h| h.sid == *sid).map_or_else(
                || format!("{kind} {} unknown -\n", sid.as_str()),
                |h| line(kind, h),
            )
        };
        for h in &pushes {
            body.push_str(&line("pushes", h));
        }
        for (sid, _) in inbound.iter().filter(|(_, write)| *write) {
            body.push_str(&peer_line("pushed-by", sid));
        }
        for h in &pulls {
            body.push_str(&line("pulls", h));
        }
        for (sid, _) in inbound.iter().filter(|(_, write)| !*write) {
            body.push_str(&peer_line("pulled-by", sid));
        }
    }
    format!("OK {}\n{body}", body.lines().count())
}

/// `ready [timeout_ms]` -> block until the target session is ALIVE and IDLE, then
/// `OK ready <reason>\n`; `OK timeout\n` if it does not become ready in time
/// (default 30 000 ms, capped at 600 000); `ERR exited\n` if the session has
/// exited (it will never become ready). Lets an agent CHAIN sessions — spawn one,
/// `ready` on it, then drive it — without busy-polling a screen read.
///
/// Exactly two ready reasons are emitted:
///   * `prompt` — the newest OSC-133 block is at a fresh prompt (`PromptOnly`)
///     or a finished command (`Complete`): the shell is waiting for input. The
///     precise "prompt-end" signal, used when shell integration is present.
///   * `idle`   — the kernel's `IdleFor` watcher latched: `content_seq` held
///     stable across the settle window (output stopped changing). This is the
///     fallback for a session with no in-flight completed block — covering plain
///     shells (no shell integration) and the between-commands case alike.
///
/// Fully event-driven (NO poll): arms an `IdleFor` watcher, registers a
/// subscriber, and parks on its wake — driven by output / exit notifications and
/// the idle deadline. The registry lifecycle is re-checked on each wake, and a
/// session exit `notify`s us (`Wake::Exit`), so an exit is reported promptly.
pub(crate) fn cmd_ready(
    term: &Arc<Mutex<Terminal>>,
    store: &Store,
    session: u64,
    rest: &str,
    subscribers: &crate::subscribe::Subscribers,
) -> String {
    use std::time::{Duration, Instant};

    use aterm_core::terminal::{BlockState, WatcherSpec};

    use crate::session_store::SessionState;
    use crate::subscribe::SubscriberSet;

    // Accept a bare `<ms>` (original) or `timeout=<ms>` (k=v, aligning with the
    // rest of the wait family).
    let timeout_ms = {
        let t = rest.trim();
        t.strip_prefix("timeout=")
            .unwrap_or(t)
            .parse::<u64>()
            .unwrap_or(30_000)
            .min(600_000)
    };
    let now0 = Instant::now();
    let deadline = now0 + Duration::from_millis(timeout_ms);
    // The no-shell-integration settle window. This now drives the model-checked
    // kernel `IdleFor` (deterministic, no-silent-loss) INSTEAD of the old racy
    // "3 stable 20ms samples" — the engine resets the deadline on every content
    // advance (`observe_at`), so it latches only after SETTLE of TRUE quiet.
    const SETTLE: Duration = Duration::from_millis(60);

    // The lifecycle state of THIS resolved session, by its local id. `None` (not in
    // the registry — e.g. a headless unit term) is treated as Alive — UNLESS the
    // session was registered at arm and is now gone (deregistered on teardown),
    // which is a dead session the `Wake::Exit` notify woke us for (see `gone`).
    let was_registered = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_local(session)
        .is_some();
    let gone = |store: &Store| -> bool {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        match g.by_local(session).map(|h| h.state) {
            Some(SessionState::Exited) => true,
            None => was_registered,
            _ => false,
        }
    };

    // Arm one idle watcher up front; the engine auto-resets its deadline on output.
    let idle_id = term_lock(term).watch(WatcherSpec::IdleFor { dur: SETTLE }, now0);
    let disarm = |term: &Arc<Mutex<Terminal>>| {
        if let Some(id) = idle_id {
            term_lock(term).watch_disarm(id);
        }
    };
    // Subscribe so output wakes us (block-state changes ride content_seq, so the
    // notify covers the shell-integration path too) — event-driven, no poll.
    let sub = SubscriberSet::register(subscribers, &[session]);

    loop {
        if gone(store) {
            disarm(term);
            return "ERR exited\n".to_string();
        }
        let now = Instant::now();
        let (prompt, settled, next_dl) = {
            let mut t = term_lock(term);
            // Shell-integration fast path: newest block prompt/complete => ready
            // prompt (read directly so an ALREADY-ready session returns at once).
            let prompt = matches!(
                t.all_blocks().last().map(|b| b.state),
                Some(BlockState::PromptOnly | BlockState::Complete)
            );
            t.watch_expire(now); // host-injected idle fire
            let settled = idle_id.and_then(|id| t.watch_poll(id)).is_some();
            (prompt, settled, t.watch_next_deadline())
        };
        if prompt {
            disarm(term);
            return "OK ready prompt\n".to_string();
        }
        if settled {
            disarm(term);
            return "OK ready idle\n".to_string();
        }
        if now >= deadline {
            disarm(term);
            return "OK timeout\n".to_string();
        }
        // Park until a REAL event wakes us — fully event-driven, no re-poll: an
        // output burst or session exit (both `notify` us), the kernel idle
        // deadline (`next_dl`, always armed here so block-state transitions are
        // re-checked within the settle window), or the overall deadline.
        let mut wake = deadline;
        if let Some(dl) = next_dl {
            wake = wake.min(dl);
        }
        let dur = wake
            .saturating_duration_since(now)
            .max(Duration::from_millis(1));
        let _ = sub.wait(dur);
    }
}

/// `await <idle <ms> | seq [<n>] | match <re> [rows <a> <b>] | block> [timeout <ms>]`
/// — block until the Observation Kernel (L0) latches the predicate, then return
/// `OK <kind> <seq>`; `OK timeout` if the overall deadline elapses; `ERR exited`
/// if the session dies.
///
/// This is the L1 exposure of the core primitive. The CORRECTNESS — no-silent-
/// loss for content/match/block, and a deterministic idle deadline — lives in the
/// kernel (`observe_at` at the `post_process` seam, model-checked by
/// `watcher_latch_model` / `idle_deadline_model`). This verb only *waits*, and it
/// is **fully event-driven, with no polling**: it registers a subscriber and
/// parks on its wake, so it sleeps until a REAL event arrives —
///   * an output burst   (`Wake::Output` → `Subscribers::notify`) for content/
///     match/block predicates,
///   * the next idle deadline (the exact `IdleFor` fire instant, via
///     `watch_next_deadline`) for `await idle`,
///   * session exit       (`Wake::Exit` → notify) → `ERR exited`,
///   * the overall timeout (the ultimate liveness backstop) → `OK timeout`.
///
/// CPU is ~0% while parked; every wake corresponds to an event the caller cares
/// about.
pub(crate) fn cmd_await(
    term: &Arc<Mutex<Terminal>>,
    store: &Store,
    session: u64,
    rest: &str,
    subscribers: &crate::subscribe::Subscribers,
) -> String {
    use std::time::{Duration, Instant};

    use aterm_core::terminal::{RowRange, WatcherSpec};

    use crate::session_store::SessionState;
    use crate::subscribe::SubscriberSet;

    const USAGE: &str =
        "ERR usage: await <idle <ms>|seq [<n>]|match <re> [rows <a> <b>]|block> [timeout <ms>]\n";

    // Split off an optional `timeout <ms>` anywhere in the args; the rest is the
    // predicate + its arguments.
    let toks: Vec<&str> = rest.split_whitespace().collect();
    let mut timeout_ms = 30_000u64;
    let mut args: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        // Accept BOTH `timeout <ms>` (the original await grammar) and `timeout=<ms>`
        // (the k=v form turn/turns/subscribe use) so the wait family reads uniformly.
        if let Some(v) = toks[i].strip_prefix("timeout=") {
            timeout_ms = v.parse().unwrap_or(30_000);
            i += 1;
        } else if toks[i] == "timeout" && i + 1 < toks.len() {
            timeout_ms = toks[i + 1].parse().unwrap_or(30_000);
            i += 2;
        } else {
            args.push(toks[i]);
            i += 1;
        }
    }
    let timeout_ms = timeout_ms.min(600_000);
    let Some(&kind) = args.first() else {
        return USAGE.to_string();
    };

    let now0 = Instant::now();
    // Arm the predicate. The `match` verb compiles an UNTRUSTED regex: that work
    // is now bounded (row_matcher caps pattern length + NFA/DFA size) but is
    // still non-trivial parse/compile, so do it BEFORE taking the terminal lock.
    // Holding the Mutex across it would let a crafted (bounded) pattern stall
    // rendering / PTY processing / other control verbs that contend for the same
    // lock; here we lock only long enough to install the compiled matcher.
    let armed = if kind == "match" {
        let Some(pat) = args.get(1) else {
            return USAGE.to_string();
        };
        let range = match (args.get(2), args.get(3), args.get(4)) {
            (Some(&"rows"), Some(a), Some(b)) => match (a.parse::<usize>(), b.parse::<usize>()) {
                (Ok(start), Ok(end)) => RowRange::Span { start, end },
                _ => return USAGE.to_string(),
            },
            _ => RowRange::All,
        };
        // Compile the regex in `aterm-observe` (regex out of the engine core),
        // OUTSIDE the lock; only `watch_rows` runs under it.
        let matcher = match aterm_observe::row_matcher(pat) {
            Ok(m) => m,
            Err(_) => return "ERR badregex\n".to_string(),
        };
        term_lock(term).watch_rows(matcher, range, now0)
    } else {
        let mut t = term_lock(term);
        match kind {
            "idle" => {
                let Some(ms) = args.get(1).and_then(|s| s.parse::<u64>().ok()) else {
                    return USAGE.to_string();
                };
                t.watch(
                    WatcherSpec::IdleFor {
                        dur: Duration::from_millis(ms),
                    },
                    now0,
                )
            }
            "seq" => {
                // Default `after` = the current content_seq (wait for the NEXT change).
                let after = args
                    .get(1)
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or_else(|| t.content_seq());
                t.watch(WatcherSpec::SeqAdvanced { after }, now0)
            }
            "block" => t.watch(WatcherSpec::BlockComplete, now0),
            _ => return USAGE.to_string(),
        }
    };
    let Some(id) = armed else {
        return "ERR watcher budget full\n".to_string();
    };

    let overall = now0 + Duration::from_millis(timeout_ms);

    // Register a subscriber on THIS session: the producer's `Wake::Output` and
    // `Wake::Exit` hooks (`Subscribers::notify`) wake us the instant output lands
    // or the session dies — so content predicates (`seq`/`match`/`block`) and
    // exit detection are event-driven with NO fixed-interval poll. `await idle`
    // parks straight to the kernel's `next_deadline`. The single-slot notify is
    // lossless (a wake that arrives between two `wait`s stays pending), and the
    // overall timeout is the ultimate backstop — so no re-query is needed.
    let sub = SubscriberSet::register(subscribers, &[session]);

    // Whether the session was in the registry at arm. A session that WAS
    // registered but is later GONE (deregistered during teardown) is dead — and
    // the `Wake::Exit` notify can race the deregistration, so the woken thread may
    // see `None`. Treating "was registered, now None" as exited closes that race;
    // a headless unit term (never registered) stays Alive on `None` as before.
    let was_registered = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_local(session)
        .is_some();
    let exited = |store: &Store| -> bool {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        match g.by_local(session).map(|h| h.state) {
            Some(SessionState::Exited) => true,
            None => was_registered,
            _ => false,
        }
    };

    loop {
        if exited(store) {
            term_lock(term).watch_disarm(id);
            return "ERR exited\n".to_string();
        }
        let now = Instant::now();
        let (sat, next_dl) = {
            let mut t = term_lock(term);
            t.watch_expire(now); // fire any elapsed idle deadline (host-injected `now`)
            (t.watch_poll(id), t.watch_next_deadline())
        };
        if let Some(s) = sat {
            term_lock(term).watch_disarm(id);
            return format!("OK {kind} {}\n", s.seq);
        }
        if now >= overall {
            term_lock(term).watch_disarm(id);
            return "OK timeout\n".to_string();
        }
        // Park until a REAL event wakes us — fully event-driven, no re-poll:
        // an output burst or session exit (both `notify` us), the next idle
        // deadline (`next_dl`), or the overall timeout (the backstop).
        let mut wake = overall;
        if let Some(dl) = next_dl {
            wake = wake.min(dl);
        }
        let dur = wake
            .saturating_duration_since(now)
            .max(Duration::from_millis(1));
        let _ = sub.wait(dur);
    }
}

/// Process-wide `turn` id mint: every turn gets a unique id, reported on the
/// reply status line (`id=<n>`) and named by the lease's `ERR busy turn=<n>` so
/// a refused writer can tell WHICH exchange it collided with.
static NEXT_TURN_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Input delivery for [`cmd_turn`], resolved by the dispatch site so the ONE
/// orchestration below serves both targets: the SELF/active-tab path delivers
/// through the event loop (`Wake::Input`, so gesture/window side-effects run for
/// the tab the user is looking at), the CROSS path through the source-blind seam
/// (`seam_egress` on the resolved target's `(term, sink)`). `cmd_turn` itself
/// never touches an input API — it only orchestrates watchers around these two
/// calls, so the Tier-1 byte-indistinguishability of both paths is untouched.
pub(crate) struct TurnIo<'a> {
    /// Deliver the message text into the target's input path with PASTE
    /// semantics (control bytes stripped by `format_paste`; bracketed-paste
    /// framing only when the app itself enabled the mode).
    /// `false` means the bounded egress did not accept the complete paste; the
    /// turn stops before submit and the durable operator marks its intent in-doubt.
    pub paste: &'a dyn Fn(&str) -> bool,
    /// Press a named key (the `key` verb vocabulary, e.g. "enter"). `false`
    /// means the name did not parse — reported as a usage error.
    pub press: &'a dyn Fn(&str) -> bool,
}

/// `turn [idle=<ms>] [timeout=<ms>] [submit=<key|none>] [settle=match:<re>]
/// [submit_window=<ms>] [presses=<n>] <text>` — one complete HUMAN TURN against
/// the target session: type `<text>`, submit it with a real keypress, block until
/// the app's response settles, and return the settled screen —
/// `OK <rows> turn submitted=<0|1> status=<settled|timeout> seq=<n> id=<n>
/// dur_ms=<n> hash=<hex16>` then `<rows>` text rows (the same rows `text` prints).
/// `hash` is the FNV-1a of the settled screen (the SAME value `history` reports for
/// this `id`), so a driver can diff two turns' outputs inline without a second call.
///
/// WHY A COMPOSITE VERB: driving one turn out of `paste` + `key enter` +
/// `await idle` from a client is RACY at the submit seam — a TUI line editor
/// that is still ingesting a large paste burst (agent CLIs bracket-paste big
/// input into a "[Pasted text]" chip) swallows an Enter that arrives during
/// ingestion, and the client cannot see when ingestion ended. Server-side, the
/// Observation Kernel can: this verb parks on the SAME model-checked watcher
/// primitives `await` uses and only presses submit AFTER the echo settled, then
/// VERIFIES the press landed within `submit_window` (default 2000ms) and re-presses
/// up to `presses` times (default 3) before giving up. `presses=1` disables re-press
/// for a slow/high-latency link where a second Enter could auto-confirm a following
/// prompt. HOW the press is verified is `submit_verify=`: `block` keys on the OSC-133
/// command-start (a command block enters Executing) — ambient-repaint-IMMUNE, so a
/// periodically-repainting TUI cannot false-verify a swallowed Enter; `seq` keys on a
/// bare `content_seq` advance. The DEFAULT is AUTO: `block` when the target is at a
/// shell prompt (a submit will start a command — the sound choice), else `seq` (a
/// full-screen TUI or a session with no shell integration has no press-attributable
/// signal). So a shell drive is sound by default. AUTO additionally DEGRADES
/// honestly: when the window expires with content having moved after the press but
/// no command block started, the prompt-shaped block AUTO keyed on has proven
/// STALE (a desynced 133 stream — e.g. a distro integration whose C mark never
/// lands), so AUTO takes the seq verdict (`submitted=1`) instead of re-pressing —
/// a press whose echo moved the screen was consumed, and another Enter would land
/// in the running command. Re-press is reserved for a press after which NOTHING
/// moved (the true swallowed case). Explicit `submit_verify=block` never degrades.
/// The whole exchange is one request line — no client-side timing enters the
/// protocol.
///
/// The gesture vocabulary is app-agnostic on purpose: nothing here knows what
/// program is driven. Claude Code, codex, a shell, emacs (`submit=none` types
/// without submitting; any `key`-verb name may be the submit key) — anything in
/// a PTY holds a conversation through the exact surface a human uses. For an
/// ordinary interactive `turn`, a human typing INTO the target mid-turn simply
/// extends the same content stream: the settle watcher keeps resetting, and the
/// returned screen contains whatever both parties produced. The durable operator
/// uses the guarded twin below, which instead aborts before submit when any
/// foreign PTY input attempt interjects.
///
/// SETTLE (phase 3) defaults to GLOBAL idle — no content change for `idle_ms` —
/// which assumes the app STOPS painting when done. A periodically-repainting TUI
/// (a status-bar clock, a spinner, `watch -n1`) never goes idle, so an idle-settle
/// turn against it burns the full timeout; pass `settle=match:<re>` to key settle
/// on a screen PATTERN instead (the same regex/`RowRange` machinery as
/// `await match`), or lower `idle=` below the repaint cadence.
///
/// Fully event-driven like `ready`/`await` (subscriber park + kernel deadlines,
/// zero polling). Empty `<text>` skips the paste (submit-only: useful to fire a
/// chip already sitting in the target's editor); `submit=none` skips the press
/// (type-only). `ERR exited` if the target dies at any phase.
pub(crate) fn cmd_turn(
    term: &Arc<Mutex<Terminal>>,
    store: &Store,
    session: u64,
    rest: &str,
    subscribers: &crate::subscribe::Subscribers,
    ctx: &SessionCtx,
    io: &TurnIo<'_>,
) -> String {
    let mut allow_preflight = || Ok(());
    let mut allow_submit = || Ok(());
    cmd_turn_guarded(
        term,
        store,
        session,
        rest,
        subscribers,
        ctx,
        io,
        &mut allow_preflight,
        &mut allow_submit,
        None,
    )
}

/// `turn` with last-moment validation hooks. `preflight` runs after the exclusive
/// turn lease is held and before any watcher is armed or input is emitted.
/// `pre_submit` runs after paste echo-settle and immediately before every submit
/// press. The embedded operator uses both plus its sink epoch to prevent a human
/// or raw controller interjection from turning a previously-safe paste into an
/// unsafe Enter.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_turn_guarded(
    term: &Arc<Mutex<Terminal>>,
    store: &Store,
    session: u64,
    rest: &str,
    subscribers: &crate::subscribe::Subscribers,
    ctx: &SessionCtx,
    io: &TurnIo<'_>,
    preflight: &mut dyn FnMut() -> Result<(), String>,
    pre_submit: &mut dyn FnMut() -> Result<(), String>,
    ledger_text_override: Option<&str>,
) -> String {
    use std::time::{Duration, Instant};

    use aterm_core::terminal::{BlockState, WatcherSpec};

    use crate::session_store::SessionState;
    use crate::subscribe::SubscriberSet;

    const USAGE: &str = "ERR usage: turn [idle=<ms>] [timeout=<ms>] [submit=<key|none>] [settle=match:<re>] [submit_window=<ms>] [presses=<n>] [submit_verify=<auto|seq|block>] <text>\n";
    /// Echo-settle window: the paste burst has been ingested and painted.
    const ECHO_SETTLE: Duration = Duration::from_millis(150);
    /// Echo phase cap — a busy app (spinner mid-turn) may never go echo-quiet;
    /// after this we press anyway rather than stall the whole turn.
    const ECHO_CAP: Duration = Duration::from_secs(5);

    // ── grammar: leading k=v options, the remainder verbatim is the message ──
    let mut idle_ms = 1_500u64;
    let mut timeout_ms = 240_000u64;
    let mut submit = "enter".to_string();
    // Submit-verification knobs: `submit_window` is how long one press has to move
    // `content_seq` before a re-press; `presses` is the max total presses.
    // `presses=1` disables re-press for slow/high-latency links where a duplicate
    // Enter would be harmful (e.g. auto-confirming a following prompt).
    let mut submit_window_ms = 2_000u64;
    let mut presses = 3u32;
    // `submit_verify` chooses the submit-verification signal. `block` verifies the
    // submit against the OSC-133 command-start transition (ambient-repaint-immune);
    // `seq` uses a bare `content_seq` advance. DEFAULT (None) is AUTO: `block` when
    // the target is at a shell prompt (a submit will start a command — the sound
    // choice), else `seq` (a TUI or no shell integration has no press-attributable
    // signal, so seq is the best available). So a shell drive is sound by default.
    let mut submit_verify: Option<bool> = None;
    // `settle=match:<re>` keys phase-3 settle on a screen PATTERN instead of global
    // idle; None => the default idle settle. Compiled below, before any input.
    let mut settle_match: Option<String> = None;
    let mut text = rest.trim_start();
    loop {
        let (tok, tail) = match text.split_once(char::is_whitespace) {
            Some((t, rest)) => (t, rest.trim_start()),
            None => (text, ""),
        };
        if tok == "--" {
            text = tail;
            break;
        }
        let Some((k, v)) = tok.split_once('=') else {
            break;
        };
        match k {
            "idle" => match v.parse::<u64>() {
                Ok(ms) => idle_ms = ms.clamp(1, 600_000),
                Err(_) => return USAGE.to_string(),
            },
            "timeout" => match v.parse::<u64>() {
                Ok(ms) => timeout_ms = ms,
                Err(_) => return USAGE.to_string(),
            },
            "submit" => submit = v.to_string(),
            "submit_window" => match v.parse::<u64>() {
                Ok(ms) => submit_window_ms = ms.clamp(1, 600_000),
                Err(_) => return USAGE.to_string(),
            },
            "presses" => match v.parse::<u32>() {
                Ok(n) if n >= 1 => presses = n.min(10),
                _ => return USAGE.to_string(),
            },
            "submit_verify" => match v {
                "seq" => submit_verify = Some(false),
                "block" => submit_verify = Some(true),
                "auto" => submit_verify = None,
                _ => return USAGE.to_string(),
            },
            "settle" => match v.strip_prefix("match:") {
                Some(pat) if !pat.is_empty() => settle_match = Some(pat.to_string()),
                _ => return USAGE.to_string(),
            },
            _ => break, // not an option: the message itself starts with `word=…`
        }
        text = tail;
    }
    let timeout_ms = timeout_ms.min(600_000);

    // Compile a `settle=match:<re>` pattern up front (untrusted regex, bounded by
    // `row_matcher`) OUTSIDE the terminal lock and BEFORE the lease/paste, so a bad
    // pattern fails fast without ever typing into the target or holding the lease.
    let settle_matcher = match &settle_match {
        Some(pat) => match aterm_observe::row_matcher(pat) {
            Ok(m) => Some(m),
            Err(_) => return "ERR badregex\n".to_string(),
        },
        None => None,
    };

    // ── turn LEASE: one driver per session at a time. Acquire-or-busy under the
    // lock (the dispatch-level check is the fast fail; THIS is authoritative), and
    // release on EVERY exit via the drop guard — including timeouts, exits, usage
    // errors and watcher-budget failures below.
    let turn_id = {
        let mut lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
        // A LIVE lease of either kind blocks a new turn: a `turn` lease is the usual
        // busy case; a cooperative `Drive` lease (unexpired) refuses too, so a raw
        // driver's hold is not stomped mid-drive. An EXPIRED `Drive` lease is free —
        // the turn takes over (and its guard clears the slot on exit).
        if let Some(held) = lease
            .as_ref()
            .filter(|h| h.is_live(crate::metrics::now_us()))
        {
            return match held {
                crate::Lease::Turn(id) => format!("ERR busy turn={id}\n"),
                crate::Lease::Drive { holder, .. } => format!("ERR busy lease={holder}\n"),
            };
        }
        let id = NEXT_TURN_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        *lease = Some(crate::Lease::Turn(id));
        id
    };
    struct LeaseGuard<'a> {
        /// `ctx.turn_lease` — a NAMED field (not a tuple `.0`) so the
        /// lock-order census (OB-7) resolves this Drop-path acquisition to the
        /// same `turn_lease` identity as the acquire above.
        turn_lease: &'a std::sync::Mutex<Option<crate::Lease>>,
        /// This turn's own id. The guard clears the slot ONLY if it still holds THIS
        /// turn's lease — so a `lease release force` preemption of a WEDGED turn
        /// (whose driver crashed) followed by a fresh turn acquiring the slot is not
        /// stomped when this turn finally returns and its guard drops.
        id: u64,
    }
    impl Drop for LeaseGuard<'_> {
        fn drop(&mut self) {
            let mut lease = self.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
            if matches!(lease.as_ref(), Some(crate::Lease::Turn(held)) if *held == self.id) {
                *lease = None;
            }
        }
    }
    let _lease = LeaseGuard {
        turn_lease: &ctx.turn_lease,
        id: turn_id,
    };

    if let Err(error) = preflight() {
        let error = error.trim_end_matches(['\r', '\n']);
        return format!("ERR {error}\n");
    }

    let now0 = Instant::now();
    let started_ms = crate::turn_ledger::now_ms();
    let deadline = now0 + Duration::from_millis(timeout_ms);

    // Exit detection, identical to `await` (including the deregistration race:
    // "was registered, now gone" is exited).
    let was_registered = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_local(session)
        .is_some();
    let exited = |store: &Store| -> bool {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        match g.by_local(session).map(|h| h.state) {
            Some(SessionState::Exited) => true,
            None => was_registered,
            _ => false,
        }
    };

    // One subscriber for the whole turn: output/exit notifies wake every phase.
    let sub = SubscriberSet::register(subscribers, &[session]);

    // Park until watcher `id` latches, `until` passes, or the session exits.
    // Returns Some(latched seq), None on phase deadline; `ERR exited` bubbles
    // via the Err arm. Disarms the watcher on every path.
    enum Phase {
        Latched,
        Deadline,
        Exited,
    }
    let wait = |id: aterm_core::terminal::WatchId, until: Instant| -> Phase {
        loop {
            if exited(store) {
                term_lock(term).watch_disarm(id);
                return Phase::Exited;
            }
            let now = Instant::now();
            let (sat, next_dl) = {
                let mut t = term_lock(term);
                t.watch_expire(now);
                (t.watch_poll(id), t.watch_next_deadline())
            };
            if sat.is_some() {
                term_lock(term).watch_disarm(id);
                return Phase::Latched;
            }
            if now >= until {
                term_lock(term).watch_disarm(id);
                return Phase::Deadline;
            }
            let mut wake = until;
            if let Some(dl) = next_dl {
                wake = wake.min(dl);
            }
            let dur = wake
                .saturating_duration_since(now)
                .max(Duration::from_millis(1));
            let _ = sub.wait(dur);
        }
    };
    // Arm a watcher; None (budget full) fails the whole verb honestly.
    let arm = |spec: WatcherSpec| -> Option<aterm_core::terminal::WatchId> {
        term_lock(term).watch(spec, Instant::now())
    };

    // ── phase 1: type. Paste semantics; the seam strips control bytes. ──
    if !text.is_empty() {
        if !(io.paste)(text) {
            return "ERR paste delivery failed\n".to_string();
        }
        // Echo settle: the editor ingested + painted the burst. Cap the phase so
        // an app that is ALREADY animating (mid-turn spinner) cannot stall us.
        let Some(id) = arm(WatcherSpec::IdleFor { dur: ECHO_SETTLE }) else {
            return "ERR watcher budget full\n".to_string();
        };
        match wait(id, deadline.min(Instant::now() + ECHO_CAP)) {
            Phase::Exited => return "ERR exited\n".to_string(),
            Phase::Latched | Phase::Deadline => {}
        }
    }

    // ── phase 2: verified submit. Press; content_seq MUST advance within the
    // submit window, else the press was swallowed mid-ingestion — press again.
    // `submitted` reports "a press VERIFIABLY landed": with `submit=none` there
    // is no press, so it stays 0 (honest) while the settle phase still runs.
    let mut submitted = false;
    if submit != "none" {
        // EFFECTIVE verification mode. Explicit `submit_verify=` wins; otherwise AUTO:
        // `block` when the target is at a shell prompt (a submit will start a command,
        // so the OSC-133 signal exists and is ambient-repaint-immune), else `seq`.
        // AUTO retains the right to DEGRADE below: a prompt-shaped block may be a
        // STALE one from a desynced 133 stream (a distro-side integration whose C
        // mark never lands — stock Ubuntu bash sources vte.sh twice and its PS0
        // 133;C is clobbered), where block verification can never succeed even
        // though every press lands. Explicit `block` stays strict.
        let auto_mode = submit_verify.is_none();
        let submit_verify_block = submit_verify.unwrap_or_else(|| {
            matches!(
                term_lock(term).all_blocks().last().map(|b| b.state),
                Some(BlockState::PromptOnly | BlockState::EnteringCommand)
            )
        });
        // Count of blocks that have STARTED a command (Executing or Complete). A
        // submit at a prompt transitions the current prompt block to Executing (OSC
        // 133;C) — this count RISES; an ambient repaint never touches it. (Counting
        // the transition, not a new block id: a new id appears only at the NEXT
        // prompt, after the command completes — too late to verify the submit.)
        let commands_started = |t: &Terminal| {
            t.all_blocks()
                .filter(|b| matches!(b.state, BlockState::Executing | BlockState::Complete))
                .count()
        };
        for _ in 0..presses {
            let base_block = commands_started(&term_lock(term));
            let window = deadline.min(Instant::now() + Duration::from_millis(submit_window_ms));
            // Arm BEFORE the press so the press's own content change latches it
            // (`after` = pre-press seq); a post-press arm would race past it.
            let after = term_lock(term).content_seq();
            let Some(mut id) = arm(WatcherSpec::SeqAdvanced { after }) else {
                return "ERR watcher budget full\n".to_string();
            };
            // The watcher must be armed before input, but the guard remains the
            // final operation before the press. For the durable operator this
            // re-reads live non-approval state and checks its attempted-input
            // epoch; generic turns pass an allow hook and retain their behavior.
            if let Err(error) = pre_submit() {
                term_lock(term).watch_disarm(id);
                let error = error.trim_end_matches(['\r', '\n']);
                return format!("ERR {error}\n");
            }
            if !(io.press)(&submit) {
                term_lock(term).watch_disarm(id);
                return USAGE.to_string();
            }
            // `seq`: the first content advance verifies. `block`: only a NEW command
            // block does — re-arm past ambient repaints until the window expires, so a
            // periodically-repainting TUI cannot false-verify a swallowed Enter.
            // `advanced` records whether THIS press's window saw ANY content advance:
            // it separates a swallowed press (nothing moved — press again) from a
            // press the app consumed without starting a command block.
            let mut advanced = false;
            loop {
                match wait(id, window) {
                    Phase::Exited => return "ERR exited\n".to_string(),
                    Phase::Latched => {
                        advanced = true;
                        if !submit_verify_block || commands_started(&term_lock(term)) > base_block {
                            submitted = true;
                            break;
                        }
                        // block mode, ambient repaint only: keep waiting in-window.
                        if Instant::now() >= window {
                            break;
                        }
                        let after = term_lock(term).content_seq();
                        match arm(WatcherSpec::SeqAdvanced { after }) {
                            Some(nid) => id = nid,
                            None => return "ERR watcher budget full\n".to_string(),
                        }
                    }
                    Phase::Deadline => break,
                }
            }
            // AUTO degrade: the window expired with content moving after the press
            // but no command block starting — and the session has NEVER started
            // one (`base_block == 0`). A stream that has never once fired 133;C
            // has proven the block signal unavailable (dead or desynced
            // integration), so fall back to the verdict AUTO would have chosen
            // without it — the seq advance. The press provably reached the app;
            // re-pressing here would type a REAL extra Enter into a target that
            // already consumed the first (and a driver told submitted=0 would
            // re-type the whole turn — the double-type this closes). A session
            // whose stream HAS started blocks before keeps the strict re-press:
            // there, no-block-plus-ambient-movement is exactly what a swallowed
            // Enter beside a spinner looks like, and claiming submitted=1 on it
            // would break this fn's "a press VERIFIABLY landed" contract.
            // Explicit `submit_verify=block` stays strict unconditionally:
            // soundness was asked for by name.
            if !submitted && advanced && auto_mode && base_block == 0 {
                submitted = true;
            }
            if submitted || Instant::now() >= deadline {
                break; // verified, or the overall deadline: report honestly
            }
        }
    }

    // ── phase 3: the turn settles — no content change for `idle_ms`. ──
    // An unverified submit skips the settle wait (waiting `idle_ms` for a turn
    // that never started would just burn the deadline) and reports honestly.
    let mut status = "timeout";
    if submitted || submit == "none" {
        // Default settle = GLOBAL idle: no content change for `idle_ms`, which
        // assumes the app stops painting when done. `settle=match:<re>` keys on a
        // screen PATTERN instead — for a periodically-repainting TUI (clock,
        // spinner, `watch`) that never goes idle and would otherwise burn the
        // whole timeout.
        let armed = match &settle_matcher {
            Some(m) => term_lock(term).watch_rows(
                m.clone(),
                aterm_core::terminal::RowRange::All,
                Instant::now(),
            ),
            None => arm(WatcherSpec::IdleFor {
                dur: Duration::from_millis(idle_ms),
            }),
        };
        let Some(id) = armed else {
            return "ERR watcher budget full\n".to_string();
        };
        match wait(id, deadline) {
            Phase::Exited => return "ERR exited\n".to_string(),
            Phase::Latched => status = "settled",
            Phase::Deadline => {}
        }
    }

    // ── reply: the settled screen, framed like `text` with turn metadata on the
    // status line (the client streams `OK <count> …` + count rows). ──
    let (rows, seq, screen) = {
        let t = term_lock(term);
        let rows = t.rows() as usize;
        let seq = t.content_seq();
        let mut screen = String::new();
        for r in 0..rows {
            screen.push_str(&super::visible_row(&t, r));
            screen.push('\n');
        }
        (rows, seq, screen)
    };
    // Record this exchange in the session TURN LEDGER — read back by `turns`,
    // streamed by the `subscribe … events` digest. Drop-oldest + bounded; the
    // ledger lock is disjoint from the terminal lock (released just above).
    // Computed ONCE and shared by the ledger record AND the verdict line below, so
    // the inline `dur_ms=`/`hash=` an AI diffs by (the catalog + INTROSPECTION.md
    // promise them) are the SAME values `history` later reports for this id.
    let dur_ms = now0.elapsed().as_millis() as u64;
    let screen_hash = crate::turn_ledger::fnv1a_64(screen.as_bytes());
    {
        let mut ledger = ctx.turns.lock().unwrap_or_else(|p| p.into_inner());
        ledger.push(crate::turn_ledger::TurnRecord {
            id: turn_id,
            started_ms,
            dur_ms,
            submitted,
            status,
            // Durable operator proposals may contain sensitive model/context
            // text. Their caller supplies a fixed redaction marker; ordinary
            // interactive turns retain the historical submitted-text ledger.
            text: crate::turn_ledger::clamp_text(ledger_text_override.unwrap_or(text)),
            screen_hash,
            seq,
        });
    }
    // Wake any `events` subscriber NOW so it scans the fresh record immediately
    // rather than on its next timeout tick — the same content-less notify the
    // output/exit producers use (the ledger write above is the scannable state).
    if subscribers.any() {
        subscribers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .notify(session);
    }
    let mut out = format!(
        "OK {rows} turn submitted={} status={status} seq={seq} id={turn_id} dur_ms={dur_ms} hash={screen_hash:016x}\n",
        u8::from(submitted)
    );
    out.push_str(&screen);
    out
}

/// `history [<n>] [since=<id>]` -> the session's TURN LEDGER, newest-last, framed
/// `OK <count>\n` + one line per record (streaming like `text`). `<n>` keeps the
/// last n (default: all retained); `since=<id>` keeps records with id strictly
/// greater than `<id>` (poll the ledger forward). Each record: `turn <id>
/// submitted=<0|1> status=<..> started_ms=<..> dur_ms=<..> seq=<..> hash=<hex16>
/// text=<pct-encoded>` — the durable memory of what a driver typed and what
/// settled, keyed by the same id the `turn` reply and events digest use.
pub(crate) fn cmd_history(ctx: &SessionCtx, rest: &str) -> String {
    let mut n = 0usize;
    let mut since: Option<u64> = None;
    for tok in rest.split_whitespace() {
        if let Some(v) = tok.strip_prefix("since=") {
            match v.parse::<u64>() {
                Ok(id) => since = Some(id),
                Err(_) => return "ERR usage: history [<n>] [since=<id>]\n".to_string(),
            }
        } else if let Ok(v) = tok.parse::<usize>() {
            n = v;
        } else {
            return "ERR usage: history [<n>] [since=<id>]\n".to_string();
        }
    }
    let ledger = ctx.turns.lock().unwrap_or_else(|p| p.into_inner());
    let mut recs: Vec<&crate::turn_ledger::TurnRecord> = ledger.since(since).collect();
    if n > 0 && recs.len() > n {
        recs.drain(..recs.len() - n);
    }
    let mut out = format!("OK {}\n", recs.len());
    for r in recs {
        out.push_str(&format!(
            "turn {} submitted={} status={} started_ms={} dur_ms={} seq={} hash={:016x} text={}\n",
            r.id,
            u8::from(r.submitted),
            r.status,
            r.started_ms,
            r.dur_ms,
            r.seq,
            r.screen_hash,
            super::pct_encode(&r.text),
        ));
    }
    out
}

/// `meta` — the SESSION-METADATA verb (stage 1). Three forms:
///
/// * bare `meta` (Read): one status line joining the ENGINE identity (live OSC
///   title, reported cwd, lifecycle state) with the USER identity (`meta set`
///   fields) — `OK title=<pct> user_title=<pct|-> description=<pct|-> icon=<pct|->
///   role=<pct|-> attention=<pct|-> cwd=<pct|-> state=<s>`. Every free-text
///   value is pct-encoded so the reply is always ONE line; `-` marks an unset
///   optional.
/// * `meta set <title|description|icon|role|attention> <text...>`
///   (write-escalated by the dispatch gate): stamp the operator's identity on
///   the session. Byte caps (after trim): title ≤ 120, description ≤ 1024,
///   icon ≤ 64, role ≤ 64, attention ≤ 256 — over-cap is a hard ERR, never a
///   silent truncation (the caller must know its label was refused). C0/C1,
///   line separators, bidi controls, and spoof-relevant invisible format
///   characters are rejected rather than silently stored. The user title
///   OUTRANKS the OSC title in tab labels. `role operator` designates the
///   fleet operator to the menu-bar status item; a non-empty `attention` is
///   the typed needs-human escalation it badges and lists.
/// * `meta unset <field>` — clear a field (labels fall back down the chain).
///
/// Returns `(reply, changed)`: `changed` is `true` only when a stored value
/// ACTUALLY moved — the dispatch arm keys its side-effects (tab-strip repaint
/// wake + subscriber notify) on it, so a no-op re-set stays silent. The
/// `meta-change` timeline record happens HERE (same change gate), so `timeline`
/// and the `events` digest see exactly one event per real change.
pub(crate) fn cmd_meta(
    term: &Arc<Mutex<Terminal>>,
    store: &Store,
    session: u64,
    ctx: &SessionCtx,
    rest: &str,
) -> (String, bool) {
    let mut toks = rest.split_whitespace();
    match toks.next() {
        None => (meta_status(term, store, session, ctx), false),
        Some("set") => {
            let Some(field) = toks.next() else {
                return (
                    "ERR usage: meta set <title|description|icon|role|attention> <text...>\n".to_string(),
                    false,
                );
            };
            let Some(typed) = MetaField::parse(field) else {
                return (
                    "ERR unknown meta field (title|description|icon|role|attention)\n".to_string(),
                    false,
                );
            };
            // The VALUE is the raw remainder after the field token (single-space
            // grammar like `turn`) — token iteration would collapse interior
            // whitespace the caller meant to keep. This is WIRE GRAMMAR and stops
            // here: everything below is the shared policy ladder, which a GUI
            // caller (already holding a discrete value) enters directly.
            let value = rest
                .split_once("set")
                .and_then(|(_, r)| r.trim_start().split_once(char::is_whitespace))
                .map_or("", |(_, v)| v);
            match write_session_meta(ctx, typed, MetaEdit::Set(value)) {
                Ok(changed) => ("OK\n".to_string(), changed),
                // On the wire `meta set title ""` is a USAGE ERROR, never a
                // clear: clearing has its own explicit `meta unset` form.
                Err(MetaWriteError::Empty) => (
                    "ERR usage: meta set <title|description|icon|role|attention> <text...>\n".to_string(),
                    false,
                ),
                Err(MetaWriteError::ForbiddenFormatting) => (
                    format!(
                        "ERR {field} must be single-line and contain no control, bidi, or invisible formatting characters\n"
                    ),
                    false,
                ),
                Err(MetaWriteError::TooLong { cap }) => {
                    (format!("ERR {field} too long (max {cap} bytes)\n"), false)
                }
            }
        }
        Some("unset") => {
            let Some(field) = toks.next() else {
                return ("ERR usage: meta unset <field>\n".to_string(), false);
            };
            let Some(field) = MetaField::parse(field) else {
                return (
                    "ERR unknown meta field (title|description|icon|role|attention)\n".to_string(),
                    false,
                );
            };
            // A clear has no validation ladder to run, so it applies directly.
            let changed = apply_meta_value(ctx, field, None);
            ("OK\n".to_string(), changed)
        }
        Some(_) => (
            "ERR usage: meta [set <field> <text...> | unset <field>]\n".to_string(),
            false,
        ),
    }
}

/// The bare-`meta` status line. Locks are strictly SEQUENTIAL leaves: meta,
/// then term, then the store read — never nested, so this cannot deadlock
/// against the render path (meta before term there too) or the registry.
fn meta_status(
    term: &Arc<Mutex<Terminal>>,
    store: &Store,
    session: u64,
    ctx: &SessionCtx,
) -> String {
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let (title, cwd) = {
        use crate::cwd_native::ReportedCwd as _;
        let t = term_lock(term);
        (
            t.title().to_string(),
            // Same native conversion the `cwd` verb uses — `meta` reported the
            // engine's `/C:/Users//m6-an` URI path before, so the two verbs must
            // keep sharing one boundary or they will disagree.
            t.native_working_directory()
                .filter(|c| !c.is_empty())
                .map(|c| c.into_owned()),
        )
    };
    let state = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.by_local(session)
            .map_or_else(|| "-".to_string(), |h| h.state.as_str().to_string())
    };
    // A STORED value of exactly "-" must not read back as the unset sentinel:
    // pct_encode leaves '-' verbatim, so it is escaped here ("%2D") — any
    // percent-decoder recovers it, and `-` stays unambiguous as "unset".
    let opt = |v: Option<&str>| match v {
        None => "-".to_string(),
        Some("-") => "%2D".to_string(),
        Some(v) => pct_encode(v),
    };
    format!(
        "OK title={} user_title={} description={} icon={} role={} attention={} cwd={} state={state}\n",
        pct_encode(&title),
        opt(meta.user_title.as_deref()),
        opt(meta.description.as_deref()),
        opt(meta.icon.as_deref()),
        opt(meta.role.as_deref()),
        opt(meta.attention.as_deref()),
        opt(cwd.as_deref()),
    )
}

/// `timeline [<n>] [since=<id>]` -> the session's EVENT TIMELINE, oldest-first,
/// framed `OK <count>\n` + one `event <id> t=<ms> kind=<k> <payload>` line per
/// record — the lifecycle twin of `history` (same `<n>`/`since=` grammar, same
/// drop-oldest ring semantics). Payload values were pct-encoded at record time,
/// so every event is exactly one line.
pub(crate) fn cmd_timeline(ctx: &SessionCtx, rest: &str) -> String {
    let mut n = 0usize;
    let mut since: Option<u64> = None;
    for tok in rest.split_whitespace() {
        if let Some(v) = tok.strip_prefix("since=") {
            match v.parse::<u64>() {
                Ok(id) => since = Some(id),
                Err(_) => return "ERR usage: timeline [<n>] [since=<id>]\n".to_string(),
            }
        } else if let Ok(v) = tok.parse::<usize>() {
            n = v;
        } else {
            return "ERR usage: timeline [<n>] [since=<id>]\n".to_string();
        }
    }
    let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    let mut recs: Vec<&crate::session_timeline::TimelineEvent> = tl.since(since).collect();
    if n > 0 && recs.len() > n {
        recs.drain(..recs.len() - n);
    }
    let mut out = format!("OK {}\n", recs.len());
    for e in recs {
        out.push_str(&format!(
            "event {} t={} kind={} {}\n",
            e.id, e.t_ms, e.kind, e.payload
        ));
    }
    out
}

/// `cast` -> `OK <nbytes>\n` then the session's full asciicast v2 recording as
/// the body (design A.5.1 / B.7). The body is the JSON header line followed by
/// one `[t, "o", …]`/`[t, "r", …]` event per recorded burst — exactly what
/// `asciinema play -`/`agg` consume. `<nbytes>` is the byte length of the body
/// that follows (UTF-8), matching the read-verb framing so the existing client
/// can read the body without guessing where it ends. Output-only and bounded
/// (drop-oldest) by the recorder; this verb only serializes the snapshot, never
/// the renderer, so it is cheap and lock-disjoint from the PTY write path.
pub(crate) fn cmd_cast(ctx: &SessionCtx) -> String {
    let body = {
        let rec = ctx.cast.lock().unwrap_or_else(|p| p.into_inner());
        rec.to_asciicast()
    };
    format!("OK {}\n{}", body.len(), body)
}

/// `cast frames [count=N]` -> expand the compressed `cast` recording into `count`
/// evenly-spaced keyframe SCREENS (default 8) — the recording read as a "video"
/// FLIPBOOK an AI can watch: raw `cast` is the compact, sendable asciicast; this
/// is its readable expansion. Framed like `text` (`OK <n>\n` + n lines): each
/// frame is a `--- frame <k>/<N> @ <ms>ms ---` header then that instant's screen
/// rows. Frame instants are rebased to the first retained event, so a truncated
/// recording spreads frames across the surviving span (no blank leading frames).
/// A recording with no output yields `OK 0`. The events are snapshotted OUT of the
/// recorder lock (Arc refcount bumps) and folded ONCE forward with the lock
/// released, so this stays cheap and lock-disjoint from the reader's cast writer.
pub(crate) fn cmd_cast_frames(ctx: &SessionCtx, rest: &str) -> String {
    let mut count = 8usize;
    for tok in rest.split_whitespace() {
        match tok.strip_prefix("count=").map(|v| v.parse::<usize>()) {
            Some(Ok(n)) if n >= 1 => count = n.min(240),
            _ => return "ERR usage: cast frames [count=N]\n".to_string(),
        }
    }
    // Lift the events out of the lock (refcount bumps), THEN fold off-lock: the
    // O(events) VTE fold must never run under `ctx.cast`, which the reader's cast
    // writer thread contends per output burst (a long fold would stall it and
    // punch uncounted holes in the very recording being observed).
    let snapshot = {
        let rec = ctx.cast.lock().unwrap_or_else(|p| p.into_inner());
        rec.snapshot()
    };
    let frames = snapshot.fold_frames(count);
    let total = frames.len();
    let mut body = String::new();
    // DISCLOSE head-eviction, mirroring `to_asciicast`'s `aterm_truncated` header: if
    // the recording overflowed the RAM budget and drop-oldest evicted head events,
    // the leading engine state (SGR/alt-screen/scroll) is incomplete, so the early
    // frames may render wrong. Without this, a truncated flipbook reads as a faithful
    // full run — the same silent-misleading class the `cast` verb already closes.
    if snapshot.evicted() > 0 {
        body.push_str(&format!(
            "--- WARNING: {} head event(s) evicted (budget overflow); leading state incomplete, early frames may be wrong ---\n",
            snapshot.evicted()
        ));
    }
    for (k, (t, rows)) in frames.iter().enumerate() {
        body.push_str(&format!(
            "--- frame {}/{} @ {}ms ---\n",
            k + 1,
            total,
            t.as_millis()
        ));
        for row in rows {
            body.push_str(row);
            body.push('\n');
        }
    }
    format!("OK {}\n{body}", body.lines().count())
}

/// `temporal [tick]` -> `OK <nbytes>\n` then the session's screen RECONSTRUCTED at
/// logical `tick` (default: the latest recorded instant) — the read half of the
/// hydratable temporal spine (design Addendum B / B.9), the endpoint the
/// `TemporalRecorder` producer feeds. Replay seeds from the nearest retained
/// keyframe `<= tick` and folds the recorded `RawIn`/`Resize` events forward
/// through a fresh HEADLESS engine, then serializes its rows exactly as `text`
/// does (same [`super::visible_row`] path), so the body is faithful past-instant
/// screen text. Pure observer of the TARGET's own recorder (no renderer, no live
/// term lock), so it is correct cross-session like `cast`. `<tick>` is
/// MICROSECONDS since the session's recorder epoch; `temporal status` -> `OK
/// enabled=<bool> latest_tick=<n> keyframes=<n> live_events=<n> dropped_events=<n>`
/// reports the reachable window and whether recording is on, so a caller can pick a
/// valid tick without guessing. Two DISTINCT failures: `ERR temporal: recording
/// disabled …\n` when recording was never enabled (the default — a config fix),
/// versus `ERR temporal unreachable\n` when the base keyframe (or a needed input
/// blob) has aged out of the bounded retention window (honest partial reach, never
/// a wrong reconstruction). `<nbytes>` is the UTF-8 body length, matching the
/// read-verb framing so the existing client reads the body without guessing.
pub(crate) fn cmd_temporal(ctx: &SessionCtx, rest: &str) -> String {
    // `temporal status` -> the reachable window + whether recording is on, so a
    // caller can DISCOVER a valid tick (µs since session start) and tell an OFF
    // recorder from an aged-out one without guessing. Additive subform; the bare
    // `temporal` / `temporal <tick>` wire shapes are unchanged.
    if rest.trim() == "status" {
        let rec = ctx.temporal.lock().unwrap_or_else(|p| p.into_inner());
        let enabled = rec.total_events() > 0;
        return format!(
            "OK enabled={enabled} latest_tick={} keyframes={} live_events={} dropped_events={}\n",
            rec.latest_tick().0,
            rec.keyframe_count(),
            rec.live_events(),
            rec.dropped_events(),
        );
    }
    let at = match rest.trim() {
        "" => None,
        s => match s.parse::<u64>() {
            Ok(t) => Some(aterm_buffer::Ticks(t)),
            Err(_) => {
                return "ERR usage: temporal [status | <tick>]  (tick = µs since \
                        session start; `temporal status` reports the reachable range)\n"
                    .to_string();
            }
        },
    };
    // An enabled recorder ALWAYS seeds a t0 keyframe at spawn, so `total_events()
    // == 0` means recording was never turned on (the default) — a config fix, not
    // the inherent retention bound that `ERR temporal unreachable` names. Reporting
    // one error for both stranded first-contact callers.
    let (enabled, replay) = {
        let rec = ctx.temporal.lock().unwrap_or_else(|p| p.into_inner());
        let enabled = rec.total_events() > 0;
        let replay = if enabled {
            rec.replay_at(aterm_core::terminal::HostBindings::none(), at)
        } else {
            None
        };
        (enabled, replay)
    };
    if !enabled {
        return "ERR temporal: recording disabled (set temporal_recording=true in aterm.toml)\n"
            .to_string();
    }
    let Some(term) = replay else {
        return "ERR temporal unreachable\n".to_string();
    };
    let rows = term.rows() as usize;
    let mut body = String::new();
    for r in 0..rows {
        body.push_str(&super::visible_row(&term, r));
        body.push('\n');
    }
    format!("OK {}\n{}", body.len(), body)
}

/// `grant <src-id> <op>` -> mint an edge (src -> this session, op) and return its
/// bearer token hex. Owner-only (also enforced by the gate's catch-all Deny).
pub(crate) fn cmd_grant(ctx: &SessionCtx, scope: Scope, rest: &str) -> String {
    if scope != Scope::Owner {
        return "ERR denied\n".to_string();
    }
    let mut it = rest.split_whitespace();
    let (Some(src), Some(op_s)) = (it.next(), it.next()) else {
        return "ERR usage: grant <src-id> <op>\n".to_string();
    };
    let Some(op) = Op::parse(op_s) else {
        return "ERR unknown op\n".to_string();
    };
    let src = SessionId::new(src);
    let tok = {
        let mut edges = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
        edges.grant(src.clone(), ctx.self_id.clone(), op, ctx.nonce)
    };
    // Audit the act (§1.4#5) OFF the table lock: the logger sink has its own
    // mutex and must never nest inside `ctx.edges`.
    session_edge_audit::emit(EdgeAction::Grant, "wire", &src, &ctx.self_id, op.as_str());
    format!("OK {}\n", tok.to_hex())
}

/// `revoke <edge-hex>` -> remove one edge by its bearer token. `revoke
/// src=<sid>` -> sweep EVERY edge from that source (any op) and reply
/// `OK <removed>` — the wire dissolution primitive (design §1.4#4/§6): token
/// hexes are deliberately non-enumerable, so the sweep is the only way to
/// dissolve edges whose hex is lost. Owner-only; both forms fail closed with
/// `ERR no such edge` when nothing matches. Each successful act emits one
/// `session_edge` audit event (§1.4#5).
pub(crate) fn cmd_revoke(ctx: &SessionCtx, scope: Scope, rest: &str) -> String {
    if scope != Scope::Owner {
        return "ERR denied\n".to_string();
    }
    let rest = rest.trim();
    if let Some(src) = rest.strip_prefix("src=") {
        if src.is_empty() {
            return "ERR usage: revoke <edge-hex> | revoke src=<sid>\n".to_string();
        }
        let src = SessionId::new(src);
        let removed = {
            let mut edges = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
            edges.revoke_src(&src)
        };
        if removed == 0 {
            return "ERR no such edge\n".to_string();
        }
        // ONE event for the whole sweep: `op=*` (every op the source held).
        session_edge_audit::emit(EdgeAction::RevokeSrc, "wire", &src, &ctx.self_id, "*");
        return format!("OK {removed}\n");
    }
    let Some(tok) = EdgeToken::from_hex(rest) else {
        return "ERR bad token\n".to_string();
    };
    let removed = {
        let mut edges = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
        // Read the row's (src, dst, op) identity BEFORE removal — the audit
        // event describes the edge, and after `revoke` the row is gone.
        let row = edges.edge_of(&tok);
        if edges.revoke(&tok) { row } else { None }
    };
    let Some(edge) = removed else {
        return "ERR no such edge\n".to_string();
    };
    session_edge_audit::emit(EdgeAction::Revoke, "wire", &edge.src, &edge.dst, edge.op.as_str());
    "OK\n".to_string()
}

/// Parse the `dst=<sid> src=<sid> [kind=pull|push|both]` grammar shared by
/// `connect` and `disconnect`. `kind` is `None` when absent — each verb applies
/// its own default (`connect` ⇒ `both`, the §1.2 default connection;
/// `disconnect` ⇒ the whole connection). Any unknown token, empty value, or
/// missing endpoint is `Err` (the caller's usage string) — an authority-minting
/// argument gets no guessed default.
fn parse_connection_args(
    rest: &str,
) -> Result<(SessionId, SessionId, Option<ConnectionKind>), ()> {
    let (mut dst, mut src, mut kind) = (None, None, None);
    for tok in rest.split_whitespace() {
        if let Some(v) = tok.strip_prefix("dst=") {
            if v.is_empty() {
                return Err(());
            }
            dst = Some(SessionId::new(v));
        } else if let Some(v) = tok.strip_prefix("src=") {
            if v.is_empty() {
                return Err(());
            }
            src = Some(SessionId::new(v));
        } else if let Some(v) = tok.strip_prefix("kind=") {
            kind = Some(match v {
                "pull" => ConnectionKind::Pull,
                "push" => ConnectionKind::Push,
                "both" => ConnectionKind::Both,
                _ => return Err(()),
            });
        } else {
            return Err(());
        }
    }
    match (dst, src) {
        (Some(dst), Some(src)) => Ok((dst, src, kind)),
        _ => Err(()),
    }
}

/// `connect dst=<sid> src=<sid> [kind=pull|push|both]` against the process-wide
/// connection store. See [`cmd_connect_in`].
pub(crate) fn cmd_connect(store: &Store, scope: Scope, rest: &str) -> String {
    cmd_connect_in(&crate::connections::connections(), store, scope, rest)
}

/// The §6 `connect` verb: declaratively SET the session connection `src → dst`
/// to exactly `kind` (default `both`) through the [`crate::connections`] seam —
/// one atomic mint+revoke transition, audited on `session_edge` with origin
/// `wire`. Owner-only (also enforced by the dispatch's `OwnerOnly` table gate).
///
/// `dst` must be a LIVE registered session (its table + nonce are the mint
/// target); `src` may be ANY sid — the shipping `grant` contract, since the
/// source presents the token later and an unknown src simply never connects —
/// but a self-loop is refused (§1.5). Reply, caller-is-the-deliverer (the
/// `grant` principle, not its one-hex grammar): one Status line naming only the
/// LIVE ops — `OK read-screen=<hex> write-input=<hex> signal=<hex>`. A
/// declarative no-op (the connection already IS `kind`) replies the current
/// set the same way: the record's original tokens stay live, so re-deriving
/// the reply from the record covers both paths with no token churn.
pub(crate) fn cmd_connect_in(
    conn: &crate::connections::ConnectionStore,
    store: &Store,
    scope: Scope,
    rest: &str,
) -> String {
    if scope != Scope::Owner {
        return "ERR denied\n".to_string();
    }
    let Ok((dst, src, kind)) = parse_connection_args(rest) else {
        return "ERR usage: connect dst=<sid> src=<sid> [kind=pull|push|both]\n".to_string();
    };
    let kind = kind.unwrap_or(ConnectionKind::Both);
    if src == dst {
        return "ERR self-loop\n".to_string();
    }
    // Resolve the DESTINATION from the registry (clone-then-release: the ctx
    // Arc is lifted out before any table lock). A dead session's table would
    // mint rows its nonce already fails closed — refuse honestly instead.
    let dst_ctx = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        match g.by_sid(&dst) {
            None => return "ERR no such session\n".to_string(),
            Some(h) if matches!(h.state, crate::session_store::SessionState::Exited) => {
                return "ERR exited\n".to_string();
            }
            Some(h) => h.ctx.clone(),
        }
    };
    if !crate::connections::connect_in(
        conn,
        &src,
        &dst,
        &dst_ctx.edges,
        &dst_ctx.nonce,
        kind,
        "wire",
    ) {
        // `connect_in` refuses only a self-loop, checked above; keep the
        // fail-closed reply rather than an unreachable! on a future refusal.
        return "ERR self-loop\n".to_string();
    }
    let tokens = conn
        .records()
        .get(&(src, dst))
        .map(|r| r.tokens.clone())
        .unwrap_or_default();
    let mut out = String::from("OK");
    for (op, tok) in &tokens {
        out.push_str(&format!(" {}={}", op.as_str(), tok.to_hex()));
    }
    out.push('\n');
    out
}

/// `disconnect dst=<sid> src=<sid> [kind=…]` against the process-wide
/// connection store. See [`cmd_disconnect_in`].
pub(crate) fn cmd_disconnect(store: &Store, scope: Scope, rest: &str) -> String {
    cmd_disconnect_in(&crate::connections::connections(), store, scope, rest)
}

/// The §6 `disconnect` verb: dissolve the session connection `src → dst`,
/// KIND-FILTERED when `kind=` is given (`disconnect … kind=pull` revokes only
/// the pull half). Owner-only. Recorded connections dissolve pair-precisely by
/// held token; wire-granted rows with no record fall back to the op-filtered
/// source sweep — see [`crate::connections::disconnect_kind_in`]. Reply
/// `OK <revoked>`; an unknown pair (no record AND nothing swept) fails closed
/// with `ERR no such connection`. Unlike `connect`, an Exited-but-registered
/// dst is a legal target: dissolution is first-class (§1.4#4) and needs no
/// liveness.
pub(crate) fn cmd_disconnect_in(
    conn: &crate::connections::ConnectionStore,
    store: &Store,
    scope: Scope,
    rest: &str,
) -> String {
    if scope != Scope::Owner {
        return "ERR denied\n".to_string();
    }
    let Ok((dst, src, kind)) = parse_connection_args(rest) else {
        return "ERR usage: disconnect dst=<sid> src=<sid> [kind=pull|push|both]\n".to_string();
    };
    if src == dst {
        // A self-loop is never a connection; refusing here also keeps the
        // fallback sweep from eating raw self-granted rows by accident.
        return "ERR self-loop\n".to_string();
    }
    let dst_ctx = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        match g.by_sid(&dst) {
            Some(h) => h.ctx.clone(),
            None => return "ERR no such session\n".to_string(),
        }
    };
    match crate::connections::disconnect_kind_in(conn, &src, &dst, &dst_ctx.edges, kind, "wire") {
        Some(n) => format!("OK {n}\n"),
        None => "ERR no such connection\n".to_string(),
    }
}

/// `flows [--json]` -> the instance's AGGREGATED session-connection graph
/// (design §5.3/§6): every live edge row across EVERY registered session's
/// table, `OK <n>\n` + one `<src> <dst> <op>` line per row, sorted by
/// `(src, dst, op)`. Owner-only — the aggregated view discloses the whole
/// fabric, the same rationale as `sessions`. `--json` groups per directed pair
/// in the `cmd_edges_json` style: `{"flows":[{"src":..,"dst":..,"ops":[..]}]}`.
/// Collection reuses the [`crate::connections`] snapshot discipline (registry
/// snapshot first, each table locked briefly, no store lock held across a
/// table lock).
pub(crate) fn cmd_flows(store: &Store, scope: Scope, rest: &str) -> String {
    if scope != Scope::Owner {
        return "ERR denied\n".to_string();
    }
    let mut json = false;
    for tok in rest.split_whitespace() {
        match tok {
            "--json" | "json" => json = true,
            _ => return "ERR usage: flows [--json]\n".to_string(),
        }
    }
    let edges = crate::connections::all_edges(store);
    if json {
        // Grouped per (src, dst): the sort above makes pairs consecutive. The
        // op tokens are the closed `Op::as_str` vocabulary (no escaping
        // needed); sids go through `json_str_field` like `edges --json`.
        let mut items: Vec<String> = Vec::new();
        let mut i = 0;
        while i < edges.len() {
            let (src, dst) = (edges[i].src.clone(), edges[i].dst.clone());
            let mut ops: Vec<String> = Vec::new();
            while i < edges.len() && edges[i].src == src && edges[i].dst == dst {
                ops.push(format!("\"{}\"", edges[i].op.as_str()));
                i += 1;
            }
            items.push(format!(
                "{{{},{},\"ops\":[{}]}}",
                json_str_field("src", src.as_str()),
                json_str_field("dst", dst.as_str()),
                ops.join(","),
            ));
        }
        return json_ok(&format!("{{\"flows\":[{}]}}", items.join(",")));
    }
    let mut out = format!("OK {}\n", edges.len());
    for e in &edges {
        out.push_str(&format!(
            "{} {} {}\n",
            e.src.as_str(),
            e.dst.as_str(),
            e.op.as_str()
        ));
    }
    out
}

/// Resolve the `raise <sid>` argument against the registry — the pure,
/// headless-testable half of [`cmd_raise`] (the proxy hop cannot run off the
/// event loop; `pub(super)` for exactly that test). Exactly one sid token; an
/// unknown sid fails closed.
pub(super) fn raise_target(store: &Store, rest: &str) -> Result<u64, String> {
    let mut toks = rest.split_whitespace();
    let (Some(sid), None) = (toks.next(), toks.next()) else {
        return Err("ERR usage: raise <sid>\n".to_string());
    };
    let g = store.read().unwrap_or_else(|p| p.into_inner());
    match g.by_sid(&SessionId::new(sid)) {
        Some(h) => Ok(h.local_id),
        None => Err("ERR no such session\n".to_string()),
    }
}

/// `raise <sid>` -> raise the window hosting that session and select its tab —
/// the wire twin of the menu Show / map Enter (design §6; no shipping verb can
/// do this: `tab` drives only the front window, `focus` is a PTY focus event).
/// Owner-only. Main-thread hop like `spawn` (window/tab state is `App`-owned):
/// the event loop runs the `OperatorAction::Show` switch+focus shape for the
/// resolved session and reports whether any window hosts it.
pub(crate) fn cmd_raise(
    proxy: &EventLoopProxy<Wake>,
    store: &Store,
    scope: Scope,
    rest: &str,
) -> String {
    if scope != Scope::Owner {
        return "ERR denied\n".to_string();
    }
    let session = match raise_target(store, rest) {
        Ok(session) => session,
        Err(e) => return e,
    };
    match super::control_media::call_main(proxy, |reply| Wake::RaiseSession { session, reply }) {
        Ok(Ok(())) => "OK\n".to_string(),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// `whoami` -> report this session's fabric id + nonce + the connection's EFFECTIVE
/// scope against the session active RIGHT NOW. For an edge, the op is re-derived from
/// the presented token via `authorize` (the same per-request authority the gate uses)
/// rather than a cached connect-time op — so whoami can never over-report power the
/// token no longer holds after the ActiveHandle swung `@.` to a different session
/// (`edge unauthorized` when the token grants nothing against the now-active table).
pub(crate) fn cmd_whoami(ctx: &SessionCtx, scope: Scope) -> String {
    let s = match scope {
        Scope::Owner => "owner".to_string(),
        Scope::Edge(presented) => {
            let table = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
            match table.authorize(&presented, &ctx.self_id, &ctx.nonce) {
                Some(op) => format!("edge {}", op.as_str()),
                None => "edge unauthorized".to_string(),
            }
        }
    };
    format!("OK {} {} {}\n", ctx.self_id.as_str(), ctx.nonce.to_hex(), s)
}
