// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session-graph, drive, and capability-authority control verbs.

use std::sync::{Arc, Mutex};

use aterm_core::terminal::Terminal;
use aterm_session::{ConnectionKind, EdgeTable, EdgeToken, Op, SessionId};
use aterm_window::event_loop::EventLoopProxy;

use super::{Scope, json_ok, json_str_field, pct_encode};
use crate::Wake;
use crate::app_introspect::SessionWindowRow;
use crate::input::InputEvent;
use crate::session_edge_audit::{self, EdgeAction};
use crate::session_store::{ExitActor, SessionHandle, SessionState, Store};
use crate::session_timeline::{
    AUTOPILOT_USAGE, MetaEdit, MetaField, MetaWriteError, questions_usage, write_session_meta,
};
use crate::turn_ledger::TurnBy;
use crate::{SessionCtx, term_lock};

/// `sessions` -> list the process-wide registry: `OK <n>\n` then one line per session, sorted by local id: `<local> <sid> <parent|-> <state> <title> meta=<1|0> nonce=<hex32> window=<id|none|-> active=<1|0|-> wfocus=<1|0|-> detail=<pct|-> path=<frozen|live> program=<name|-> agent=<word|-> agent_detail=<pct|-> agent_rev=<n> agent_since_ms=<ms> agent_gen=<e.s|-> agent_fp=<hex16|-> program_state=<word[:kind]|-> … supervisor=<holder|-> supervisor_proven=<0|1> autopilot=<off|cruise|land|-> path_evidence=<measured:<ms>|unconfirmed:<ms>|carried|-> copy=<managed|foreign|-> upgrade=<-|<state>/<to>/<why>/<age>> land_at=<unix s|-> task=<open|landed|theirs|-> pause=<unix s|-> task_state=<working|done|exited|stalled|-> task_rev=<n>`.
pub(crate) fn cmd_sessions_store(store: &Store, proxy: Option<&EventLoopProxy<Wake>>) -> String {
    let (snapshot, frozen, posture) = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        let snapshot = g.snapshot();
        let frozen: std::collections::HashSet<u64> = snapshot
            .iter()
            .filter(|h| g.has_frozen_path(h.local_id))
            .map(|h| h.local_id)
            .collect();
        (snapshot, frozen, g.harness_posture())
    };
    let rows = if snapshot.is_empty() {
        Ok(Vec::new())
    } else {
        session_window_rows(proxy)
    };
    let rows = rows.as_deref().map_err(|_| "placement hop failed");
    sessions_lines(&snapshot, rows, posture, |id| frozen.contains(&id))
}

/// `sessions bridge`: the roster's local-id, sid and launch-nonce triples, without the placement hop or any per-session lock.
pub(crate) fn cmd_sessions_bridge(store: &Store) -> String {
    use std::fmt::Write as _;

    let rows = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .bridge_roster();
    let mut out = format!("OK {}\n", rows.len());
    for (local, sid, nonce) in rows {
        writeln!(
            &mut out,
            "{local} {} nonce={}",
            sid.as_str(),
            nonce.to_hex()
        )
        .expect("writing to String cannot fail");
    }
    out
}

const BRIDGE_STATUS_HOP_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// `sessions status`: a Lines-framed roster of the status fields the Fabric bridge consumes, agent verdict included, in one main-thread wake.
pub(crate) fn cmd_sessions_status(proxy: &EventLoopProxy<Wake>) -> String {
    match super::control_media::call_main_within(proxy, BRIDGE_STATUS_HOP_TIMEOUT, |reply| {
        Wake::ReadSessionStatuses { reply }
    }) {
        Ok(rows) => rows,
        Err(e) => format!("ERR {e}\n"),
    }
}

const PLACEMENT_HOP_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// The main-thread hop behind the roster's window columns, bounded by [`PLACEMENT_HOP_TIMEOUT`] so a busy main thread costs the window columns, never the listing.
pub(crate) fn session_window_rows(
    proxy: Option<&EventLoopProxy<Wake>>,
) -> Result<Vec<SessionWindowRow>, super::control_media::MainHopError> {
    let Some(proxy) = proxy else {
        return Err(super::control_media::MainHopError::Reason("no event loop"));
    };
    super::control_media::call_main_within(proxy, PLACEMENT_HOP_TIMEOUT, |reply| {
        Wake::ReadSessionWindows { reply }
    })
}

fn session_detail(h: &SessionHandle) -> Option<String> {
    match h.term.try_lock() {
        Ok(t) => crate::session_status::executing_detail(&t),
        Err(std::sync::TryLockError::Poisoned(p)) => {
            crate::session_status::executing_detail(&p.into_inner())
        }
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

fn roster_title(h: &SessionHandle) -> String {
    let user_title = h
        .ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .presentation_value("title");
    if let Some(title) = user_title {
        return title;
    }
    let live = |t: &Terminal| -> Option<String> {
        use crate::cwd_native::ReportedCwd as _;
        let title = crate::tab_label::without_console_program_path(t.title());
        if !title.is_empty() {
            return Some(title.to_owned());
        }
        t.native_working_directory()
            .filter(|cwd| !cwd.is_empty())
            .map(|cwd| crate::app_tabs::home_abbreviated(&cwd))
    };
    let resolved = match h.term.try_lock() {
        Ok(t) => live(&t),
        Err(std::sync::TryLockError::Poisoned(p)) => live(&p.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    };
    resolved.unwrap_or_else(|| {
        let stale = crate::tab_label::without_console_program_path(&h.title);
        if stale.is_empty() {
            "aterm".to_owned()
        } else {
            stale.to_owned()
        }
    })
}

/// The `sessions` wire body for `snapshot`: the store columns, then the placement columns from `rows` — or `-` for all three window columns when the rows could not be read (`Err`), with every line still printed.
pub(crate) fn sessions_lines(
    snapshot: &[SessionHandle],
    rows: Result<&[SessionWindowRow], &str>,
    posture: crate::autopilot::HarnessPosture,
    frozen_path: impl Fn(u64) -> bool,
) -> String {
    use std::fmt::Write as _;
    let mut out = format!("OK {}\n", snapshot.len());
    for h in snapshot {
        let parent = h
            .parent
            .as_ref()
            .map_or("-", aterm_session::SessionId::as_str);
        let title = pct_encode(&roster_title(h));
        let (
            has_meta,
            supervisor,
            role,
            attention,
            user_title,
            word,
            land_at,
            (claimed_here, proven, shown_proven),
        ) = {
            let meta = h.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            let opt = |v: Option<&str>| v.map_or_else(|| "-".to_string(), pct_encode);
            let now = crate::metrics::now_us();
            let live = meta.live_supervisor(now);
            (
                u8::from(meta.any_set()),
                opt(live),
                opt(meta.role.as_deref()),
                opt(meta.attention.as_deref()),
                opt(meta.user_title.as_deref()),
                meta.autopilot_word(),
                meta.land_at,
                (
                    live == Some(crate::autopilot::own_holder()),
                    meta.supervisor_proven(now),
                    meta.claim_proven(now),
                ),
            )
        };
        let flag = |set: bool| if set { "1" } else { "0" };
        let (window, active, wfocus) = match rows {
            Ok(rows) => {
                let row = rows.iter().find(|r| r.local == h.local_id);
                (
                    row.map_or_else(|| "none".to_string(), |r| r.window.to_string()),
                    flag(row.is_some_and(|r| r.active_tab)),
                    flag(row.is_some_and(|r| r.window_focused)),
                )
            }
            Err(_) => ("-".to_string(), "-", "-"),
        };
        let detail = session_detail(h)
            .as_deref()
            .map_or_else(|| "-".to_string(), pct_encode);
        let (
            agent,
            program_state,
            (path, owner),
            task_record,
            (supervised_agent, group),
            face,
            pause,
        ) = {
            let timeline = h.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            (
                timeline.agent().wire_fields(),
                crate::program_state::column(timeline.program_shown()),
                timeline.owner_columns(frozen_path(h.local_id), crate::upgrade_host::now_s()),
                crate::task_record::status_tokens(timeline.task()),
                (
                    crate::harness_host::agent_of(timeline.agent()),
                    timeline.agent().program_pgid,
                ),
                timeline.task_face().map(|(face, _)| face),
                timeline.pause(),
            )
        };
        let supervised_agent = crate::harness_host::admitted_as(&h.ctx, supervised_agent, group);
        let autopilot = crate::autopilot::wire_field(posture, word, supervised_agent);
        let land_at =
            crate::autopilot::land_at_field(land_at.filter(|_| supervised_agent.is_some()));
        let task = crate::autopilot::task_field(face, claimed_here || proven, supervised_agent);
        let pause = crate::autopilot::pause_field(pause, claimed_here, supervised_agent);
        let human = h.ctx.human_input.wire(crate::metrics::now_us());
        let _ = writeln!(
            out,
            "{} {} {} {} {} meta={has_meta} nonce={nonce} window={window} active={active} \
             wfocus={wfocus} detail={detail} path={path} {agent} program_state={program_state} \
             human_ms={human} role={role} attention={attention} user_title={user_title} \
             supervisor={supervisor} supervisor_proven={proven} autopilot={autopilot} {owner} \
             land_at={land_at} task={task} pause={pause} {task_record}",
            h.local_id,
            h.sid.as_str(),
            parent,
            h.state.as_str(),
            title,
            nonce = h.nonce.to_hex(),
            proven = flag(shown_proven),
        );
    }
    out
}

/// `who` -> the presence readout for the whole instance: one line per session naming who is driving it and how many peers are watching it, so a human or an orchestrator can always see the hand and the eye on a fleet.
pub(crate) fn cmd_who(store: &Store) -> String {
    let snapshot = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.snapshot()
    };
    let foreground_groups: Vec<i32> = snapshot
        .iter()
        .map(|h| crate::quit_safety::foreground_pgrp(h.master))
        .collect();
    let mut out = format!("OK {}\n", snapshot.len());
    for (h, fgpgid) in snapshot.iter().zip(foreground_groups) {
        let fgpgid = if fgpgid > 0 {
            fgpgid.to_string()
        } else {
            "-".to_string()
        };
        let driving = h
            .ctx
            .turn_lease
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|l| l.driving_token(crate::metrics::now_us()))
            .unwrap_or_else(|| "-".to_string());
        let watchers = h.ctx.watchers.count();
        let turns = h.ctx.turns.lock().unwrap_or_else(|p| p.into_inner()).len();
        out.push_str(&format!(
            "{} {} driving={driving} watchers={watchers} turns={turns} {} nonce={nonce} fgpgid={fgpgid}\n",
            h.local_id,
            h.sid.as_str(),
            h.state.as_str(),
            nonce = h.nonce.to_hex(),
        ));
    }
    out
}

static NEXT_LEASE_TAG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

const LEASE_TTL_DEFAULT_MS: u64 = 30_000;
const LEASE_TTL_MAX_MS: u64 = 600_000;

/// `lease [status] | lease acquire [ttl=<ms>] [holder=<name>] [hard] [here] | lease release [holder=<name>] [force] [here]` — the explicit drive lease.
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

fn lease_status(ctx: &SessionCtx) -> String {
    let now = crate::metrics::now_us();
    let lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
    match lease.as_ref() {
        Some(crate::Lease::Turn { id, .. }) => format!("OK lease turn={id}\n"),
        Some(crate::Lease::Drive {
            holder,
            expires_us,
            hard,
            holder_pid,
            ..
        }) if *expires_us > now => {
            format!(
                "OK lease holder={holder} expires_in_ms={}{}{}\n",
                (*expires_us - now) / 1000,
                if *hard { " hard=1" } else { "" },
                holder_pid.map_or_else(String::new, |pid| format!(" pid={pid}"))
            )
        }
        _ => "OK lease none\n".to_string(),
    }
}

fn lease_acquire<'a>(ctx: &SessionCtx, args: impl Iterator<Item = &'a str>) -> String {
    let mut ttl_ms = LEASE_TTL_DEFAULT_MS;
    let mut holder: Option<String> = None;
    let mut hard = false;
    let mut here = false;
    for t in args {
        if let Some(v) = t.strip_prefix("ttl=") {
            match v.parse::<u64>() {
                Ok(n) if n >= 1 => ttl_ms = n.min(LEASE_TTL_MAX_MS),
                _ => {
                    return format!(
                        "ERR lease acquire: bad ttl '{t}' (a positive ms count, capped at \
                         {LEASE_TTL_MAX_MS})\n"
                    );
                }
            }
        } else if let Some(v) = t.strip_prefix("holder=") {
            if v.is_empty() || v.len() > 64 || !v.chars().all(|c| c.is_ascii_graphic()) {
                return "ERR lease acquire: holder must be 1..=64 printable ASCII chars\n"
                    .to_string();
            }
            holder = Some(v.to_string());
        } else if t == "hard" {
            hard = true;
        } else if t == "here" {
            here = true;
        } else {
            return format!(
                "ERR lease acquire: unknown arg '{t}' (ttl=<ms> holder=<name> hard here)\n"
            );
        }
    }
    let holder = holder.unwrap_or_else(|| {
        let n = NEXT_LEASE_TAG.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        format!("drv-{n}")
    });
    let now = crate::metrics::now_us();
    let asking = super::serving_connection();
    let mut lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(held) = lease.as_ref().filter(|h| h.is_live(now)) {
        match held {
            crate::Lease::Turn { id, .. } => return format!("ERR busy turn={id}\n"),
            crate::Lease::Drive {
                holder: h,
                expires_us,
                conn,
                ..
            } if *h != holder || (here && *conn != asking) => {
                return format!(
                    "ERR lease held holder={h} expires_in_ms={}\n",
                    (*expires_us - now) / 1000
                );
            }
            crate::Lease::Drive { .. } => {}
        }
    }
    *lease = Some(crate::Lease::Drive {
        holder: holder.clone(),
        expires_us: now + ttl_ms.saturating_mul(1000),
        conn: asking,
        hard,
        here,
        holder_pid: super::serving_peer_pid(),
    });
    drop(lease);
    if here && asking.is_some() {
        super::note_connection_claim();
    }
    crate::presence::post_lease_changed(&ctx.self_id);
    format!(
        "OK lease acquired holder={holder} ttl_ms={ttl_ms} expires_in_ms={ttl_ms}{}\n",
        if hard { " hard=1" } else { "" }
    )
}

fn lease_release<'a>(ctx: &SessionCtx, args: impl Iterator<Item = &'a str>) -> String {
    let mut holder: Option<String> = None;
    let mut force = false;
    let mut here = false;
    for t in args {
        if let Some(v) = t.strip_prefix("holder=") {
            holder = Some(v.to_string());
        } else if t == "force" {
            force = true;
        } else if t == "here" {
            here = true;
        } else {
            return format!("ERR lease release: unknown arg '{t}' (holder=<name> force here)\n");
        }
    }
    enum Act {
        None,
        Release(Option<aterm_session::SessionId>),
        Refuse(String),
    }
    let now = crate::metrics::now_us();
    let mut lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
    let act = match lease.as_ref() {
        None => Act::None,
        Some(crate::Lease::Turn { id, driver, .. }) => {
            if force {
                Act::Release(driver.clone())
            } else {
                Act::Refuse(format!(
                    "ERR busy turn={id} (a turn releases its own lease; use `lease release force` to preempt a wedged turn)\n"
                ))
            }
        }
        Some(crate::Lease::Drive {
            holder: h,
            expires_us,
            conn,
            ..
        }) => {
            let live = *expires_us > now;
            let named = holder.as_deref() == Some(h.as_str());
            if !live || force || (named && (!here || *conn == super::serving_connection())) {
                Act::Release(None)
            } else if named {
                Act::Refuse(format!("ERR lease held by {h} over another connection\n"))
            } else {
                Act::Refuse(format!(
                    "ERR lease held by {h} (pass holder={h} or force)\n"
                ))
            }
        }
    };
    match act {
        Act::None => "OK lease none\n".to_string(),
        Act::Release(driver) => {
            *lease = None;
            drop(lease);
            lease_freed(ctx);
            if let Some(driver) = driver {
                crate::presence::post_lease_changed(&driver);
            }
            "OK lease released\n".to_string()
        }
        Act::Refuse(e) => e,
    }
}

fn lease_freed(ctx: &SessionCtx) {
    ctx.turns
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take_typed();
    crate::presence::post_lease_changed(&ctx.self_id);
}

/// CONNECTION `conn` ENDED: the drive lease it took `here` ([`crate::Lease::Drive`]'s `here`) ends with it, live or lapsed, as [`lease_release`] frees it.
pub(crate) fn release_connection_lease(ctx: &SessionCtx, conn: u64) {
    let mut lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
    if !matches!(lease.as_ref(), Some(crate::Lease::Drive { conn: Some(held_by), here: true, .. })
        if *held_by == conn)
    {
        return;
    }
    *lease = None;
    drop(lease);
    lease_freed(ctx);
}

/// `edges` / `grants` -> this session's inbound capability edges (its [`aterm_session::EdgeTable`]): `OK <n>` then `<src> <dst> <op>` per edge, sorted by `(src, op)`; `<dst>` is always this session.
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

/// `edges --json` / `grants --json` -> `{"edges":[{"src":"..","dst":"..","op":".."}], "dst":"<self>"}`: the edges [`cmd_edges`] lists, as one object.
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

/// `family [<sid>]` -> the target's place in the session hierarchy, from the registry's `parent` links.
pub(crate) fn cmd_family(ctx: &SessionCtx, store: &Store, scope: Scope, rest: &str) -> String {
    let target_sid = match rest.trim() {
        "" => ctx.self_id.clone(),
        s => SessionId::new(s),
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
            pct_encode(&roster_title(h)),
        )
    };
    let mut body = String::new();
    body.push_str(&line("self", node));
    match node.parent.as_ref() {
        Some(psid) => match snapshot.iter().find(|h| h.sid == *psid) {
            Some(ph) => body.push_str(&line("parent", ph)),
            None => body.push_str(&format!("parent {} unknown -\n", psid.as_str())),
        },
        None => body.push_str("parent - - -\n"),
    }
    for h in snapshot
        .iter()
        .filter(|h| h.parent.as_ref() == Some(&target_sid))
    {
        body.push_str(&line("child", h));
    }
    if scope.is_owner_class() {
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

const PARKED_POLL: std::time::Duration = std::time::Duration::from_millis(50);

fn fire_idle_unless_parked(t: &mut Terminal, now: std::time::Instant, parked: bool) {
    if parked {
        t.watch_restart_idle(now);
    } else {
        t.watch_expire(now);
    }
}

/// The exit check every parked verb shares: whether `session` was in the registry is read here, at arm, and the returned probe answers "has it ended?" against the store it is handed.
pub(crate) fn exit_probe(store: &Store, session: u64) -> impl Fn(&Store) -> bool {
    let was_registered = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_local(session)
        .is_some();
    move |store: &Store| -> bool {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        match g.by_local(session).map(|h| h.state) {
            Some(SessionState::Exited) => true,
            None => was_registered,
            _ => false,
        }
    }
}

/// `ready [timeout_ms]` -> block until the target session is alive and idle, then `OK ready <reason>\n`; `OK timeout\n` if it does not become ready in time (default 30 000 ms, capped at 600 000); `ERR exited\n` if the session has exited (it will never become ready).
pub(crate) fn cmd_ready(
    term: &Arc<Mutex<Terminal>>,
    store: &Store,
    session: u64,
    ctx: &SessionCtx,
    rest: &str,
) -> String {
    use std::time::{Duration, Instant};

    use aterm_core::terminal::{BlockState, WatcherSpec};

    let timeout_ms = match rest.trim() {
        "" => aterm_control::wire::WAIT_DEFAULT_MS,
        t => match aterm_control::wire::wait_ms(t.strip_prefix("timeout=").unwrap_or(t)) {
            Some(ms) => ms,
            None => return "ERR usage: ready [<ms>|timeout=<ms>]\n".to_string(),
        },
    };
    let now0 = Instant::now();
    let deadline = now0 + Duration::from_millis(timeout_ms);
    const SETTLE: Duration = Duration::from_millis(60);

    let gone = exit_probe(store, session);

    let idle_id = term_lock(term).watch(WatcherSpec::IdleFor { dur: SETTLE }, now0);
    let sub = crate::session_watch::Subscription::single(&ctx.watchers);
    let exited = || gone(store);
    let park = Park::new(
        &sub,
        &exited,
        OnHangup::Abort,
        Stretch::Hold(ctx.update_parked.as_ref()),
    );
    let look = |now: Instant, parked: bool, closing: bool| {
        let mut t = term_lock(term);
        // The shipped integrations mark `133;B` once their prompt is drawn, so a resting prompt
        // is `EnteringCommand`, with or without a line typed at it.
        let prompt = matches!(
            t.all_blocks().last().map(|b| b.state),
            Some(BlockState::PromptOnly | BlockState::EnteringCommand | BlockState::Complete)
        );
        fire_idle_unless_parked(&mut t, now, parked);
        let settled = idle_id.and_then(|id| t.watch_poll(id)).is_some();
        let ready = if parked {
            None
        } else if prompt {
            Some("OK ready prompt\n")
        } else if settled {
            Some("OK ready idle\n")
        } else {
            None
        };
        if (ready.is_some() || closing)
            && let Some(id) = idle_id
        {
            t.watch_disarm(id);
        }
        (ready, t.watch_next_deadline())
    };
    let disarm = || {
        if let Some(id) = idle_id {
            term_lock(term).watch_disarm(id);
        }
    };
    match park.run(deadline, look, disarm) {
        Parked::Done(ready) => ready.to_string(),
        Parked::Deadline => "OK timeout\n".to_string(),
        Parked::Exited => "ERR exited\n".to_string(),
        Parked::HungUp => crate::control::HUNG_UP_REPLY.to_string(),
    }
}

/// `await <idle <ms> | seq [<n>] | match <re> [rows <a> <b>] | gone <re> [rows <a> <b>]`
pub(crate) fn cmd_await(
    term: &Arc<Mutex<Terminal>>,
    store: &Store,
    session: u64,
    ctx: &SessionCtx,
    rest: &str,
) -> String {
    use std::time::{Duration, Instant};

    use aterm_core::terminal::{RowRange, WatcherSpec};

    const USAGE: &str = "ERR usage: await idle <ms> | seq [<n>] | match <re> [rows <a> <b>] | \
                         gone <re> [rows <a> <b>] | block | frame | inbox since=<id>|re=<off> \
                         [kinds=<k|hold,...>] [topic=<t>] | \
                         agent <word>[,<word>...] [since=<rev>] | \
                         task [@<sel>[:<rev>[:<fact_rev>]][,...]] [any|all] [since=<rev>] \
                         [facts=1 [fact_since=<fact_rev>]] | \
                         log <need|open=<lvl>|close=<lvl>|kind=<k>>[,...] [since=<seq> ep=<e>] | \
                         sys <predicate> | \
                         edit ticket=<t>|uri=<file-uri> | consent [kind=debugger] | \
                         momentum <floor 0..=1>; each takes [timeout=<ms>]\n";

    let mut toks = rest.split_whitespace();
    let mut timeout_ms = aterm_control::wire::WAIT_DEFAULT_MS;
    let mut args: Vec<&str> = Vec::new();
    while let Some(tok) = toks.next() {
        let value = if let Some(v) = tok.strip_prefix("timeout=") {
            v
        } else if tok == "timeout"
            && let Some(v) = toks.next()
        {
            v
        } else {
            args.push(tok);
            continue;
        };
        let Some(ms) = aterm_control::wire::wait_ms(value) else {
            return USAGE.to_string();
        };
        timeout_ms = ms;
    }
    let Some(&kind) = args.first() else {
        return USAGE.to_string();
    };

    if kind == "inbox" {
        return crate::fabric::cmd_await_inbox(ctx, &args[1..], timeout_ms);
    }

    if kind == "agent" {
        let Some(words) = args.get(1).and_then(|w| parse_agent_words(w)) else {
            return USAGE.to_string();
        };
        let mut since = None;
        for arg in &args[2..] {
            if let Some(v) = arg.strip_prefix("since=") {
                let Ok(rev) = v.parse::<u64>() else {
                    return USAGE.to_string();
                };
                since = Some(rev);
            }
        }
        return await_agent(store, session, ctx, &words, since, timeout_ms);
    }

    if kind == "task" {
        if rest.split_whitespace().next() != Some("task") {
            return crate::task_record::AWAIT_TASK_USAGE.to_string();
        }
        let parsed = match crate::task_record::parse_await_task(&args[1..], false) {
            Ok(parsed) => parsed,
            Err(usage) => return usage,
        };
        let target = TaskTarget {
            local_id: session,
            ctx,
            since: parsed.since,
            facts: parsed.facts,
            fact_since: parsed.fact_since,
        };
        return await_task(store, &[target], parsed.combine, timeout_ms);
    }

    if kind == "log" {
        return match crate::session_log::LogAwait::parse(&args[1..]) {
            Ok(wait) => await_log(store, session, ctx, wait, timeout_ms),
            Err(usage) => usage.to_string(),
        };
    }

    let now0 = Instant::now();
    let armed = if kind == "match" || kind == "gone" {
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
        let matcher = match aterm_observe::row_matcher(pat) {
            Ok(m) => m,
            Err(_) => return "ERR badregex\n".to_string(),
        };
        let mut t = term_lock(term);
        let grid_rows = t.rows() as usize;
        if !range.covers_any(grid_rows) {
            return super::control_query::rows_off_grid(grid_rows);
        }
        if kind == "gone" {
            t.watch_rows_gone(matcher, range, now0)
        } else {
            t.watch_rows(matcher, range, now0)
        }
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
                let current = t.content_seq();
                let after = args
                    .get(1)
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(current);
                if after > current {
                    return format!("OK seq {current}\n");
                }
                t.watch(WatcherSpec::SeqAdvanced { after }, now0)
            }
            "block" => {
                let after = t.completed_command_seq();
                t.watch(WatcherSpec::BlockComplete { after }, now0)
            }
            "frame" => t.watch(WatcherSpec::FrameWhole, now0),
            _ => return USAGE.to_string(),
        }
    };
    let Some(id) = armed else {
        return format!("ERR {WAITS_FULL}\n");
    };

    let overall = now0 + Duration::from_millis(timeout_ms);

    let sub = crate::session_watch::Subscription::single(&ctx.watchers);

    let probe = exit_probe(store, session);
    let exited = || probe(store);
    let park = Park::new(
        &sub,
        &exited,
        OnHangup::Abort,
        Stretch::Hold(ctx.update_parked.as_ref()),
    );
    match park.run(overall, watcher_look(term, id), disarm_watch(term, id)) {
        Parked::Done(seq) => format!("OK {kind} {seq}\n"),
        Parked::Deadline => "OK timeout\n".to_string(),
        Parked::Exited => "ERR exited\n".to_string(),
        Parked::HungUp => crate::control::HUNG_UP_REPLY.to_string(),
    }
}

const AGENT_WORDS: [&str; 7] = [
    "busy", "prompt", "question", "idle", "survey", "unknown", "-",
];

#[derive(Clone, Debug, PartialEq, Eq)]
enum AgentWord {
    Verdict(&'static str),
    Wall(Option<String>),
}

impl AgentWord {
    fn matches(&self, word: &str) -> bool {
        match self {
            Self::Verdict(w) => *w == word,
            Self::Wall(None) => word.starts_with("wall:"),
            Self::Wall(Some(kind)) => word.strip_prefix("wall:") == Some(kind.as_str()),
        }
    }
}

fn parse_agent_words(list: &str) -> Option<Vec<AgentWord>> {
    let mut out = Vec::new();
    for w in list.split(',') {
        let word = if w == "wall" {
            AgentWord::Wall(None)
        } else if let Some(kind) = w.strip_prefix("wall:") {
            let ok = (1..=32).contains(&kind.len())
                && kind.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-'
                });
            if !ok {
                return None;
            }
            AgentWord::Wall(Some(kind.to_string()))
        } else {
            AgentWord::Verdict(AGENT_WORDS.iter().find(|k| **k == w)?)
        };
        out.push(word);
    }
    (!out.is_empty()).then_some(out)
}

fn await_agent(
    store: &Store,
    session: u64,
    ctx: &SessionCtx,
    words: &[AgentWord],
    since: Option<u64>,
    timeout_ms: u64,
) -> String {
    use std::time::{Duration, Instant};

    let overall = Instant::now() + Duration::from_millis(timeout_ms);
    let sub = crate::session_watch::Subscription::single(&ctx.watchers);
    let exited = exit_probe(store, session);
    let in_the_look = || false;
    let park = Park::new(&sub, &in_the_look, OnHangup::Abort, Stretch::None);
    let look = |_: Instant, _: bool, _: bool| {
        let (word, rev) = {
            let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            (tl.agent().word, tl.agent().rev)
        };
        let answer =
            if since.is_none_or(|since| rev > since) && words.iter().any(|w| w.matches(word)) {
                Some(format!("OK agent {word} rev={rev}\n"))
            } else {
                exited(store).then(|| "ERR exited\n".to_string())
            };
        (answer, None)
    };
    match park.run(overall, look, || {}) {
        Parked::Done(answer) => answer,
        Parked::Deadline => "OK timeout\n".to_string(),
        Parked::Exited => "ERR exited\n".to_string(),
        Parked::HungUp => crate::control::HUNG_UP_REPLY.to_string(),
    }
}

/// One target of an `await task` ([`await_task`]): the session, and the task rev it waits past — its own `@<sel>:<rev>` or the request's `since=`, else (`None`) its rev when the wait is armed — and, with `facts=1` (`facts`), the fact revision it waits past likewise (`@<sel>:<rev>:<fact_rev>` or `fact_since=`, else its `fact_rev` when the wait is armed).
pub(crate) struct TaskTarget<'a> {
    pub(crate) local_id: u64,
    pub(crate) ctx: &'a SessionCtx,
    pub(crate) since: Option<u64>,
    pub(crate) facts: bool,
    pub(crate) fact_since: Option<u64>,
}

/// `await task`: park until the task records of `targets` have news — a target whose record's rev is past its `since` with a state to read (`done`, `exited`, `stalled`) or the supervisor's attention raised ([`crate::task_record::TaskRecord::has_news`]), a `facts=1` target whose fact word was noted past its fact edge (a clear is none), or whose session closed — at the first such target (`any`) or once every target is one (`all`). A fact edit never moves `rev`, so a wait without `facts=1` never wakes on one.
pub(crate) fn await_task(
    store: &Store,
    targets: &[TaskTarget<'_>],
    combine: crate::task_record::Combine,
    timeout_ms: u64,
) -> String {
    use std::time::{Duration, Instant};

    use crate::task_record::Combine;

    let overall = Instant::now() + Duration::from_millis(timeout_ms);
    let mut sub = crate::session_watch::Subscription::new();
    for target in targets {
        let _ = sub.watch(&target.ctx.watchers);
    }
    let edges: Vec<(u64, Option<u64>)> = targets
        .iter()
        .map(|t| {
            let record = task_record_of(t.ctx);
            let rev = t
                .since
                .unwrap_or_else(|| record.as_ref().map_or(0, |task| task.rev));
            let fact = t.facts.then(|| {
                t.fact_since
                    .unwrap_or_else(|| record.as_ref().map_or(0, |task| task.fact_rev))
            });
            (rev, fact)
        })
        .collect();
    let exits: Vec<_> = targets
        .iter()
        .map(|t| exit_probe(store, t.local_id))
        .collect();
    let mut latched: Vec<Option<String>> = vec![None; targets.len()];
    let in_the_look = || false;
    let park = Park::new(&sub, &in_the_look, OnHangup::Abort, Stretch::None);
    let look = |_: Instant, _: bool, _: bool| {
        latch_task_news(store, targets, &edges, &exits, &mut latched);
        let with_news = latched.iter().filter(|latch| latch.is_some()).count();
        let answered = match combine {
            Combine::Any => with_news > 0,
            Combine::All => with_news == targets.len(),
        };
        (answered.then_some(()), None)
    };
    let tail = match park.run(overall, look, || {}) {
        Parked::Done(()) => "",
        Parked::Deadline | Parked::Exited => " status=timeout",
        Parked::HungUp => return crate::control::HUNG_UP_REPLY.to_string(),
    };
    let mut out = format!("OK {}{tail}\n", targets.len());
    for ((target, exited), latch) in targets.iter().zip(&exits).zip(latched) {
        let row = latch.unwrap_or_else(|| {
            task_wait_row(
                target,
                task_record_of(target.ctx).as_ref(),
                exited(store),
                false,
            )
        });
        out.push_str(&row);
        out.push('\n');
    }
    out
}

fn task_record_of(ctx: &SessionCtx) -> Option<crate::task_record::TaskRecord> {
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .task()
        .cloned()
}

/// `task_news` belongs to this request's qualifying latch, not its latest record
/// level. It preserves partial all-wait news across client legs, including the
/// initial leg whose armed rev/fact edges the client has not read yet.
fn task_wait_row(
    target: &TaskTarget<'_>,
    record: Option<&crate::task_record::TaskRecord>,
    closed: bool,
    news: bool,
) -> String {
    let (agent, program, attention) = crate::task_record::agent_program_and_attention(target.ctx);
    let mut row = crate::task_record::await_row(
        target.ctx.self_id.as_str(),
        record,
        agent,
        &program,
        &attention,
        closed,
    );
    row.push_str(&format!(" task_news={}", u8::from(news)));
    row
}

fn latch_task_news<F: Fn(&Store) -> bool>(
    store: &Store,
    targets: &[TaskTarget<'_>],
    edges: &[(u64, Option<u64>)],
    exits: &[F],
    latched: &mut [Option<String>],
) {
    for (((target, edge), exited), latch) in targets.iter().zip(edges).zip(exits).zip(latched) {
        if latch.is_some() {
            continue;
        }
        let closed = exited(store);
        let news = {
            let timeline = target
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let (rev_edge, fact_edge) = *edge;
            timeline
                .task()
                .filter(|task| {
                    (task.rev > rev_edge && task.has_news())
                        || fact_edge.is_some_and(|edge| task.fact_rev > edge && task.fact.is_some())
                })
                .cloned()
        };
        if closed || news.is_some() {
            let record = news.or_else(|| task_record_of(target.ctx));
            *latch = Some(task_wait_row(target, record.as_ref(), closed, true));
        }
    }
}

fn await_log(
    store: &Store,
    session: u64,
    ctx: &SessionCtx,
    wait: crate::session_log::LogAwait,
    timeout_ms: u64,
) -> String {
    await_log_seamed(store, session, ctx, wait, timeout_ms, || {})
}

/// [`await_log`], running `after_look` the instant after each look it takes at an ended session and before it decides (production passes nothing): the seam a test lands a retire on, to pin that the look is the waiter's only observation.
pub(crate) fn await_log_seamed(
    store: &Store,
    session: u64,
    ctx: &SessionCtx,
    wait: crate::session_log::LogAwait,
    timeout_ms: u64,
    mut after_look: impl FnMut(),
) -> String {
    use std::time::{Duration, Instant};

    use crate::session_log::LogAwaitStep;

    let overall = Instant::now() + Duration::from_millis(timeout_ms);
    let sub = crate::session_watch::Subscription::single(&ctx.watchers);
    let exited = exit_probe(store, session);
    let mut cursor = match wait.since {
        Some(since) => since,
        None => match ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .slog()
            .await_start(wait.what)
        {
            LogAwaitStep::Answer(reply) => return reply,
            LogAwaitStep::Wait(head) => head,
        },
    };
    let in_the_look = || false;
    let park = Park::new(&sub, &in_the_look, OnHangup::Abort, Stretch::None);
    let look = |_: Instant, _: bool, _: bool| {
        let step = ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .slog()
            .await_check(wait.what, cursor);
        match step {
            LogAwaitStep::Answer(reply) => return (Some(reply), None),
            LogAwaitStep::Wait(next) => cursor = next,
        }
        if exited(store) {
            let (step, due) = ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .slog()
                .await_look(wait.what, cursor);
            after_look();
            match step {
                LogAwaitStep::Answer(reply) => return (Some(reply), None),
                LogAwaitStep::Wait(next) => cursor = next,
            }
            if !due {
                return (Some("ERR exited\n".to_string()), None);
            }
        }
        (None, None)
    };
    match park.run(overall, look, || {}) {
        Parked::Done(reply) => reply,
        Parked::Deadline => "OK timeout\n".to_string(),
        Parked::Exited => "ERR exited\n".to_string(),
        Parked::HungUp => crate::control::HUNG_UP_REPLY.to_string(),
    }
}

static NEXT_TURN_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn turn_id_count() -> &'static std::sync::atomic::AtomicU64 {
    #[cfg(test)]
    if let Some(own) = OWN_TURN_IDS.get() {
        return own;
    }
    &NEXT_TURN_ID
}

#[cfg(test)]
thread_local! {
    static OWN_TURN_IDS: std::cell::Cell<Option<&'static std::sync::atomic::AtomicU64>> =
        const { std::cell::Cell::new(None) };
}

/// TESTS ONLY: this thread mints, reads and raises turn ids on a count of its own, from 0, until the guard drops; no other test's moves it, and it moves no other test's.
#[cfg(test)]
pub(crate) fn own_turn_ids() -> OwnTurnIds {
    let own: &'static std::sync::atomic::AtomicU64 =
        Box::leak(Box::new(std::sync::atomic::AtomicU64::new(0)));
    OwnTurnIds {
        previous: OWN_TURN_IDS.replace(Some(own)),
    }
}

/// TESTS ONLY: the guard [`own_turn_ids`] returns.
#[cfg(test)]
pub(crate) struct OwnTurnIds {
    previous: Option<&'static std::sync::atomic::AtomicU64>,
}

#[cfg(test)]
impl Drop for OwnTurnIds {
    fn drop(&mut self) {
        OWN_TURN_IDS.set(self.previous);
    }
}

fn mint_turn_id() -> u64 {
    let id = turn_id_count().fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    #[cfg(unix)]
    crate::crash_restore::note_turn_ids();
    id
}

/// [`mint_turn_id`], for a crash-restore test that holds the region to every id minted.
#[cfg(all(test, unix))]
pub(crate) fn mint_turn_id_for_test() -> u64 {
    mint_turn_id()
}

/// The last turn id minted — by this process, or by the one it continued the count from ([`raise_turn_ids`]); 0 before either — for the handoff manifest.
pub(crate) fn turn_ids_minted() -> u64 {
    turn_id_count().load(std::sync::atomic::Ordering::Relaxed)
}

/// Continue the turn-id count from `minted` (an earlier process's [`turn_ids_minted`]).
pub(crate) fn raise_turn_ids(minted: u64) {
    turn_id_count().fetch_max(
        minted.min(crate::handoff_carry::MAX_TURN_ID),
        std::sync::atomic::Ordering::Relaxed,
    );
    #[cfg(unix)]
    crate::crash_restore::note_turn_ids();
}

/// How many turn ids a self-update leaves this process between the park's manifest and its `_exit` ([`turn_ids_for_handoff`]).
pub(crate) const TURN_ID_RESERVE: u64 = 1 << 12;

/// The turn-id count a handoff manifest carries: ids minted so far plus [`TURN_ID_RESERVE`], within what a manifest holds.
#[cfg(any(unix, test))]
pub(crate) fn turn_ids_for_handoff() -> u64 {
    turn_ids_minted()
        .saturating_add(TURN_ID_RESERVE)
        .min(crate::handoff_carry::MAX_TURN_ID)
}

/// Whether this process has minted a turn id past `ceiling`, the count the attempt's handoff manifest carried: the successor would mint it again, so the Commit stands down (and the next attempt carries a fresh count).
#[cfg(unix)]
pub(crate) fn turn_ids_past_handoff_ceiling(ceiling: u64) -> bool {
    past_ceiling(turn_ids_minted(), ceiling)
}

#[cfg(any(unix, test))]
const fn past_ceiling(minted: u64, ceiling: u64) -> bool {
    ceiling > 0 && ceiling < crate::handoff_carry::MAX_TURN_ID && minted > ceiling
}

/// Input delivery for [`cmd_turn`], resolved by the dispatch: the active tab's path delivers through the event loop (`Wake::Input`, so window side effects run), a cross-session target through the source-blind seam (`seam_egress`).
pub(crate) struct TurnIo<'a> {
    /// Deliver the message text into the target's input path with PASTE semantics (control bytes stripped by `format_paste`; bracketed-paste framing only when the app itself enabled the mode).
    pub paste: &'a dyn Fn(&str) -> bool,
    /// Press a named key (the `key` verb vocabulary, e.g. "enter").
    pub press: &'a dyn Fn(&str) -> bool,
    /// Press one pre-built key event (`turn typed=1`'s per-grapheme press, built by [`typed_key_events`]) through the same route `press` takes.
    pub key: &'a dyn Fn(InputEvent) -> bool,
    /// One reading of the target's typing momentum (`trail status momentum=`) and when it was read, for `turn yield=<floor>`.
    pub momentum: &'a dyn Fn() -> Result<(f32, std::time::Instant), String>,
    /// WHO is driving: the session the connection's edge token was granted to (`EdgeTable::src_of` on the resolved target's table — the one that authorized this request), or `None` for an Owner-class caller (the per-instance token, the bridge), neither of which is a session.
    pub driver: Option<aterm_session::SessionId>,
    /// WHO is calling, for the turn's ledger record ([`turn_by`]: the connection's identity, `ctl`, `bridge` or the edge's sid).
    pub by: TurnBy,
    /// The session log's `by=` for the turn this exchange opens: the connection's ([`log_actor`]: `harness`, `ctl`, `bridge`, an edge's minted sid, else `-`), never a parameter.
    pub actor: aterm_slog::Actor,
    /// A leading `cause=` the dispatch took and checked: the turn this exchange opens names it.
    pub cause: Option<aterm_slog::ShortId>,
    /// Shared receipt observer for the actual production routes; bool alone is not byte evidence.
    pub mark: Option<&'a crate::control_input_ack::InputMark>,
    /// The `id=` claim's witness, told when the turn answers having written nothing (no paste, typed key or submit press reached for the PTY, or its first was refused whole by the input queue or the fence ([`TurnIo::refused`])), so its sequence goes back whatever it answered.
    pub witness: Option<&'a crate::pty_idem::Witness>,
    /// Asked once a write route (`paste`, `press`, `key`) answered `false`: the `ERR busy …` line `send` answers when that write was refused whole, nothing of it written (the session's input queue full, the `if-supervisor=` fence), else `None`.
    pub refused: &'a dyn Fn() -> Option<String>,
}

/// The end of turn `id`'s INPUT: its lease stops saying the driver may type ([`crate::Lease::driver_may_type`]) — only while the slot still holds this turn, never a turn that preempted it — and the presence is woken, so the session's slot reads the lease's end now.
pub(crate) fn end_turn_input(
    turn_lease: &std::sync::Mutex<Option<crate::Lease>>,
    id: u64,
    sid: &aterm_session::SessionId,
) -> bool {
    let mut lease = turn_lease.lock().unwrap_or_else(|p| p.into_inner());
    let Some(crate::Lease::Turn {
        id: held, typing, ..
    }) = lease.as_mut()
    else {
        return false;
    };
    if *held != id || !*typing {
        return false;
    }
    *typing = false;
    drop(lease);
    crate::presence::post_lease_changed(sid);
    true
}

#[cfg(test)]
impl TurnIo<'static> {
    /// The two routes a plain `turn` needs and none of the even-hand ones: a base for `..TurnIo::paste_only()` at a test site that pastes and presses only.
    #[cfg(test)]
    pub(crate) fn paste_only() -> Self {
        fn no_paste(_: &str) -> bool {
            false
        }
        fn no_key(_: InputEvent) -> bool {
            false
        }
        fn no_momentum() -> Result<(f32, std::time::Instant), String> {
            Err("no momentum source on this route".to_string())
        }
        fn not_refused() -> Option<String> {
            None
        }
        TurnIo {
            paste: &no_paste,
            press: &no_paste,
            key: &no_key,
            momentum: &no_momentum,
            driver: None,
            by: TurnBy::Unknown,
            actor: aterm_slog::Actor::Ctl,
            cause: None,
            mark: None,
            witness: None,
            refused: &not_refused,
        }
    }
}

/// The typed-prefix cap of `turn typed=1`: at most this many graphemes are pressed one key at a time, the remainder going as one paste. 240 at the default cadence is ~17 s of typing, inside the default 240 s deadline with room for the reply.
pub(crate) const TYPED_CAP: usize = 240;

/// The most graphemes `turn typed=1 cap=<n>` may ask to have pressed one key at a time.
pub(crate) const TYPED_CAP_MAX: usize = 4096;

/// The default inter-key interval of `turn typed=1`, in ms (~14 cps).
pub(crate) const TYPED_CADENCE_MS: u64 = 70;

const fn unshift_us(c: char) -> Option<char> {
    Some(match c {
        'A'..='Z' => c.to_ascii_lowercase(),
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '~' => '`',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        _ => return None,
    })
}

const fn has_us_base_layout(base: char) -> bool {
    base.is_ascii_lowercase()
        || base.is_ascii_digit()
        || matches!(
            base,
            '`' | '-' | '=' | '[' | ']' | '\\' | ';' | '\'' | ',' | '.' | '/'
        )
}

fn typed_key_event(g: &str) -> Option<InputEvent> {
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    let mut chars = g.chars();
    let (key, mods) = match (chars.next(), chars.next()) {
        (Some('\n' | '\r'), None) => (Key::Named(NamedKey::Enter), Modifiers::empty()),
        (Some('\r'), Some('\n')) if chars.next().is_none() => {
            (Key::Named(NamedKey::Enter), Modifiers::empty())
        }
        (Some('\t'), None) => (Key::Named(NamedKey::Tab), Modifiers::empty()),
        (Some(' '), None) => (Key::Named(NamedKey::Space), Modifiers::empty()),
        (Some(c), None) if c.is_control() => return None,
        (Some(c), None) => match unshift_us(c) {
            Some(base) => (Key::Character(base), Modifiers::SHIFT),
            None => (Key::Character(c), Modifiers::empty()),
        },
        (Some(_), Some(_)) => return Some(InputEvent::Text(g.to_string())),
        (None, _) => return None,
    };
    let base_layout = match key {
        Key::Character(base) if has_us_base_layout(base) => Some(base),
        _ => None,
    };
    Some(InputEvent::Key {
        key,
        mods,
        base_layout,
        layout_text: None,
        event_type: KeyEventType::Press,
    })
}

/// `turn typed=1`'s press list for `text`, one event per grapheme ([`typed_key_event`]), pure so the tuple a typed turn presses is pinned beside the tuple a keyboard presses without an event loop.
pub(crate) fn typed_key_events(text: &str) -> Vec<InputEvent> {
    use aterm_grapheme::GraphemeClusters;
    text.graphemes().filter_map(typed_key_event).collect()
}

/// Split `text` at the `cap`th grapheme ([`TYPED_CAP`] unless the turn said `cap=`): `(typed prefix, pasted remainder)`.
pub(crate) fn split_typed_cap(text: &str, cap: usize) -> (&str, &str) {
    use aterm_grapheme::GraphemeClusters;
    match text.grapheme_indices().nth(cap) {
        Some((at, _)) => text.split_at(at),
        None => (text, ""),
    }
}

struct TypingHold {
    epoch: u64,
    person: u64,
    frame: Option<Frame>,
    /// The `if-rows=` fence the first look deferred to this hold, judged on the first whole frame.
    unvouched: Option<aterm_types::control_verbs::RowsFence>,
}

struct Frame {
    top: String,
    bottom: String,
    caret: String,
}

impl Frame {
    fn of(terminal: &Terminal, fence: aterm_types::control_verbs::RowsFence) -> Option<Self> {
        use aterm_grapheme::GraphemeClusters;
        let row = |r: usize| crate::control::visible_row(terminal, r);
        let (top, bottom) = (row(fence.first), row(fence.last));
        let caret = row(fence.first.saturating_add(1))
            .graphemes()
            .next()
            .unwrap_or_default()
            .to_string();
        (fence.first.saturating_add(1) < fence.last
            && aterm_phase::phase::is_rule(&top)
            && aterm_phase::phase::is_rule(&bottom)
            && !caret.trim().is_empty())
        .then_some(Self { top, bottom, caret })
    }

    fn holds(&self, terminal: &Terminal, row: usize) -> bool {
        let text = |r: usize| crate::control::visible_row(terminal, r);
        let under = row.saturating_add(1);
        !self.caret.is_empty()
            && under < usize::from(terminal.rows())
            && text(under) == self.bottom
            && (0..=row)
                .rev()
                .find(|&r| text(r) == self.top)
                .is_some_and(|top| text(top.saturating_add(1)).starts_with(self.caret.as_str()))
    }
}

enum Look {
    Held(aterm_core::grid::Cursor),
    Wait,
    Stop(&'static str),
}

/// The write a typing hold waits for the program to read and draw ([`TypingHold::held`]).
#[derive(Clone, Copy)]
struct Sent<'a> {
    /// The caret the hold read just before the write, which the write's echo moves.
    from: aterm_core::grid::Cursor,
    /// A guarded submit's guard, at its tail: the cursor row matching it shows the write drawn,
    /// though the program left the caret where it was.
    guard: Option<&'a dyn aterm_core::terminal::RowMatch>,
}

impl Sent<'_> {
    const fn key(from: aterm_core::grid::Cursor) -> Self {
        Self { from, guard: None }
    }

    /// Whether `terminal`, its caret at `cursor`, shows the write drawn: the caret moved off
    /// [`Sent::from`], or the cursor row matches the guard.
    fn drawn(&self, terminal: &Terminal, cursor: aterm_core::grid::Cursor) -> bool {
        use aterm_core::terminal::{RowRange, first_matching_row};
        let row = usize::from(cursor.row);
        let range = RowRange::Span {
            start: row,
            end: row,
        };
        self.from != cursor
            || self
                .guard
                .is_some_and(|guard| first_matching_row(terminal, guard, range).is_some())
    }
}

impl TypingHold {
    fn of(
        terminal: &Terminal,
        ctx: &SessionCtx,
        rows: Option<aterm_types::control_verbs::RowsFence>,
        unvouched: Option<aterm_types::control_verbs::RowsFence>,
    ) -> Self {
        Self {
            epoch: crate::control::screen_gen(terminal).epoch,
            person: ctx.human_input.seq(),
            frame: rows.and_then(|fence| Frame::of(terminal, fence)),
            unvouched,
        }
    }

    fn look(&mut self, terminal: &Terminal, ctx: &SessionCtx, sent: Option<Sent<'_>>) -> Look {
        if ctx.human_input.seq() != self.person {
            return Look::Stop("person");
        }
        if crate::control::screen_gen(terminal).epoch != self.epoch {
            return Look::Stop("changed");
        }
        let cursor = terminal.cursor();
        let Some(frame) = &self.frame else {
            return Look::Held(cursor);
        };
        if terminal.sync_frame_unfinished() {
            Look::Wait
        } else if self.unvouched.take().is_some_and(|fence| {
            crate::frame_memo::ScreenText::of(terminal).rows_hash(fence.first, fence.last)
                != Some(fence.hash)
        }) || !frame.holds(terminal, usize::from(cursor.row))
        {
            Look::Stop("changed")
        } else if sent.is_some_and(|sent| keys_unread(ctx) || !sent.drawn(terminal, cursor)) {
            Look::Wait
        } else {
            Look::Held(cursor)
        }
    }

    /// Waits for a look that holds, cut at `deadline`. A wait that ends held gives the time it
    /// took back to `deadline`: the program drawing a frame or an echo is not the turn's own
    /// time, so a starved program that keeps drawing is never cut part-way by the sum of its
    /// lags, and `deadline`'s slack bounds each wait alone, a hang detector.
    fn held(
        &mut self,
        term: &Arc<Mutex<Terminal>>,
        ctx: &SessionCtx,
        sub: &crate::session_watch::Subscription,
        sent: Option<Sent<'_>>,
        tick: std::time::Duration,
        deadline: &mut std::time::Instant,
    ) -> Result<aterm_core::grid::Cursor, &'static str> {
        let mut since = None;
        loop {
            let look = self.look(&term_lock(term), ctx, sent);
            match look {
                Look::Held(cursor) => {
                    if let Some(since) = since {
                        let waited = hold_wait_now().saturating_duration_since(since);
                        *deadline = deadline.checked_add(waited).unwrap_or(*deadline);
                    }
                    return Ok(cursor);
                }
                Look::Stop(why) => return Err(why),
                Look::Wait => {
                    let now = hold_wait_now();
                    if now >= *deadline {
                        return Err("deadline");
                    }
                    since.get_or_insert(now);
                    let _ = sub.wait(tick.min(*deadline - now));
                }
            }
        }
    }
}

#[cfg(test)]
thread_local! {
    static HOLD_WAIT_FOR_TEST: std::cell::RefCell<Option<Box<dyn Fn() -> std::time::Instant>>> =
        const { std::cell::RefCell::new(None) };
}

/// A test's hand at this thread's typing-hold waits: it plays the program there and answers the instant the wait is judged at, and the instant a wait that held ends at (`None`: the clock).
#[cfg(test)]
pub(crate) fn set_hold_wait_for_test(hand: Option<Box<dyn Fn() -> std::time::Instant>>) {
    HOLD_WAIT_FOR_TEST.with(|slot| *slot.borrow_mut() = hand);
}

fn hold_wait_now() -> std::time::Instant {
    #[cfg(test)]
    if let Some(now) = HOLD_WAIT_FOR_TEST.with(|slot| slot.borrow().as_ref().map(|hand| hand())) {
        return now;
    }
    std::time::Instant::now()
}

fn keys_unread(ctx: &SessionCtx) -> bool {
    ctx.sink
        .input_backlog()
        .is_some_and(|b| b.queued != 0 || b.spilled != Some(0))
}

const fn is_digit_key(ev: &InputEvent) -> bool {
    matches!(
        ev,
        InputEvent::Key {
            key: aterm_types::keyboard::Key::Character(c),
            mods,
            ..
        } if c.is_ascii_digit() && mods.is_empty()
    )
}

fn typed_cut(
    term: &Arc<Mutex<Terminal>>,
    why: &str,
    typed: usize,
    pasted: usize,
    id: u64,
) -> String {
    let seq = term_lock(term).content_seq();
    format!(
        "OK 0 turn skipped reason={why} typed={typed} pasted={pasted} submitted=0 seq={seq} \
         id={id}\n"
    )
}

enum Parked<T> {
    Done(T),
    Deadline,
    Exited,
    HungUp,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OnHangup {
    Abort,
    Finish,
}

#[derive(Clone, Copy)]
enum Stretch<'a> {
    None,
    Hold(&'a crate::spawn::UpdatePark),
    Window {
        parked: &'a crate::spawn::UpdatePark,
        bound: std::time::Instant,
    },
}

impl Stretch<'_> {
    fn parked(self) -> bool {
        match self {
            Stretch::None => false,
            Stretch::Hold(parked) | Stretch::Window { parked, .. } => parked.is_parked(),
        }
    }
}

struct Park<'a> {
    sub: &'a crate::session_watch::Subscription,
    exited: &'a dyn Fn() -> bool,
    on_hangup: OnHangup,
    stretch: Stretch<'a>,
    lane: crate::park::Lane,
}

impl<'a> Park<'a> {
    fn new(
        sub: &'a crate::session_watch::Subscription,
        exited: &'a dyn Fn() -> bool,
        on_hangup: OnHangup,
        stretch: Stretch<'a>,
    ) -> Self {
        Park {
            sub,
            exited,
            on_hangup,
            stretch,
            lane: crate::park::Lane::SERVED,
        }
    }

    fn run<T>(
        &self,
        until: std::time::Instant,
        mut look: impl FnMut(std::time::Instant, bool, bool) -> (Option<T>, Option<std::time::Instant>),
        disarm: impl FnOnce(),
    ) -> Parked<T> {
        use std::time::Instant;
        let lane = match self.on_hangup {
            OnHangup::Abort => self.lane,
            OnHangup::Finish => self.lane.ignoring_hangup(),
        };
        let mut until = until;
        let mut parked_since: Option<Instant> = None;
        loop {
            if (self.exited)() {
                disarm();
                return Parked::Exited;
            }
            if (lane.hung_up)() {
                disarm();
                return Parked::HungUp;
            }
            let now = Instant::now();
            let parked = self.stretch.parked();
            let closes = match self.stretch {
                Stretch::Window { bound, .. } => {
                    match (parked, parked_since) {
                        (true, None) => parked_since = Some(now),
                        (false, Some(since)) => {
                            until = (until + now.saturating_duration_since(since))
                                .min(bound.max(until));
                            parked_since = None;
                        }
                        _ => {}
                    }
                    if parked { bound.max(until) } else { until }
                }
                Stretch::None | Stretch::Hold(_) => until,
            };
            let closing = now >= closes;
            let (answer, next_dl) = look(now, parked, closing);
            if let Some(answer) = answer {
                return Parked::Done(answer);
            }
            if closing {
                return Parked::Deadline;
            }
            let mut wake = closes;
            if let Some(dl) = next_dl {
                wake = wake.min(dl);
            }
            if parked {
                wake = wake.min(now + PARKED_POLL);
            }
            let flipped = || self.stretch.parked() != parked;
            let _ = crate::park::park(|bound| self.sub.wait(bound), wake, lane, &flipped);
        }
    }
}

fn watcher_look(
    term: &Arc<Mutex<Terminal>>,
    id: aterm_core::terminal::WatchId,
) -> impl FnMut(std::time::Instant, bool, bool) -> (Option<u64>, Option<std::time::Instant>) + '_ {
    move |now, parked, closing| {
        let mut t = term_lock(term);
        fire_idle_unless_parked(&mut t, now, parked);
        let latched = t.watch_poll(id).map(|s| s.seq);
        if latched.is_some() || closing {
            t.watch_disarm(id);
        }
        (latched, t.watch_next_deadline())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PressLook {
    Latched,
    Started,
    /// The cursor left the row the text was typed on: a program with no command blocks took the line.
    Moved,
}

fn press_look(
    term: &Arc<Mutex<Terminal>>,
    id: aterm_core::terminal::WatchId,
    (base, typed_row): (Option<u64>, Option<u16>),
) -> impl FnMut(std::time::Instant, bool, bool) -> (Option<PressLook>, Option<std::time::Instant>) + '_
{
    move |now, parked, closing| {
        let mut t = term_lock(term);
        fire_idle_unless_parked(&mut t, now, parked);
        let latched = t.watch_poll(id).is_some();
        let moved = typed_row.is_some_and(|row| t.cursor().row != row);
        if latched || moved || closing {
            t.watch_disarm(id);
        }
        let started = closing && base.is_some_and(|base| t.started_command_seq() > base);
        let answer = if started {
            Some(PressLook::Started)
        } else if latched {
            Some(PressLook::Latched)
        } else if moved {
            Some(PressLook::Moved)
        } else {
            None
        };
        (answer, t.watch_next_deadline())
    }
}

fn disarm_watch(
    term: &Arc<Mutex<Terminal>>,
    id: aterm_core::terminal::WatchId,
) -> impl FnOnce() + '_ {
    move || term_lock(term).watch_disarm(id)
}

/// How a wait for the ribbon to exhale ended.
pub(crate) enum MomentumWait {
    /// The metric read at or under the floor.
    Below(f32),
    /// The backstop passed first; the reading is the one taken at that moment.
    Deadline(f32),
    /// The session died while parked.
    Exited,
    /// The caller hung up while parked (nothing was typed yet).
    HungUp,
    /// No reading could be taken (the reason), or the watcher budget was full.
    Unreadable(String),
}

fn momentum_read_outcome(
    value: f32,
    floor: f32,
    now: std::time::Instant,
    until: std::time::Instant,
) -> Option<MomentumWait> {
    if value <= floor {
        Some(MomentumWait::Below(value))
    } else if now >= until {
        Some(MomentumWait::Deadline(value))
    } else {
        None
    }
}

/// The yield: park until the target's typing momentum has decayed to `floor`, analytically: one reading, one crossing solved from the release law (`TypingMomentum::reading_low_crossing`), one kernel timer ([`WatcherSpec::MomentumBelow`](aterm_core::terminal::WatcherSpec::MomentumBelow)), no polling.
pub(crate) fn park_until_momentum_below(
    term: &Arc<Mutex<Terminal>>,
    exited: &dyn Fn() -> bool,
    sub: &crate::session_watch::Subscription,
    read: &dyn Fn() -> Result<(f32, std::time::Instant), String>,
    floor: f32,
    until: std::time::Instant,
) -> MomentumWait {
    use aterm_core::terminal::WatcherSpec;
    use aterm_effects::typing_momentum::TypingMomentum;
    loop {
        let (v, at) = match read() {
            Ok(r) => r,
            Err(e) => return MomentumWait::Unreadable(e),
        };
        if v > floor && exited() {
            return MomentumWait::Exited;
        }
        if let Some(outcome) = momentum_read_outcome(v, floor, std::time::Instant::now(), until) {
            return outcome;
        }
        let mut metric = TypingMomentum::default();
        metric.set_value(at, v);
        let crossing = metric.reading_low_crossing(floor).unwrap_or(at);
        let armed = term_lock(term).watch(
            WatcherSpec::MomentumBelow { floor, crossing },
            std::time::Instant::now(),
        );
        let Some(id) = armed else {
            return MomentumWait::Unreadable(WAITS_FULL.to_string());
        };
        let park = Park::new(sub, exited, OnHangup::Abort, Stretch::None);
        match park.run(until, watcher_look(term, id), disarm_watch(term, id)) {
            Parked::Done(_) => {}
            Parked::Deadline => {
                let now_v = read().map_or(v, |(v, _)| v);
                return MomentumWait::Deadline(now_v);
            }
            Parked::Exited => return MomentumWait::Exited,
            Parked::HungUp => return MomentumWait::HungUp,
        }
    }
}

/// `await momentum <floor> [timeout <ms>|timeout=<ms>]` — block until the typing momentum of the window hosting this session (`trail status momentum=`) has decayed to `floor` (0..=1), then `OK momentum <v>`; `OK timeout` past the deadline (default 30 s), `ERR exited` if the session dies.
pub(crate) fn cmd_await_momentum(
    term: &Arc<Mutex<Terminal>>,
    store: &Store,
    session: u64,
    ctx: &SessionCtx,
    rest: &str,
    read: &dyn Fn() -> Result<(f32, std::time::Instant), String>,
) -> String {
    use std::time::{Duration, Instant};

    const USAGE: &str = "ERR usage: await momentum <floor 0..=1> [timeout <ms>]\n";
    use aterm_control::wire::wait_ms;
    let mut floor: Option<f32> = None;
    let mut timeout_ms = aterm_control::wire::WAIT_DEFAULT_MS;
    let toks: Vec<&str> = rest.split_whitespace().collect();
    let mut i = 0;
    while i < toks.len() {
        if i == 0 && toks[0] == "momentum" {
            i += 1;
        } else if let Some(v) = toks[i].strip_prefix("timeout=") {
            let Some(ms) = wait_ms(v) else {
                return USAGE.to_string();
            };
            timeout_ms = ms;
            i += 1;
        } else if toks[i] == "timeout" && i + 1 < toks.len() {
            let Some(ms) = wait_ms(toks[i + 1]) else {
                return USAGE.to_string();
            };
            timeout_ms = ms;
            i += 2;
        } else if floor.is_none() {
            match toks[i].parse::<f32>() {
                Ok(f) if f.is_finite() && (0.0..=1.0).contains(&f) => floor = Some(f),
                _ => return USAGE.to_string(),
            }
            i += 1;
        } else {
            return USAGE.to_string();
        }
    }
    let Some(floor) = floor else {
        return USAGE.to_string();
    };
    let until = Instant::now() + Duration::from_millis(timeout_ms);
    let sub = crate::session_watch::Subscription::single(&ctx.watchers);
    let probe = exit_probe(store, session);
    let exited = || probe(store);
    match park_until_momentum_below(term, &exited, &sub, read, floor, until) {
        MomentumWait::Below(v) => format!("OK momentum {v:.2}\n"),
        MomentumWait::Deadline(_) => "OK timeout\n".to_string(),
        MomentumWait::Exited => "ERR exited\n".to_string(),
        MomentumWait::HungUp => crate::control::HUNG_UP_REPLY.to_string(),
        MomentumWait::Unreadable(e) => format!("ERR {e}\n"),
    }
}

enum GuardedSubmit {
    Pressed,
    Missed,
    /// The sink took none of the Enter until the submit window ended: nothing of this press was written.
    Busy,
    Failed(String),
}

fn guarded_submit(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    guard: &dyn aterm_core::terminal::RowMatch,
    window: std::time::Instant,
    mark: Option<&crate::control_input_ack::InputMark>,
) -> GuardedSubmit {
    use super::control_input::GuardedInput;
    use crate::input::Delivery;
    const BUSY_RETRY: std::time::Duration = std::time::Duration::from_millis(5);
    loop {
        let Some(enter) = crate::control::parse_key("enter") else {
            return GuardedSubmit::Failed("ERR internal: `enter` did not parse\n".to_string());
        };
        let fence = super::control_input::InputFence {
            guard: Some(guard),
            guard_at_cursor: true,
            ..super::control_input::InputFence::default()
        };
        match super::control_input::input_if_fenced_observed(
            term,
            ctx,
            enter,
            &fence,
            crate::ReadOn::Committed,
            mark.map(|m| m.completion()),
        ) {
            GuardedInput::Pressed(Delivery::Full) => {
                return GuardedSubmit::Pressed;
            }
            GuardedInput::Skipped | GuardedInput::Changed | GuardedInput::Person => {
                return GuardedSubmit::Missed;
            }
            GuardedInput::Pressed(Delivery::BusyZero) if std::time::Instant::now() < window => {
                std::thread::sleep(BUSY_RETRY);
            }
            GuardedInput::Pressed(Delivery::BusyZero) => return GuardedSubmit::Busy,
            decision @ GuardedInput::Pressed(_) => {
                return GuardedSubmit::Failed(super::control_input::guarded_input_reply(decision));
            }
        }
    }
}

/// `turn`'s usage line.
pub(crate) const TURN_USAGE: &str = "ERR usage: turn [id=<epoch>:<producer>:<seq>] [if-supervisor=<holder>] [idle=<ms>] [timeout=<ms>] [submit=<key|none|guarded:<re>>] [settle=match:<re>|gone:<re>|submitted] [submit_window=<ms>] [presses=<n>] [submit_verify=<auto|seq|block>] [trim=<0|1>] [typed=<0|1>] [cadence=<ms>] [cap=<n>] [yield=<floor>] [if-gen=<epoch>.<seq>] [if-rows=<first>-<last>:<hex16>] [cause=@<off>|<seq>] <text>\n";

/// `turn`'s answer when a key it pressed (a typed grapheme, the submit key) did not deliver.
const KEY_DELIVERY_FAILED: &str = "ERR key delivery failed\n";

/// `turn`'s answer when a paste of its text did not deliver.
const PASTE_DELIVERY_FAILED: &str = "ERR paste delivery failed\n";

/// `turn`'s options as parsed ([`turn_options`]): every leading `k=v`, and the message after them, verbatim.
pub(crate) struct TurnOptions<'a> {
    idle_ms: u64,
    pub(crate) timeout_ms: u64,
    submit: String,
    submit_guard: Option<String>,
    submit_window_ms: u64,
    presses: u32,
    submit_verify: Option<bool>,
    settle_match: Option<(bool, String)>,
    settle_submitted: bool,
    trim: bool,
    typed: bool,
    cadence_ms: u64,
    typed_cap: usize,
    yield_floor: Option<f32>,
    if_gen: Option<crate::control::ScreenGen>,
    if_rows: Option<aterm_types::control_verbs::RowsFence>,
    pub(crate) cause: Option<crate::session_log::CauseRef>,
    text: &'a str,
}

/// PARSE `turn`'s options (pure: no lock, no session): the leading `k=v` options, the message after them verbatim, and every refusal of their shape — a bad value, a `submit=` that names no key, a repeated `cause=`, a turn with no text that still submits.
pub(crate) fn turn_options(rest: &str) -> Result<TurnOptions<'_>, String> {
    const USAGE: &str = TURN_USAGE;
    let mut idle_ms = 1_500u64;
    let mut timeout_ms = 240_000u64;
    let mut submit = "enter".to_string();
    let mut submit_guard: Option<String> = None;
    let mut submit_window_ms = 2_000u64;
    let mut presses = 3u32;
    let mut submit_verify: Option<bool> = None;
    let mut settle_match: Option<(bool, String)> = None;
    let mut settle_submitted = false;
    let mut trim = false;
    let mut typed = false;
    let mut cadence_ms = TYPED_CADENCE_MS;
    let mut typed_cap = TYPED_CAP;
    let mut yield_floor: Option<f32> = None;
    let mut if_gen: Option<crate::control::ScreenGen> = None;
    let mut if_rows: Option<aterm_types::control_verbs::RowsFence> = None;
    let mut cause_opt: Option<crate::session_log::CauseRef> = None;
    let (options, text) = aterm_types::control_verbs::turn_option_words(rest);
    for (k, v) in options {
        match k {
            "idle" => match v.parse::<u64>() {
                Ok(ms) => idle_ms = ms.clamp(1, 600_000),
                Err(_) => return Err(USAGE.to_string()),
            },
            "timeout" => match v.parse::<u64>() {
                Ok(ms) => timeout_ms = ms,
                Err(_) => return Err(USAGE.to_string()),
            },
            "submit" => match v.strip_prefix("guarded:") {
                Some("") => return Err(USAGE.to_string()),
                Some(pat) => {
                    submit = "enter".to_string();
                    submit_guard = Some(pat.to_string());
                }
                None => {
                    submit = v.to_string();
                    submit_guard = None;
                }
            },
            "submit_window" => match v.parse::<u64>() {
                Ok(ms) => submit_window_ms = ms.clamp(1, 600_000),
                Err(_) => return Err(USAGE.to_string()),
            },
            "presses" => match v.parse::<u32>() {
                Ok(n) if n >= 1 => presses = n.min(10),
                _ => return Err(USAGE.to_string()),
            },
            "submit_verify" => match v {
                "seq" => submit_verify = Some(false),
                "block" => submit_verify = Some(true),
                "auto" => submit_verify = None,
                _ => return Err(USAGE.to_string()),
            },
            "settle" if v == "submitted" => {
                settle_match = None;
                settle_submitted = true;
            }
            "settle" => match v.split_once(':') {
                Some(("match", pat)) if !pat.is_empty() => {
                    settle_match = Some((false, pat.to_string()));
                    settle_submitted = false;
                }
                Some(("gone", pat)) if !pat.is_empty() => {
                    settle_match = Some((true, pat.to_string()));
                    settle_submitted = false;
                }
                _ => return Err(USAGE.to_string()),
            },
            "trim" => match v {
                "1" => trim = true,
                "0" => trim = false,
                _ => return Err(USAGE.to_string()),
            },
            "typed" => match v {
                "1" => typed = true,
                "0" => typed = false,
                _ => return Err(USAGE.to_string()),
            },
            "cadence" => match v.parse::<u64>() {
                Ok(ms) => cadence_ms = ms.clamp(1, 2_000),
                Err(_) => return Err(USAGE.to_string()),
            },
            "cap" => match v.parse::<usize>() {
                Ok(n) if n >= 1 => typed_cap = n.min(TYPED_CAP_MAX),
                _ => return Err(USAGE.to_string()),
            },
            "yield" => match v.parse::<f32>() {
                Ok(f) if f.is_finite() && (0.0..=1.0).contains(&f) => yield_floor = Some(f),
                _ => return Err(USAGE.to_string()),
            },
            "if-gen" => match crate::control::ScreenGen::parse(v) {
                Some(generation) => if_gen = Some(generation),
                None => return Err(USAGE.to_string()),
            },
            "if-rows" => match aterm_types::control_verbs::RowsFence::parse(v) {
                Some(fence) => if_rows = Some(fence),
                None => return Err(USAGE.to_string()),
            },
            "cause" => match crate::session_log::CauseRef::parse(v) {
                Some(c) if cause_opt.is_none() => cause_opt = Some(c),
                _ => return Err(crate::session_log::CAUSE_USAGE.to_string()),
            },
            _ => return Err(USAGE.to_string()),
        }
    }
    let timeout_ms = timeout_ms.min(600_000);
    if text.trim().is_empty() && submit != "none" {
        return Err(USAGE.to_string());
    }
    if settle_submitted && submit == "none" {
        return Err(USAGE.to_string());
    }
    if submit != "none" && crate::control::parse_key(&submit).is_none() {
        return Err(USAGE.to_string());
    }
    Ok(TurnOptions {
        idle_ms,
        timeout_ms,
        submit,
        submit_guard,
        submit_window_ms,
        presses,
        submit_verify,
        settle_match,
        settle_submitted,
        trim,
        typed,
        cadence_ms,
        typed_cap,
        yield_floor,
        if_gen,
        if_rows,
        cause: cause_opt,
        text,
    })
}

type TurnMatchers = (
    Option<(bool, Arc<dyn aterm_core::terminal::RowMatch>)>,
    Option<Arc<dyn aterm_core::terminal::RowMatch>>,
);

fn turn_matchers(o: &TurnOptions<'_>) -> Result<TurnMatchers, String> {
    let settle_matcher = match &o.settle_match {
        Some((gone, pat)) => match aterm_observe::row_matcher(pat) {
            Ok(m) => Some((*gone, m)),
            Err(_) => return Err("ERR badregex\n".to_string()),
        },
        None => None,
    };
    let submit_matcher = super::control_input::compile_guard(o.submit_guard.as_deref())?;
    Ok((settle_matcher, submit_matcher))
}

/// Every refusal a `turn` can make of its own words, made before the exactly-once claim (`pty_idem::guarded`): the options' grammar, a bad pattern, and the `cause=` among them.
pub(crate) fn turn_preflight(rest: &str) -> Result<Option<crate::session_log::CauseRef>, String> {
    let o = turn_options(rest)?;
    turn_matchers(&o)?;
    Ok(o.cause)
}

/// `turn [<options>] <text>` (the options: [`TURN_USAGE`]) — one complete human turn against the target session: type `<text>`, submit it with a real keypress, block until the app's response settles, and return the settled screen — `OK <rows> turn submitted=<0|1> status=<settled|timeout|hangup|submitted> seq=<n> id=<n> dur_ms=<n> hash=<hex16>[ typed=<n> pasted=<n>][ yielded_ms=<ms>][ pressed=<n>][ task_rev=<n>][ agent_rev=<n>][ frame=open][ trimmed=<k>]` then `<rows>` text rows (the same rows `text` prints).
pub(crate) fn cmd_turn(
    term: &Arc<Mutex<Terminal>>,
    store: &Store,
    session: u64,
    rest: &str,
    ctx: &SessionCtx,
    io: &TurnIo<'_>,
) -> String {
    let wrote = std::cell::Cell::new(false);
    let reply = turn_attempt(term, store, session, rest, ctx, io, &wrote);
    if !wrote.get()
        && let Some(witness) = io.witness
    {
        witness.nothing_written();
    }
    reply
}

/// The body of [`cmd_turn`]: `wrote` is set before each write it makes, delivered or not, and cleared again when its first write was refused whole by the input queue or the fence ([`TurnIo::refused`]).
fn turn_attempt(
    term: &Arc<Mutex<Terminal>>,
    store: &Store,
    session: u64,
    rest: &str,
    ctx: &SessionCtx,
    io: &TurnIo<'_>,
    wrote: &std::cell::Cell<bool>,
) -> String {
    use std::time::{Duration, Instant};

    use aterm_core::terminal::{BlockState, WatcherSpec};

    const ECHO_SETTLE: Duration = Duration::from_millis(150);
    const ECHO_CAP: Duration = Duration::from_secs(5);

    let o = match turn_options(rest) {
        Ok(o) => o,
        Err(refusal) => return refusal,
    };
    let cause = match (io.cause, o.cause) {
        (Some(id), _) => Some(id),
        (None, Some(c)) => match crate::session_log::validate_cause(ctx, c) {
            Ok(id) => Some(id),
            Err(refusal) => return refusal,
        },
        (None, None) => None,
    };
    let (settle_matcher, submit_matcher) = match turn_matchers(&o) {
        Ok(m) => m,
        Err(reply) => return reply,
    };
    let TurnOptions {
        idle_ms,
        timeout_ms,
        submit,
        submit_window_ms,
        presses,
        submit_verify,
        settle_submitted,
        trim,
        typed,
        cadence_ms,
        typed_cap,
        yield_floor,
        if_gen,
        if_rows,
        text,
        ..
    } = o;

    let (turn_id, handed_back) = {
        let mut lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
        let now = crate::metrics::now_us();
        let own = lease
            .as_ref()
            .filter(|h| h.held_over(now, super::serving_connection()))
            .cloned();
        if own.is_none()
            && let Some(held) = lease.as_ref().filter(|h| h.is_live(now))
        {
            return match held {
                crate::Lease::Turn { id, .. } => format!("ERR busy turn={id}\n"),
                crate::Lease::Drive { holder, .. } => format!("ERR busy lease={holder}\n"),
            };
        }
        let id = {
            let mut turns = ctx.turns.lock().unwrap_or_else(|p| p.into_inner());
            let id = mint_turn_id();
            turns.begin(id);
            id
        };
        *lease = Some(crate::Lease::Turn {
            id,
            driver: io.driver.clone(),
            typing: true,
        });
        (id, own.map(|hand| (hand, now)))
    };
    let by = typed_by(&io.by, handed_back.as_ref().map(|(hand, _)| hand));
    struct LeaseGuard<'a> {
        turn_lease: &'a std::sync::Mutex<Option<crate::Lease>>,
        turns: &'a std::sync::Mutex<crate::turn_ledger::TurnLedger>,
        id: u64,
        sid: &'a aterm_session::SessionId,
        driver: Option<&'a aterm_session::SessionId>,
        /// The serving connection's own hand the turn holds the slot in place of, and when it took it.
        handed_back: Option<(crate::Lease, u64)>,
    }
    impl Drop for LeaseGuard<'_> {
        fn drop(&mut self) {
            self.turns
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .abandon(self.id);
            let mut lease = self.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
            if matches!(lease.as_ref(), Some(crate::Lease::Turn { id: held, .. }) if *held == self.id)
            {
                // The hand's connection waits on this turn and cannot renew it meanwhile: its
                // clock stood still while the turn held the slot, so it comes back with the time
                // it had left, never lapsed by a turn longer than its TTL.
                *lease = self.handed_back.take().map(|(mut hand, taken_us)| {
                    if let crate::Lease::Drive { expires_us, .. } = &mut hand {
                        let held = crate::metrics::now_us().saturating_sub(taken_us);
                        *expires_us = expires_us.saturating_add(held);
                    }
                    hand
                });
                drop(lease);
                crate::presence::post_lease_changed(self.sid);
                if let Some(driver) = self.driver {
                    crate::presence::post_lease_changed(driver);
                }
            }
        }
    }
    let lease_guard = LeaseGuard {
        turn_lease: &ctx.turn_lease,
        turns: &ctx.turns,
        id: turn_id,
        sid: &ctx.self_id,
        driver: io.driver.as_ref(),
        handed_back,
    };
    crate::presence::post_lease_changed(&ctx.self_id);
    if let Some(driver) = &io.driver {
        crate::presence::post_lease_changed(driver);
    }

    let slog_by = io.actor;
    let note_ctl_input = || {
        let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        let at = timeline.slog().now();
        let _ = timeline.slog_mut().ctl_turn(slog_by, turn_id, cause, at);
        let _ = timeline.slog_take_wake();
    };

    let now0 = Instant::now();
    let started_ms = crate::turn_ledger::now_ms();
    let mut deadline = now0 + Duration::from_millis(timeout_ms);
    let arch = crate::turn_ledger::ArchMark::of(&term_lock(term));

    let exited = exit_probe(store, session);

    let sub = crate::session_watch::Subscription::single(&ctx.watchers);

    let exited_now = || exited(store);
    let stretch = Stretch::Window {
        parked: ctx.update_parked.as_ref(),
        bound: deadline,
    };
    let park = |id: aterm_core::terminal::WatchId, until: Instant, on_hangup| -> Parked<u64> {
        Park::new(&sub, &exited_now, on_hangup, stretch).run(
            until,
            watcher_look(term, id),
            disarm_watch(term, id),
        )
    };
    let wait = |id, until| park(id, until, OnHangup::Abort);
    let wait_committed = |id, until| park(id, until, OnHangup::Finish);
    let finishing = crate::park::Lane::SERVED.ignoring_hangup();
    let arm = |spec: WatcherSpec| -> Option<aterm_core::terminal::WatchId> {
        term_lock(term).watch(spec, Instant::now())
    };

    let mut yielded_ms: Option<u64> = None;
    if let Some(floor) = yield_floor {
        let yield0 = Instant::now();
        match park_until_momentum_below(term, &exited_now, &sub, io.momentum, floor, deadline) {
            MomentumWait::Below(_) => yielded_ms = Some(yield0.elapsed().as_millis() as u64),
            MomentumWait::Deadline(v) => return format!("ERR yield timeout momentum={v:.2}\n"),
            MomentumWait::Exited => return "ERR exited\n".to_string(),
            MomentumWait::HungUp => return crate::control::HUNG_UP_REPLY.to_string(),
            MomentumWait::Unreadable(e) => return format!("ERR yield: {e}\n"),
        }
        if let Some(refusal) = crate::input_stall::refusal(ctx, "turn", "", false) {
            return refusal;
        }
    }

    let mut hold: Option<TypingHold> = None;
    if if_gen.is_some() || if_rows.is_some() {
        let mut look = ctx.frame.look_frame();
        let typing = typed && !text.is_empty();
        let held_open = std::cell::Cell::new(false);
        let waits = |t: &Terminal| {
            held_open.set(typing && if_rows.and_then(|fence| Frame::of(t, fence)).is_some());
            if_rows.is_some() && !held_open.get()
        };
        let terminal = crate::term_lock_whole_when(term, crate::park::Lane::SERVED, waits);
        let unvouched = if_rows.filter(|_| held_open.get());
        let held = typing.then(|| TypingHold::of(&terminal, ctx, if_rows, unvouched));
        let moved = if_gen
            .is_some_and(|generation| crate::control::screen_gen(&terminal) != generation)
            || if_rows.is_some_and(|fence| {
                unvouched.is_none()
                    && (crate::frame_unfinished_now(&terminal)
                        || look.text(&terminal).rows_hash(fence.first, fence.last)
                            != Some(fence.hash))
            });
        if moved {
            let seq = terminal.content_seq();
            return format!(
                "OK 0 turn skipped reason=changed submitted=0 seq={seq} id={turn_id}\n"
            );
        }
        hold = held;
    }

    let halted = |landed: Option<(usize, usize)>| -> Option<String> {
        let refusal = crate::fabric::halt_refusal(ctx, "turn")?;
        Some(match landed {
            None => refusal,
            Some((keys, pasted)) if typed => typed_cut(term, "halted", keys, pasted, turn_id),
            Some(_) => {
                let seq = term_lock(term).content_seq();
                format!("OK 0 turn skipped reason=halted submitted=0 seq={seq} id={turn_id}\n")
            }
        })
    };
    // The answer for a write a route did not deliver. The turn's first write, refused whole,
    // wrote nothing at all: it answers the `ERR busy …` line `send` does and clears `wrote`, so
    // the `id=` goes back. Any later one follows bytes already written: `failed`, in doubt.
    let undelivered = |first: bool, failed: &str| match (io.refused)().filter(|_| first) {
        Some(refusal) => {
            wrote.set(false);
            refusal
        }
        None => failed.to_string(),
    };

    let mut typed_keys = 0usize;
    let mut pasted_graphemes = 0usize;
    let cadence = Duration::from_millis(cadence_ms);
    let mut committed: Option<crate::control::CommittedInput> = None;
    let mut echo_until = None;
    // The caret the typing hold read just before the turn's last write, which its echo moves.
    let mut sent_from = None;
    if !text.is_empty() {
        if typed {
            use aterm_grapheme::GraphemeClusters;
            let (head, remainder) = split_typed_cap(text, typed_cap);
            let keys = typed_key_events(head);
            let mut due = Instant::now();
            let last_due =
                due + cadence * u32::try_from(keys.len().saturating_sub(1)).unwrap_or(u32::MAX);
            if !keys.is_empty() && last_due >= deadline {
                return typed_cut(term, "deadline", 0, 0, turn_id);
            }
            for ev in keys {
                let now = Instant::now();
                if due > now {
                    std::thread::sleep(due - now);
                }
                if Instant::now() >= deadline {
                    return typed_cut(term, "deadline", typed_keys, 0, turn_id);
                }
                if exited(store) {
                    return "ERR exited\n".to_string();
                }
                if let Some(h) = &mut hold {
                    let digit = is_digit_key(&ev);
                    match h.held(
                        term,
                        ctx,
                        &sub,
                        sent_from.filter(|_| digit).map(Sent::key),
                        cadence,
                        &mut deadline,
                    ) {
                        Ok(cursor) => sent_from = Some(cursor),
                        Err(why) => return typed_cut(term, why, typed_keys, 0, turn_id),
                    }
                }
                if let Some(cut) = halted(committed.is_some().then_some((typed_keys, 0))) {
                    return cut;
                }
                if typed_keys == 0 {
                    note_ctl_input();
                }
                due = Instant::now() + cadence;
                let first = !wrote.replace(true);
                if !(io.key)(ev) {
                    return undelivered(first, KEY_DELIVERY_FAILED);
                }
                committed.get_or_insert_with(crate::control::CommittedInput::enter);
                typed_keys += 1;
            }
            if !remainder.is_empty() {
                // The loop checks before each key only: a head of control characters, which
                // `typed_key_events` drops, presses none and leaves this paste the first write.
                if exited(store) {
                    return "ERR exited\n".to_string();
                }
                match hold
                    .as_mut()
                    .map(|h| h.held(term, ctx, &sub, None, cadence, &mut deadline))
                {
                    Some(Err(why)) => return typed_cut(term, why, typed_keys, 0, turn_id),
                    Some(Ok(cursor)) => sent_from = Some(cursor),
                    None => {}
                }
                if let Some(cut) = halted(committed.is_some().then_some((typed_keys, 0))) {
                    return cut;
                }
                pasted_graphemes = remainder.graphemes().count();
                if typed_keys == 0 {
                    note_ctl_input();
                }
                let first = !wrote.replace(true);
                if !(io.paste)(remainder) {
                    return undelivered(first, PASTE_DELIVERY_FAILED);
                }
                committed.get_or_insert_with(crate::control::CommittedInput::enter);
            }
        } else {
            if exited(store) {
                return "ERR exited\n".to_string();
            }
            if let Some(refusal) = halted(None) {
                return refusal;
            }
            note_ctl_input();
            let first = !wrote.replace(true);
            if !(io.paste)(text) {
                return undelivered(first, PASTE_DELIVERY_FAILED);
            }
            committed = Some(crate::control::CommittedInput::enter());
        }
        // A guarded submit judges the tail the program drew, which a starved program draws
        // later than any settle: it reads and draws the last write before its echo settles,
        // the caret moving or the cursor row already matching the guard.
        if let (Some(h), Some(from), Some(guard)) =
            (hold.as_mut(), sent_from, submit_matcher.as_deref())
            && let Err(why) = h.held(
                term,
                ctx,
                &sub,
                Some(Sent {
                    from,
                    guard: Some(guard),
                }),
                cadence,
                &mut deadline,
            )
        {
            return typed_cut(term, why, typed_keys, pasted_graphemes, turn_id);
        }
        let Some(id) = arm(WatcherSpec::IdleFor { dur: ECHO_SETTLE }) else {
            return format!("ERR {WAITS_FULL}\n");
        };
        let until = deadline.min(Instant::now() + ECHO_CAP);
        echo_until = Some(until);
        match wait_committed(id, until) {
            Parked::Exited => return "ERR exited\n".to_string(),
            Parked::Done(_) | Parked::Deadline | Parked::HungUp => {}
        }
        if let Some(Err(why)) = hold
            .as_mut()
            .map(|h| h.held(term, ctx, &sub, None, cadence, &mut deadline))
        {
            return typed_cut(term, why, typed_keys, pasted_graphemes, turn_id);
        }
    }

    let mut submitted = false;
    let mut pressed = 0u32;
    let mut unjudged = false;
    let task_found = task_record_of(ctx).map(|task| (task.rev, task.entered));
    let agent_found = {
        let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        let agent = tl.agent();
        (agent.word != "-").then_some(agent.rev)
    };
    if submit != "none" {
        let auto_mode = submit_verify.is_none();
        let submit_verify_block = submit_verify.unwrap_or_else(|| {
            matches!(
                term_lock(term).all_blocks().last().map(|b| b.state),
                Some(BlockState::PromptOnly | BlockState::EnteringCommand)
            )
        });
        let composer_key = if auto_mode && !submit_verify_block && read_as_claude(ctx) {
            let look = || {
                let t = crate::term_lock_whole_when(term, finishing, |_| true);
                (composer_verify_key(&t, text), t.content_seq())
            };
            let wait = |after| {
                let Some(until) = echo_until.filter(|&u| Instant::now() < u) else {
                    return Ok(false);
                };
                let Some(id) = arm(WatcherSpec::SeqAdvanced { after }) else {
                    return Err(format!("ERR {WAITS_FULL}\n"));
                };
                match wait_committed(id, until) {
                    Parked::Exited => Err("ERR exited\n".to_string()),
                    Parked::Done(_) => Ok(true),
                    Parked::Deadline | Parked::HungUp => Ok(false),
                }
            };
            match composer_key_once_shown(look, wait) {
                Ok(key) => key,
                Err(reply) => return reply,
            }
        } else {
            None
        };
        // No composer and no command block to read: the screen alone shows the program took the line.
        let by_screen = composer_key.is_none() && !submit_verify_block;
        for press_no in 0..presses {
            // A re-press is for an Enter the program read and swallowed; one still unread would
            // only queue another behind it (`input_stall::repress_would_queue`).
            if press_no > 0 && crate::input_stall::repress_would_queue(&ctx.sink) {
                break;
            }
            if press_no > 0 && ctx.update_parked.is_parked() {
                break;
            }
            if let Some(cut) = halted(
                committed
                    .is_some()
                    .then_some((typed_keys, pasted_graphemes)),
            ) {
                if press_no > 0 {
                    break;
                }
                return cut;
            }
            let window = deadline.min(Instant::now() + Duration::from_millis(submit_window_ms));
            let (base_started, typed_row, mut id) = {
                let mut t = term_lock(term);
                let base = t.started_command_seq();
                let row = by_screen.then(|| t.cursor().row);
                let after = t.content_seq();
                match t.watch(WatcherSpec::SeqAdvanced { after }, Instant::now()) {
                    Some(id) => (base, row, id),
                    None => return format!("ERR {WAITS_FULL}\n"),
                }
            };
            note_ctl_input();
            let first = !wrote.replace(true);
            if let Some(guard) = submit_matcher.as_deref() {
                match guarded_submit(term, ctx, guard, window, io.mark) {
                    GuardedSubmit::Pressed => {}
                    GuardedSubmit::Missed => {
                        term_lock(term).watch_disarm(id);
                        if press_no == 0 {
                            let seq = term_lock(term).content_seq();
                            return format!(
                                "OK 0 turn skipped reason=guard submitted=0 seq={seq} \
                                 id={turn_id}\n"
                            );
                        }
                        break;
                    }
                    GuardedSubmit::Busy => {
                        term_lock(term).watch_disarm(id);
                        // A re-press the sink took none of wrote nothing, as a missed one: an
                        // earlier Enter went, so the verdict says `pressed=<n>`. A first one is
                        // its own reply, never `ERR busy`: the text is typed, and that head
                        // would give an `id=` back for a retry that types it again.
                        if press_no > 0 {
                            break;
                        }
                        return aterm_types::control_verbs::TURN_SUBMIT_BUSY_LINE.to_string();
                    }
                    GuardedSubmit::Failed(reply) => {
                        term_lock(term).watch_disarm(id);
                        return reply;
                    }
                }
            } else if !(io.press)(&submit) {
                // `turn_options` refused a name that is no key, so this press failed to deliver.
                term_lock(term).watch_disarm(id);
                return undelivered(first, KEY_DELIVERY_FAILED);
            }
            pressed += 1;
            let mut advanced = false;
            let mut composer_gone = false;
            loop {
                match Park::new(&sub, &exited_now, OnHangup::Finish, stretch).run(
                    window,
                    press_look(
                        term,
                        id,
                        (submit_verify_block.then_some(base_started), typed_row),
                    ),
                    disarm_watch(term, id),
                ) {
                    Parked::Exited => return "ERR exited\n".to_string(),
                    Parked::Done(PressLook::Started | PressLook::Moved) => {
                        submitted = true;
                        break;
                    }
                    Parked::Done(PressLook::Latched) => {
                        advanced = true;
                        let mut t = term_lock(term);
                        if let Some(key) = composer_key.as_deref() {
                            match composer_after_press(&t, key) {
                                ComposerAfterPress::Left => {
                                    submitted = true;
                                    break;
                                }
                                ComposerAfterPress::Gone => composer_gone = true,
                                ComposerAfterPress::Holds => composer_gone = false,
                                ComposerAfterPress::Unfinished => {}
                            }
                        } else if !submit_verify_block || t.started_command_seq() > base_started {
                            submitted = true;
                            break;
                        }
                        if Instant::now() >= window {
                            break;
                        }
                        let after = t.content_seq();
                        match t.watch(WatcherSpec::SeqAdvanced { after }, Instant::now()) {
                            Some(nid) => id = nid,
                            None => return format!("ERR {WAITS_FULL}\n"),
                        }
                    }
                    closed @ (Parked::Deadline | Parked::HungUp) => {
                        if let Some(key) = composer_key.as_deref() {
                            match composer_after_press(&term_lock(term), key) {
                                ComposerAfterPress::Left => submitted = true,
                                ComposerAfterPress::Gone => composer_gone = true,
                                ComposerAfterPress::Holds => composer_gone = false,
                                ComposerAfterPress::Unfinished => {}
                            }
                        } else if matches!(closed, Parked::HungUp)
                            && submit_verify_block
                            && term_lock(term).started_command_seq() > base_started
                        {
                            submitted = true;
                        }
                        break;
                    }
                }
            }
            if !submitted && advanced && auto_mode && base_started == 0 && composer_key.is_none() {
                submitted = true;
            }
            if !submitted && composer_gone {
                submitted = true;
            }
            if submitted || Instant::now() >= deadline {
                break;
            }
        }
        unjudged = by_screen && !submitted && pressed > 0;
    }

    drop(committed);

    if submitted {
        crate::presence::post_lease_changed(&ctx.self_id);
        crate::task_record::reopen_at_turn(
            ctx,
            session,
            &by,
            task_found.map(|(_, entered)| entered),
        );
    }

    if lease_guard.handed_back.is_none() && (submitted || submit == "none") {
        end_turn_input(&ctx.turn_lease, turn_id, &ctx.self_id);
    }

    let mut status = "timeout";
    if settle_submitted && submitted {
        status = "submitted";
    } else if submitted || submit == "none" {
        'settle: {
            let armed = match &settle_matcher {
                Some((true, m)) => {
                    let Some(seen) = term_lock(term).watch_rows(
                        m.clone(),
                        aterm_core::terminal::RowRange::All,
                        Instant::now(),
                    ) else {
                        return format!("ERR {WAITS_FULL}\n");
                    };
                    let appear_by =
                        deadline.min(Instant::now() + Duration::from_millis(submit_window_ms));
                    match wait(seen, appear_by) {
                        Parked::Exited => return "ERR exited\n".to_string(),
                        Parked::HungUp => {
                            status = "hangup";
                            break 'settle;
                        }
                        Parked::Done(_) => term_lock(term).watch_rows_gone(
                            m.clone(),
                            aterm_core::terminal::RowRange::All,
                            Instant::now(),
                        ),
                        Parked::Deadline => arm(WatcherSpec::IdleFor {
                            dur: Duration::from_millis(idle_ms),
                        }),
                    }
                }
                Some((false, m)) => term_lock(term).watch_rows(
                    m.clone(),
                    aterm_core::terminal::RowRange::All,
                    Instant::now(),
                ),
                None => arm(WatcherSpec::IdleFor {
                    dur: Duration::from_millis(idle_ms),
                }),
            };
            let Some(id) = armed else {
                return format!("ERR {WAITS_FULL}\n");
            };
            match wait(id, deadline) {
                Parked::Exited => return "ERR exited\n".to_string(),
                Parked::HungUp => status = "hangup",
                Parked::Done(_) => status = "settled",
                Parked::Deadline => {}
            }
        }
    }

    let mut look = ctx.frame.look_frame();
    let (seq, screen, frame_open) = {
        let t = crate::term_lock_whole_when(term, finishing, |_| true);
        (
            t.content_seq(),
            look.text(&t),
            crate::frame_unfinished_now(&t),
        )
    };
    drop(look);
    let rows = screen.row_count();
    let dur_ms = now0.elapsed().as_millis() as u64;
    let screen_hash = screen.fnv();
    let turn_id = {
        let mut ledger = ctx.turns.lock().unwrap_or_else(|p| p.into_inner());
        let record = crate::turn_ledger::TurnRecord {
            id: turn_id,
            started_ms,
            dur_ms,
            submitted,
            status: if status == "submitted" {
                crate::turn_ledger::STATUS_PRESSED
            } else {
                status
            },
            text: crate::turn_ledger::clamp_text(text),
            screen_hash,
            seq,
            arch,
            by,
            carried: false,
        };
        ledger.record(record, mint_turn_id)
    };
    let mut verdict = format!(
        "turn submitted={} status={status} seq={seq} id={turn_id} dur_ms={dur_ms} hash={screen_hash:016x}",
        u8::from(submitted)
    );
    if typed {
        use std::fmt::Write as _;
        let _ = write!(verdict, " typed={typed_keys} pasted={pasted_graphemes}");
    }
    if let Some(ms) = yielded_ms {
        use std::fmt::Write as _;
        let _ = write!(verdict, " yielded_ms={ms}");
    }
    // A screen that showed no press is no proof the program left the line: the Enters are named.
    if submit_matcher.is_some() || unjudged {
        use std::fmt::Write as _;
        let _ = write!(verdict, " pressed={pressed}");
    }
    if let Some((rev, _)) = task_found {
        use std::fmt::Write as _;
        let _ = write!(verdict, " task_rev={rev}");
    }
    if let Some(rev) = agent_found {
        use std::fmt::Write as _;
        let _ = write!(verdict, " agent_rev={rev}");
    }
    verdict.push_str(aterm_control::wire::frame_open_tail(frame_open));
    super::control_query::frame_rows_reply(screen.whole(), rows, &verdict, trim, 0)
}

fn composer_words(t: &Terminal) -> Option<String> {
    let rows: Vec<String> = (0..t.rows() as usize)
        .map(|r| super::control_query::visible_row(t, r))
        .collect();
    aterm_phase::phase::composer_rules(&rows)?;
    let (caret, lines) = aterm_phase::phase::composer_draft(&rows)?;
    let cursor = t.cursor();
    if usize::from(cursor.row) == caret
        && aterm_phase::phase::is_placeholder(&rows, usize::from(cursor.col))
    {
        return Some(String::new());
    }
    Some(
        lines
            .concat()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect(),
    )
}

fn read_as_claude(ctx: &SessionCtx) -> bool {
    match ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .agent()
        .reader
    {
        Some(aterm_phase::Program::Claude) => true,
        Some(
            aterm_phase::Program::Codex
            | aterm_phase::Program::Grok
            | aterm_phase::Program::Kimi
            | aterm_phase::Program::Agy
            | aterm_phase::Program::Muse
            | aterm_phase::Program::Generic,
        )
        | None => false,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ComposerKey {
    Shows(String),
    NotYet,
    No,
}

fn composer_verify_key(t: &Terminal, text: &str) -> ComposerKey {
    let key: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let Some(shown) = composer_words(t) else {
        return ComposerKey::No;
    };
    if key.is_empty() {
        ComposerKey::No
    } else if shown.contains(&key) {
        ComposerKey::Shows(key)
    } else if shown.contains("[Pastedtext") {
        ComposerKey::No
    } else {
        ComposerKey::NotYet
    }
}

fn composer_key_once_shown(
    mut look: impl FnMut() -> (ComposerKey, u64),
    mut wait: impl FnMut(u64) -> Result<bool, String>,
) -> Result<Option<String>, String> {
    loop {
        let (seen, after) = look();
        match seen {
            ComposerKey::Shows(key) => return Ok(Some(key)),
            ComposerKey::No => return Ok(None),
            ComposerKey::NotYet => {}
        }
        if !wait(after)? {
            return Ok(None);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ComposerAfterPress {
    Left,
    Gone,
    Holds,
    Unfinished,
}

fn composer_after_press(t: &Terminal, key: &str) -> ComposerAfterPress {
    if t.sync_frame_unfinished() {
        return ComposerAfterPress::Unfinished;
    }
    match composer_words(t) {
        None => ComposerAfterPress::Gone,
        Some(words) if words.contains(key) => ComposerAfterPress::Holds,
        Some(_) => ComposerAfterPress::Left,
    }
}

/// `history [<n>] [since=<id>]` -> the session's turn ledger, newest-last, framed `OK <count>\n` + one line per record (streaming like `text`).
pub(crate) fn cmd_history(ctx: &SessionCtx, rest: &str) -> String {
    let page = match RingPage::parse(rest, "history") {
        Ok(page) => page,
        Err(usage) => return usage,
    };
    let (recs, floor) = {
        let ledger = ctx.turns.lock().unwrap_or_else(|p| p.into_inner());
        let floor = ledger.live_floor().filter(|&floor| {
            page.since
                .is_none_or(|anchor| anchor.saturating_add(1) < floor)
        });
        (page.copy(ledger.since(page.since)), floor)
    };
    history_reply(&recs, floor)
}

fn history_reply(recs: &[crate::turn_ledger::TurnRecord], floor: Option<u64>) -> String {
    use std::fmt::Write as _;
    #[cfg(test)]
    crate::session_log::encode_probe();
    let mut out = format!("OK {}", recs.len());
    if let Some(floor) = floor {
        let _ = write!(out, " unheld_below={floor}");
    }
    out.push('\n');
    for r in recs {
        #[cfg(test)]
        crate::work_counts::ring_row_formatted();
        let _ = writeln!(
            out,
            "turn {} submitted={} status={} started_ms={} dur_ms={} seq={} hash={:016x} by={} arch={}{} text={}",
            r.id,
            u8::from(r.submitted),
            r.status,
            r.started_ms,
            r.dur_ms,
            r.seq,
            r.screen_hash,
            super::pct_encode(r.by.as_wire()),
            r.arch,
            if r.carried { " carried=1" } else { "" },
            super::pct_encode(&r.text),
        );
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RingPage {
    n: usize,
    since: Option<u64>,
}

impl RingPage {
    fn parse(rest: &str, verb: &str) -> Result<RingPage, String> {
        let usage = || format!("ERR usage: {verb} [<n>] [since=<id>]\n");
        let mut page = RingPage { n: 0, since: None };
        for tok in rest.split_whitespace() {
            if let Some(v) = tok.strip_prefix("since=") {
                page.since = Some(v.parse::<u64>().map_err(|_| usage())?);
            } else if let Ok(v) = tok.parse::<usize>() {
                page.n = v;
            } else {
                return Err(usage());
            }
        }
        Ok(page)
    }

    fn copy<'a, T: Clone + 'a>(&self, suffix: impl ExactSizeIterator<Item = &'a T>) -> Vec<T> {
        let skip = match self.n {
            0 => 0,
            n => suffix.len().saturating_sub(n),
        };
        suffix.skip(skip).cloned().collect()
    }
}

/// `meta` — the session-metadata verb.
#[cfg(test)]
pub(crate) fn cmd_meta(
    term: &Arc<Mutex<Terminal>>,
    store: &Store,
    session: u64,
    ctx: &SessionCtx,
    rest: &str,
) -> (String, bool) {
    cmd_meta_by(term, store, session, ctx, rest, aterm_slog::Actor::Ctl)
}

/// `cmd_meta`, each write's session log mark by `by` (the wire's [`log_actor`]).
pub(crate) fn cmd_meta_by(
    term: &Arc<Mutex<Terminal>>,
    store: &Store,
    session: u64,
    ctx: &SessionCtx,
    rest: &str,
    by: aterm_slog::Actor,
) -> (String, bool) {
    let mut toks = rest.split_whitespace();
    match toks.next() {
        None => (meta_status(term, store, session, ctx), false),
        Some("set") => {
            let Some(field) = toks.next() else {
                return (meta_set_usage(), false);
            };
            let Some(typed) = MetaField::parse(field) else {
                return (meta_unknown_field(), false);
            };
            let value = rest
                .split_once("set")
                .and_then(|(_, r)| r.trim_start().split_once(char::is_whitespace))
                .map_or("", |(_, v)| v);
            if typed == MetaField::Autopilot
                && let Some(reply) = meta_set_autopilot_until(ctx, value, by)
            {
                return reply;
            }
            if typed == MetaField::Attention {
                if let Some((owner, text)) = leading_attention_owner(value) {
                    return match owner.and_then(|owner| {
                        leading_attention_kind(text).map(|(kind, text)| (owner, kind, text))
                    }) {
                        Ok((owner, kind, text)) => {
                            attention_owned_reply(crate::session_timeline::write_attention_kind(
                                ctx,
                                owner,
                                MetaEdit::Set(text),
                                kind,
                                by,
                            ))
                        }
                        Err(refusal) => (refusal, false),
                    };
                }
                if value.trim_start().starts_with("kind=") {
                    return (attention_set_usage(), false);
                }
            }
            meta_write_reply(
                typed,
                write_session_meta(ctx, typed, MetaEdit::Set(value), by),
            )
        }
        Some("unset") => {
            let Some(field) = toks.next() else {
                return ("ERR usage: meta unset <field>\n".to_string(), false);
            };
            let Some(field) = MetaField::parse(field) else {
                return (meta_unknown_field(), false);
            };
            if field == MetaField::Attention
                && let Some(tail) = rest.split_once("attention").map(|(_, tail)| tail)
                && let Some((owner, extra)) = leading_attention_owner(tail)
            {
                if !extra.trim().is_empty() {
                    return (
                        "ERR usage: meta unset attention [owner=<k>]\n".to_string(),
                        false,
                    );
                }
                return match owner {
                    Ok(owner) => {
                        attention_owned_reply(crate::session_timeline::write_attention_owned(
                            ctx,
                            owner,
                            MetaEdit::Clear,
                            by,
                        ))
                    }
                    Err(refusal) => (refusal, false),
                };
            }
            meta_write_reply(field, write_session_meta(ctx, field, MetaEdit::Clear, by))
        }
        Some(_) => (
            "ERR usage: meta [set <field> <text...> | unset <field>]\n".to_string(),
            false,
        ),
    }
}

fn meta_write_reply(field: MetaField, result: Result<bool, MetaWriteError>) -> (String, bool) {
    match result {
        Ok(changed) => ("OK\n".to_string(), changed),
        Err(MetaWriteError::HungUp) => (super::HUNG_UP_REPLY.to_string(), false),
        Err(MetaWriteError::NotAQuestionsPolicy) => (format!("ERR {}\n", questions_usage()), false),
        Err(MetaWriteError::Empty) if field == MetaField::Questions => {
            (format!("ERR {}\n", questions_usage()), false)
        }
        Err(MetaWriteError::NotAnAutopilot) => (format!("ERR {AUTOPILOT_USAGE}\n"), false),
        Err(MetaWriteError::Empty) if field == MetaField::Autopilot => {
            (format!("ERR {AUTOPILOT_USAGE}\n"), false)
        }
        Err(MetaWriteError::Empty) => (meta_set_usage(), false),
        Err(MetaWriteError::ForbiddenFormatting) => (
            format!(
                "ERR {} must be single-line and contain no control, bidi, or invisible formatting characters\n",
                field.wire_name()
            ),
            false,
        ),
        Err(MetaWriteError::TooLong { cap }) => (
            format!("ERR {} too long (max {cap} bytes)\n", field.wire_name()),
            false,
        ),
    }
}

fn meta_set_autopilot_until(
    ctx: &SessionCtx,
    value: &str,
    by: aterm_slog::Actor,
) -> Option<(String, bool)> {
    use crate::autopilot_timers::{local_offset_at, parse_when, resolve, wall_ms};
    let mut until: Vec<&str> = Vec::new();
    let mut word: Vec<&str> = Vec::new();
    for token in value.split_whitespace() {
        match token.strip_prefix("until=") {
            Some(when) => until.push(when),
            None => word.push(token),
        }
    }
    if until.is_empty() {
        return None;
    }
    let refuse = |reason: Option<String>| {
        let reason = reason.map_or_else(String::new, |r| format!("{r}: "));
        Some((
            format!(
                "ERR {reason}{AUTOPILOT_USAGE}
"
            ),
            false,
        ))
    };
    let [when] = until[..] else {
        return refuse(None);
    };
    match crate::session_timeline::normalize_autopilot_value(&word.join(" ")) {
        Some(stored) if stored == aterm_agent::supervise::Autopilot::Cruise.word() => {}
        _ => return refuse(Some("until= goes with cruise only".to_string())),
    }
    let Some(when) = parse_when(when) else {
        return refuse(None);
    };
    let resolved = match resolve(when, wall_ms(), &local_offset_at) {
        Ok(resolved) => resolved,
        Err(refused) => return refuse(refused.reason()),
    };
    let write = crate::session_timeline::apply_autopilot(
        ctx,
        Some(aterm_agent::supervise::Autopilot::Cruise.word().to_string()),
        Some(resolved.at_ms),
        by,
        None,
    );
    Some((
        format!(
            "OK {}
",
            resolved.reply()
        ),
        write.moved,
    ))
}

fn meta_set_usage() -> String {
    format!("ERR usage: meta set <{}> <text...>\n", MetaField::NAMES)
}

fn meta_unknown_field() -> String {
    format!("ERR unknown meta field ({})\n", MetaField::NAMES)
}

fn leading_attention_owner(value: &str) -> Option<(Result<&str, String>, &str)> {
    let value = value.trim_start();
    let (head, text) = value.split_once(char::is_whitespace).unwrap_or((value, ""));
    let owner = head.strip_prefix("owner=")?;
    if crate::session_timeline::valid_owner_token(owner) {
        Some((Ok(owner), text))
    } else {
        Some((
            Err(format!(
                "ERR usage: owner=<k> is one token of 1..{} bytes from [A-Za-z0-9._:@/-]\n",
                crate::session_timeline::META_OWNER_MAX
            )),
            text,
        ))
    }
}

fn attention_set_usage() -> String {
    "ERR usage: meta set attention owner=<k> [kind=ask|wait] <text...>\n".to_string()
}

fn leading_attention_kind(
    text: &str,
) -> Result<(Option<aterm_types::needs::AttentionKind>, &str), String> {
    let trimmed = text.trim_start();
    let (head, rest) = trimmed
        .split_once(char::is_whitespace)
        .unwrap_or((trimmed, ""));
    match head.strip_prefix("kind=") {
        None => Ok((None, text)),
        Some(word) => aterm_types::needs::AttentionKind::parse(word)
            .map(|kind| (Some(kind), rest))
            .ok_or_else(attention_set_usage),
    }
}

fn attention_owned_reply(
    result: Result<bool, crate::session_timeline::AttentionWriteError>,
) -> (String, bool) {
    use crate::session_timeline::AttentionWriteError;
    match result {
        Ok(changed) => ("OK\n".to_string(), changed),
        Err(AttentionWriteError::Meta(MetaWriteError::HungUp)) => {
            (super::HUNG_UP_REPLY.to_string(), false)
        }
        Err(AttentionWriteError::Meta(
            MetaWriteError::Empty
            | MetaWriteError::NotAQuestionsPolicy
            | MetaWriteError::NotAnAutopilot,
        )) => (attention_set_usage(), false),
        Err(AttentionWriteError::Meta(MetaWriteError::ForbiddenFormatting)) => (
            "ERR attention must be single-line and contain no control, bidi, or invisible formatting characters\n"
                .to_string(),
            false,
        ),
        Err(AttentionWriteError::Meta(MetaWriteError::TooLong { cap })) => {
            (format!("ERR attention too long (max {cap} bytes)\n"), false)
        }
        Err(AttentionWriteError::OwnersFull) => (
            format!(
                "ERR attention owners full (max {}): unset one first\n",
                crate::session_timeline::ATTENTION_OWNERS_MAX
            ),
            false,
        ),
    }
}

/// `meta set supervisor <holder> [ttl=<ms>]` / `meta unset supervisor [holder=<holder>]`: the owner-writable key that makes a running supervisor visible (`status`, `sessions` and `meta` print `supervisor=<holder|->`).
pub(crate) fn cmd_meta_supervisor(
    ctx: &SessionCtx,
    scope: Scope,
    conn: Option<u64>,
    prove: &dyn Fn(&str) -> Option<crate::self_claim::Proven>,
    rest: &str,
) -> Option<(String, bool)> {
    const USAGE: &str = "ERR usage: meta set supervisor <holder> [ttl=<ms>] | meta unset supervisor \
                         [holder=<holder>]\n";
    let mut toks = rest.split_whitespace();
    let sub = toks.next()?;
    if !matches!(sub, "set" | "unset") || toks.next() != Some("supervisor") {
        return None;
    }
    if !scope.is_owner_class() {
        return Some(("ERR denied\n".to_string(), false));
    }
    let now = crate::metrics::now_us();
    let by = log_actor(scope, ctx);
    if sub == "unset" {
        let holder = match toks.next() {
            None => None,
            Some(tok) => match tok.strip_prefix("holder=") {
                Some(h) if crate::session_timeline::valid_owner_token(h) => Some(h),
                _ => return Some((USAGE.to_string(), false)),
            },
        };
        if toks.next().is_some() {
            return Some((USAGE.to_string(), false));
        }
        let changed = match holder {
            Some(h) => crate::session_timeline::release_supervisor_held_by(ctx, h, now, by),
            None => crate::session_timeline::release_supervisor(ctx, None, now, by),
        };
        return Some(("OK\n".to_string(), changed));
    }
    let Some(holder) = toks.next() else {
        return Some((USAGE.to_string(), false));
    };
    if holder == "-" || !crate::session_timeline::valid_owner_token(holder) {
        return Some((USAGE.to_string(), false));
    }
    let words: Vec<&str> = toks.collect();
    let words = match aterm_agent::harness::contract::read(holder, &words) {
        aterm_agent::harness::contract::Words::V2(words) => words,
        aterm_agent::harness::contract::Words::Refused(why) => {
            return Some((
                format!(
                    "ERR usage: meta set supervisor <id>@<pid> contract=3 scope=<word>[,<word>…] \
                     [version=<MAJOR.MINOR.0>] [vendor=<version>]: {why}\n"
                ),
                false,
            ));
        }
        aterm_agent::harness::contract::Words::Foreign => {
            return Some(claim_reply(
                ctx,
                conn,
                crate::session_timeline::claim_supervisor_as(
                    ctx, holder, conn, None, None, now, by,
                ),
                None,
            ));
        }
        aterm_agent::harness::contract::Words::Scoped(claim) => {
            let Some(proven) = prove(holder) else {
                aterm_log::info!(
                    "harness @{}: {}'s contract 3 claim is unproven (the kernel's peer, the \
                     foreground group or the image rule); nothing is claimed",
                    ctx.self_id,
                    holder
                );
                return Some((
                    "ERR unproven: a contract 3 claim needs its own direct connection, the \
                     session's foreground group and a build of its program from atpkg\n"
                        .to_string(),
                    false,
                ));
            };
            let versions = claim.versions();
            let scope = claim.scopes.words();
            let scoped = crate::harness_claim::Scoped { proven, claim };
            let claimed = crate::session_timeline::claim_supervisor_scoped(
                ctx, holder, conn, scoped, now, by,
            );
            if claimed.as_ref().is_ok_and(|c| c.changed) {
                aterm_log::info!(
                    "harness @{}: {versions} holds the session under contract 3, scope={scope}; \
                     aterm's built-in engine stands down from exactly that",
                    ctx.self_id
                );
            }
            return Some(claim_reply(ctx, conn, claimed, None));
        }
    };
    let ttl_ms = match words.first() {
        None => None,
        Some(tok) => match tok.strip_prefix("ttl=").map(str::parse::<u64>) {
            Some(Ok(ms)) if (1..=crate::session_timeline::SUPERVISOR_TTL_MAX_MS).contains(&ms) => {
                Some(ms)
            }
            _ => return Some((USAGE.to_string(), false)),
        },
    };
    if words.len() > 1 {
        return Some((USAGE.to_string(), false));
    }
    let (bound, expires_us) = match ttl_ms {
        Some(ms) => (None, Some(now.saturating_add(ms.saturating_mul(1000)))),
        None => (conn, None),
    };
    let proven = if ttl_ms.is_some() {
        None
    } else {
        prove(holder)
    };
    let proof = proven.clone();
    let claimed = crate::session_timeline::claim_supervisor_as(
        ctx, holder, bound, expires_us, proven, now, by,
    );
    Some(claim_reply(ctx, bound, claimed, proof))
}

/// A claim's reply: `OK` (noting a connection-bound claim, and a proven self-claim's record
/// copy), or `ERR busy supervisor=<holder>`.
fn claim_reply(
    ctx: &SessionCtx,
    bound: Option<u64>,
    claimed: Result<crate::session_timeline::Claimed, String>,
    proof: Option<crate::self_claim::Proven>,
) -> (String, bool) {
    match claimed {
        Ok(claimed) => {
            if bound.is_some() {
                super::note_connection_claim();
            }
            if let Some(proven) = proof {
                crate::self_claim::claimed(
                    ctx,
                    &proven,
                    claimed.revision,
                    claimed.changed,
                    claimed.replaced.as_deref(),
                );
            }
            ("OK\n".to_string(), claimed.changed)
        }
        Err(other) => (
            format!("ERR busy supervisor={}\n", pct_encode(&other)),
            false,
        ),
    }
}

const HARNESS_ACT_LEASE_MS: u64 = 5_000;
const HARNESS_ACT_USAGE: &str =
    "ERR usage: meta prepare harness_act | meta set harness_act <act> [until_us=<n>]\n";

enum HarnessActRequest<'a> {
    Prepare,
    Set { act: &'a str, until_us: Option<u64> },
}

/// Counts and report state read together before a task-face act is admitted: a landing's or a point left to a person's mark, and an opening act's turn (`SessionTimeline::note_typed_act`, read only at rest).
#[derive(Clone, Copy)]
pub(crate) struct HarnessActSnapshot {
    pub mark: crate::session_timeline::LandingMark,
    pub left_at_rest: bool,
}

fn harness_act_request(rest: &str) -> Option<Result<HarnessActRequest<'_>, &'static str>> {
    let mut toks = rest.split_whitespace();
    let sub = toks.next()?;
    if !matches!(sub, "prepare" | "set" | "unset") || toks.next() != Some("harness_act") {
        return None;
    }
    let parsed = match sub {
        "prepare" if toks.next().is_none() => Some(HarnessActRequest::Prepare),
        "set" => toks.next().and_then(|act| {
            if !crate::session_timeline::valid_owner_token(act) {
                return None;
            }
            let until_us = match toks.next() {
                None => None,
                Some(tok) => {
                    let value = tok.strip_prefix("until_us=")?;
                    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                        return None;
                    }
                    Some(value.parse::<u64>().ok()?)
                }
            };
            toks.next()
                .is_none()
                .then_some(HarnessActRequest::Set { act, until_us })
        }),
        _ => None,
    };
    Some(parsed.ok_or(HARNESS_ACT_USAGE))
}

/// `meta set harness_act <act> [until_us=<n>]` — an agent that supervises itself records an act it took on the session, so who is driving a session is seen per session.
pub(crate) fn cmd_meta_harness_act(
    ctx: &SessionCtx,
    snapshot: Option<HarnessActSnapshot>,
    conn: Option<u64>,
    by: aterm_slog::Actor,
    prove: &dyn Fn(&str) -> Option<crate::self_claim::Proven>,
    rest: &str,
) -> Option<(String, bool)> {
    cmd_meta_harness_act_with_clock(
        ctx,
        snapshot,
        conn,
        by,
        prove,
        rest,
        &crate::metrics::now_us,
    )
}

pub(super) fn cmd_meta_harness_act_with_clock(
    ctx: &SessionCtx,
    snapshot: Option<HarnessActSnapshot>,
    conn: Option<u64>,
    by: aterm_slog::Actor,
    prove: &dyn Fn(&str) -> Option<crate::self_claim::Proven>,
    rest: &str,
    now_us: &dyn Fn() -> u64,
) -> Option<(String, bool)> {
    let request = match harness_act_request(rest)? {
        Ok(request) => request,
        Err(usage) => return Some((usage.to_string(), false)),
    };
    let holder = {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        meta.live_claim(now_us()).and_then(|claim| {
            let proven = claim.proven.as_ref()?;
            let bound = conn.is_some() && claim.conn == conn;
            Some((
                claim.holder.clone(),
                proven.clone(),
                bound,
                meta.supervisor_revision,
            ))
        })
    };
    let allowed = holder
        .filter(|(holder, proven, bound, _)| *bound || prove(holder).as_ref() == Some(proven));
    let Some((holder, proven, _, revision)) = allowed else {
        return Some(("ERR denied\n".to_string(), false));
    };
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    if meta.supervisor_revision != revision
        || meta
            .live_claim(now_us())
            .and_then(|claim| claim.proven.as_ref())
            != Some(&proven)
    {
        return Some(("ERR denied\n".to_string(), false));
    }
    let HarnessActRequest::Set { act, until_us } = request else {
        let Some(until_us) = now_us().checked_add(HARNESS_ACT_LEASE_MS * 1_000) else {
            return Some(("ERR clock unavailable\n".to_string(), false));
        };
        return Some((
            format!("OK until_us={until_us} lease_ms={HARNESS_ACT_LEASE_MS}\n"),
            false,
        ));
    };
    {
        let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        let now = now_us();
        if meta.live_claim(now).and_then(|claim| claim.proven.as_ref()) != Some(&proven) {
            return Some(("ERR denied\n".to_string(), false));
        }
        if let Some(until_us) = until_us {
            if now >= until_us {
                return Some(("ERR expired harness_act\n".to_string(), false));
            }
            if until_us - now > HARNESS_ACT_LEASE_MS * 1_000 {
                return Some((HARNESS_ACT_USAGE.to_string(), false));
            }
            if crate::harness_task::self_claim_act_face(act)
                == Some(aterm_agent::harness::acts::TaskFace::Theirs)
                && !snapshot.is_some_and(|point| point.left_at_rest)
            {
                return Some(("ERR busy harness_act\n".to_string(), false));
            }
        }
        timeline.note_harness_act(act, &holder, by);
        let face = crate::harness_task::self_claim_act_face(act);
        if face.is_some_and(crate::harness_task::holds_until_another_turn) {
            let typed_at = timeline.take_typed_turn();
            if let Some(point) = snapshot {
                let mut mark = point.mark;
                if face == Some(aterm_agent::harness::acts::TaskFace::Landed) {
                    mark = crate::harness_task::landing_mark(mark, typed_at);
                    aterm_log::info!(
                        "task: @{}: landing marked at program turns {} (counted {}; the typed turn's {typed_at:?}; at rest {})",
                        ctx.self_id.as_str(),
                        mark.program_turns,
                        point.mark.program_turns,
                        point.left_at_rest
                    );
                }
                timeline.mark_landing(mark);
            }
        } else if face == Some(aterm_agent::harness::acts::TaskFace::Open)
            && aterm_agent::harness::acts::self_claim_types_turn(act)
        {
            let at_rest = snapshot
                .filter(|point| point.left_at_rest)
                .map(|point| point.mark);
            if at_rest.is_none() {
                aterm_log::info!(
                    "task: @{}: {act} came with the program in a turn: its turn is not owed to a landing ({:?})",
                    ctx.self_id.as_str(),
                    snapshot.map(|point| point.mark)
                );
            }
            timeline.note_typed_act(at_rest);
        }
        if let Some(face) = face {
            timeline.queue_self_task(face);
        }
        let _ = timeline.slog_take_wake();
    }
    Some(("OK\n".to_string(), true))
}

/// The face a recorded `meta set harness_act <act>` tells ([`crate::harness_task::self_claim_act_face`]), for the meta arm's change wake: `landed` for codex-alab's `landed@v1`, `theirs` for its `left-to-person@v1` (a person's own exchange left to them), `open` for any other act save its re-exec (`upgrade@v1`, no face); `None` for that one and for every other `meta` line.
pub(crate) fn harness_act_face(rest: &str) -> Option<aterm_agent::harness::acts::TaskFace> {
    let HarnessActRequest::Set { act, .. } = harness_act_request(rest)?.ok()? else {
        return None;
    };
    crate::harness_task::self_claim_act_face(act)
}

/// Whether `meta <rest>` is a `set harness_act` that reads the session's counts ([`HarnessActSnapshot`], under `crate::act_barrier`): a face that stands until another turn (its mark, and a point left to a person's rest check) or an opening act that types a turn ([`aterm_agent::harness::acts::self_claim_types_turn`]). Any other act reads none and waits for no barrier.
pub(crate) fn harness_act_reads_counts(rest: &str) -> bool {
    let Some(Ok(HarnessActRequest::Set { act, .. })) = harness_act_request(rest) else {
        return false;
    };
    match crate::harness_task::self_claim_act_face(act) {
        Some(face) if crate::harness_task::holds_until_another_turn(face) => true,
        Some(aterm_agent::harness::acts::TaskFace::Open) => {
            aterm_agent::harness::acts::self_claim_types_turn(act)
        }
        _ => false,
    }
}

/// `meta set questions|autopilot …` / `meta unset questions|autopilot` from a connection that is not Owner-class: `ERR denied`.
pub(crate) fn meta_questions_denied(scope: Scope, rest: &str) -> Option<(String, bool)> {
    let mut toks = rest.split_whitespace();
    let sub = toks.next()?;
    if !matches!(sub, "set" | "unset") || !matches!(toks.next(), Some("questions" | "autopilot")) {
        return None;
    }
    (!scope.is_owner_class()).then(|| ("ERR denied\n".to_string(), false))
}

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
    let (pgid, worker, trusted_cwd) = {
        let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        (
            timeline.agent().program_pgid,
            timeline.task().and_then(|t| t.agent),
            timeline.task().and_then(|t| t.trusted_cwd.clone()),
        )
    };
    let agent_cwd = {
        u32::try_from(pgid)
            .ok()
            .filter(|p| *p > 0)
            .and_then(aterm_sysprobe::process_cwd)
            .map(|p| p.to_string_lossy().into_owned())
    };
    let opt = |v: Option<&str>| match v {
        None => "-".to_string(),
        Some("-") => "%2D".to_string(),
        Some(v) => pct_encode(v),
    };
    let (autopilot_by, autopilot_since) = autopilot_by_fields(&meta);
    format!(
        "OK title={} user_title={} description={} icon={} role={} attention={} cwd={} state={state} \
         attention_owner={} attention_owners={} attention_kinds={} supervisor={} supervisor_proven={} \
         questions={} agent_cwd={} trusted_cwd={} worker={} autopilot={} land_at={} \
         autopilot_by={} autopilot_since={}\n",
        pct_encode(&title),
        opt(meta.user_title.as_deref()),
        opt(meta.description.as_deref()),
        opt(meta.icon.as_deref()),
        opt(meta.role.as_deref()),
        opt(meta.attention.as_deref()),
        opt(cwd.as_deref()),
        opt(attention_owner(&meta)),
        attention_owners(&meta),
        attention_kinds(&meta),
        opt(meta.live_supervisor(crate::metrics::now_us())),
        u8::from(meta.claim_proven(crate::metrics::now_us())),
        opt(meta.questions.as_deref()),
        opt(agent_cwd.as_deref()),
        opt(trusted_cwd.as_deref()),
        worker.map_or("-", aterm_phase::Program::name),
        opt(meta.autopilot.as_deref()),
        crate::autopilot::land_at_field(meta.land_at),
        autopilot_by,
        autopilot_since,
    )
}

/// `meta`'s `autopilot_by=` and `autopilot_since=` (`docs/DESIGN-harness-autopilot-2026-10-15.md` §11.7.2): who set the Autopilot pair — `human:chip`, `human:border`, `human:launch`, `human:menu`, `human:settings`, `human:every`, `ctl`, `harness`, `bridge`, `aterm` (the timer's fire), a session's sid (its edge connection), `-` for none attributable ([`crate::session_timeline::AutopilotSetBy::word`]) — and when, unix seconds.
pub(crate) fn autopilot_by_fields(meta: &crate::session_timeline::SessionMeta) -> (String, String) {
    match (meta.autopilot_set_by, meta.autopilot.is_some()) {
        (Some(set_by), _) => (set_by.word(), set_by.since_ms.div_euclid(1000).to_string()),
        (None, true) => ("restored".to_string(), "-".to_string()),
        (None, false) => ("-".to_string(), "-".to_string()),
    }
}

fn attention_kinds(meta: &crate::session_timeline::SessionMeta) -> String {
    if meta.attention_owners.is_empty() && meta.attention.is_some() {
        return format!(
            "{}:{}",
            aterm_types::needs::AttentionKind::Ask.word(),
            crate::session_timeline::BARE_ATTENTION_OWNER
        );
    }
    meta.attention_owners.kinds_field()
}

fn attention_owner(meta: &crate::session_timeline::SessionMeta) -> Option<&str> {
    match meta.attention_owners.effective_owner() {
        Some(owner) => Some(owner),
        None => meta
            .attention
            .as_ref()
            .map(|_| crate::session_timeline::BARE_ATTENTION_OWNER),
    }
}

fn attention_owners(meta: &crate::session_timeline::SessionMeta) -> usize {
    match meta.attention_owners.len() {
        0 => usize::from(meta.attention.is_some()),
        n => n,
    }
}

/// `timeline [<n>] [since=<id>]` -> the session's event timeline, oldest first, framed `OK <count>\n` + one `event <id> t=<ms> kind=<k> <payload>` line per row, with `history`'s window grammar, read from the session log's view: its rows' `Arc`s copied under the timeline's lock and formatted after it.
pub(crate) fn cmd_timeline(ctx: &SessionCtx, rest: &str) -> String {
    let page = match RingPage::parse(rest, "timeline") {
        Ok(page) => page,
        Err(usage) => return usage,
    };
    let copy = {
        let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        tl.view_copy(page.since, page.n)
    };
    timeline_reply(&copy.rows())
}

fn timeline_reply(events: &[crate::session_timeline::TimelineEvent]) -> String {
    use std::fmt::Write as _;
    #[cfg(test)]
    crate::session_log::encode_probe();
    let mut out = format!("OK {}\n", events.len());
    for e in events {
        #[cfg(test)]
        crate::work_counts::ring_row_formatted();
        let _ = writeln!(
            out,
            "event {} t={} kind={} {}",
            e.id, e.t_ms, e.kind, e.payload
        );
    }
    out
}

/// `log [<node>] [lvl=] [since=<seq> ep=<epoch>] [n=] [kinds=] [class=turn] [tier=content] [--json]`: the target session's session log ([`crate::session_log`]), a pure reader of the ctx's timeline lock like `timeline`, cross-session correct.
pub(crate) fn cmd_log(ctx: &SessionCtx, rest: &str) -> String {
    let query = match crate::session_log::LogQuery::parse(rest) {
        Ok(q) => q,
        Err(usage) => return usage.to_string(),
    };
    let copy = {
        let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        crate::session_log::LogCopy::copied_from(tl.slog().log(), &query)
    };
    crate::session_log::render_log_copy(&copy, &query)
}

/// `log --json` (the dispatcher's `--json`/`json` route, the flag already taken off `rest`): [`cmd_log`] with one JSON object per body line, the cursor's gap included (`{"op":"gap","why":…,"lost":…,"resync_to":…}`).
pub(crate) fn cmd_log_json(ctx: &SessionCtx, rest: &str) -> String {
    cmd_log(ctx, &format!("{rest} --json"))
}

/// `log` of a session that has closed in this process ([`cmd_log`]'s reply, `json` spelled either way, over the closed-session index's copy of its log, `session_log::ClosedLog::source`): no lock is held.
pub(crate) fn cmd_log_closed(log: &aterm_slog::Log, rest: &str) -> String {
    let rest = match crate::control::take_json_flag(rest) {
        (true, body) => format!("{body} --json"),
        (false, _) => rest.to_string(),
    };
    let query = match crate::session_log::LogQuery::parse(&rest) {
        Ok(q) => q,
        Err(usage) => return usage.to_string(),
    };
    crate::session_log::render_log_copy(
        &crate::session_log::LogCopy::copied_from(log, &query),
        &query,
    )
}

/// `brief` of a session that has closed in this process (`cmd_brief`'s reply over the closed-session index's copy of its log, as of the wall clock now); `brief all` is the instance's, as from a live session.
pub(crate) fn cmd_brief_closed(
    log: &aterm_slog::Log,
    sid: &str,
    store: &Store,
    rest: &str,
) -> String {
    let query = match crate::session_log::BriefQuery::parse(rest) {
        Ok(q) => q,
        Err(usage) => return usage.to_string(),
    };
    cmd_brief_closed_query(log, sid, store, &query, None)
}

/// [`cmd_brief_closed`] of a parsed `query` (the journal's reader places its cursor first), its header handing out `hand` in place of the log's head when given (a journal reply's, `aterm_slog::journal::View::reply_cursor`).
pub(crate) fn cmd_brief_closed_query(
    log: &aterm_slog::Log,
    sid: &str,
    store: &Store,
    query: &crate::session_log::BriefQuery,
    hand: Option<aterm_slog::Cursor>,
) -> String {
    if query.all {
        return brief_all(store, query.budget);
    }
    let copy = crate::session_log::BriefCopy::copied_from(log, query.node, query.cursor);
    let copy = match hand {
        Some(c) => copy.handing(c),
        None => copy,
    };
    let mut text = copy
        .brief(
            sid,
            query.budget.unwrap_or(aterm_slog::DEFAULT_BUDGET),
            crate::session_log::host_stamp().wall_ms,
        )
        .text;
    text.push('\n');
    text
}

/// `brief [<node>] [budget=<tokens>] [since=<seq> ep=<epoch>]`: the target session's token-budgeted rollup for an AI reader (`docs/DESIGN-session-log-2026-10-15.md` §7.4), from its session log; `brief all [budget=]` is the instance's, every session's in turn within one budget (Owner-only, escalated by `control::argument_escalation`, like `sessions`).
pub(crate) fn cmd_brief_with_jobs(
    ctx: &SessionCtx,
    store: &Store,
    rest: &str,
    jobs: Option<String>,
) -> String {
    let query = match crate::session_log::BriefQuery::parse(rest) {
        Ok(q) => q,
        Err(usage) => return usage.to_string(),
    };
    if query.all {
        return brief_all(store, query.budget);
    }
    let (copy, now) = {
        let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        (
            crate::session_log::BriefCopy::copied_from(tl.slog().log(), query.node, query.cursor),
            tl.slog().now().wall_ms,
        )
    };
    let budget = query.budget.unwrap_or(aterm_slog::DEFAULT_BUDGET);
    let sid = ctx.self_id.as_str();
    let fitted = jobs.and_then(|line| {
        let reserve = aterm_slog::tokens(line.len() + 1);
        let b = copy.brief(sid, budget.saturating_sub(reserve), now);
        (b.level < 4).then(|| crate::sys_host::splice_brief_line(&b.text, &line, budget))
    });
    let mut text = fitted.unwrap_or_else(|| copy.brief(sid, budget, now).text);
    text.push('\n');
    text
}

/// [`cmd_brief_with_jobs`] with no jobs line, for the session log's tests.
#[cfg(test)]
pub(crate) fn cmd_brief(ctx: &SessionCtx, store: &Store, rest: &str) -> String {
    cmd_brief_with_jobs(ctx, store, rest, None)
}

fn brief_all(store: &Store, budget: Option<usize>) -> String {
    let handles = store.read().unwrap_or_else(|p| p.into_inner()).snapshot();
    let budget = budget.unwrap_or(aterm_slog::INSTANCE_BUDGET);
    let parts: Vec<crate::session_log::BriefPart> = handles
        .iter()
        .map(|h| {
            let tl = h.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            crate::session_log::BriefPart {
                sid: h.sid.as_str().to_string(),
                copy: crate::session_log::BriefCopy::copied_from(
                    tl.slog().log(),
                    aterm_slog::Node::SESSION,
                    None,
                ),
                now_wall: tl.slog().now().wall_ms,
            }
        })
        .collect();
    crate::session_log::render_brief_all(&parts, budget)
}

/// `cast` -> `OK <nbytes>\n` then the session's full asciicast v2 recording as the body.
pub(crate) fn cmd_cast(cast: &Mutex<crate::cast::CastRecorder>) -> String {
    const HEAD_SLOT: &str = "OK 18446744073709551615\n";
    let snapshot = {
        let rec = cast.lock().unwrap_or_else(|p| p.into_inner());
        rec.snapshot()
    };
    let (header, events) = snapshot.into_cast();
    let mut reply = String::from(HEAD_SLOT);
    aterm_control::asciicast::write_into(&mut reply, &header, &events);
    let head = format!("OK {}\n", reply.len() - HEAD_SLOT.len());
    reply.replace_range(..HEAD_SLOT.len(), &head);
    reply
}

/// `cast frames [count=N]` -> expand the compressed `cast` recording into `count` evenly-spaced keyframe SCREENS (default 8) — the recording read as a "video" FLIPBOOK an AI can watch: raw `cast` is the compact, sendable asciicast; this is its readable expansion.
pub(crate) fn cmd_cast_frames(
    ctx: &SessionCtx,
    policy: aterm_core::grid::ResizePolicy,
    rest: &str,
) -> String {
    let mut count = 8usize;
    for tok in rest.split_whitespace() {
        match tok.strip_prefix("count=").map(|v| v.parse::<usize>()) {
            Some(Ok(n)) if n >= 1 => count = n.min(240),
            _ => return "ERR usage: cast frames [count=N]\n".to_string(),
        }
    }
    let snapshot = {
        let rec = ctx.cast.lock().unwrap_or_else(|p| p.into_inner());
        rec.snapshot()
    };
    let frames = snapshot.fold_frames(count, policy);
    let total = frames.len();
    let mut body = String::new();
    if snapshot.evicted() > 0 {
        body.push_str(&format!(
            "--- WARNING: {} head event(s) evicted (budget overflow); leading state incomplete, early frames may be wrong ---\n",
            snapshot.evicted()
        ));
    }
    if let Some(gap) = snapshot.handoff() {
        body.push_str(&format!(
            "--- WARNING: recording restarted at a seamless update (handoff {}); the screen before it is not recorded, early frames may be wrong ---\n",
            gap.payload()
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

const CAST_DRIFT_CATCH_UP: std::time::Duration = std::time::Duration::from_millis(60);

const CAST_DRIFT_CUT_ATTEMPTS: usize = 3;

const CAST_DRIFT_CONCURRENCY: usize = 2;

const CAST_DRIFT_SLOT_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

static CAST_DRIFT_SLOTS: DriftSlots = DriftSlots {
    busy: Mutex::new(0),
    freed: std::sync::Condvar::new(),
};

struct DriftSlots {
    busy: Mutex<usize>,
    freed: std::sync::Condvar,
}

struct DriftSlot(&'static DriftSlots);

impl DriftSlots {
    fn acquire(&'static self, wait: std::time::Duration) -> Option<DriftSlot> {
        let busy = self.busy.lock().unwrap_or_else(|p| p.into_inner());
        let (mut busy, timeout) = self
            .freed
            .wait_timeout_while(busy, wait, |busy| *busy >= CAST_DRIFT_CONCURRENCY)
            .unwrap_or_else(|p| p.into_inner());
        if timeout.timed_out() && *busy >= CAST_DRIFT_CONCURRENCY {
            return None;
        }
        *busy += 1;
        Some(DriftSlot(self))
    }
}

impl Drop for DriftSlot {
    fn drop(&mut self) {
        let mut busy = self.0.busy.lock().unwrap_or_else(|p| p.into_inner());
        *busy = busy.saturating_sub(1);
        drop(busy);
        self.0.freed.notify_one();
    }
}

/// `cast drift [max_runs=<k>] [rows=<n>] [seed=auto|alt|primary]` -> the counterfactual replay audit of this session's asciicast against its live screen (`aterm_control::cast_drift`, which owns the method and the wire): `OK <n> verdict=<clean|desync|unfaithful|unknown|empty> fidelity=… culprit=… … cut=<quiescent|racy> folds=<f> fold_events=<e> ms=<ms>` then `n` `run`/`row`/`cursor` lines, parse by key.
pub(crate) fn cmd_cast_drift(
    term: &Arc<Mutex<Terminal>>,
    frame: &crate::frame_memo::FrameSlot,
    cast: &Mutex<crate::cast::CastRecorder>,
    policy: impl Into<aterm_control::cast_drift::Posture>,
    rest: &str,
) -> String {
    cast_drift_with(term, frame, cast, policy, rest, CAST_DRIFT_CATCH_UP, &|| {})
}

fn cast_drift_with(
    term: &Arc<Mutex<Terminal>>,
    frame: &crate::frame_memo::FrameSlot,
    cast: &Mutex<crate::cast::CastRecorder>,
    policy: impl Into<aterm_control::cast_drift::Posture>,
    rest: &str,
    catch_up: std::time::Duration,
    between: &dyn Fn(),
) -> String {
    use aterm_control::cast_drift::{Cut, DriftArgs, LiveScreen, analyze, analyze_replay};
    let args = match DriftArgs::parse(rest) {
        Ok(args) => args,
        Err(usage) => return format!("{usage}\n"),
    };
    let Some(_slot) = CAST_DRIFT_SLOTS.acquire(CAST_DRIFT_SLOT_WAIT) else {
        return format!(
            "ERR busy: {CAST_DRIFT_CONCURRENCY} cast drift analyses are running in this instance; \
             ask again\n"
        );
    };
    let watermark = cast.lock().unwrap_or_else(|p| p.into_inner()).watermark();
    let mut attempt = 0;
    let (live, snapshot, quiescent) = loop {
        attempt += 1;
        let mut look = frame.look_frame();
        let (text, cursor, alt, cut, whole) = {
            let t = crate::term_lock_whole(term);
            let c = t.cursor();
            (
                look.text(&t),
                (c.row, c.col),
                t.is_alternate_screen(),
                crate::cast::CastCut::take(&t, &watermark),
                !crate::frame_unfinished_now(&t),
            )
        };
        drop(look);
        let live = LiveScreen {
            rows: text.rows().map(str::to_string).collect(),
            cursor: Some(cursor),
            alt: Some(alt),
        };
        between();
        let (snapshot, caught_up) = crate::cast::CastRecorder::snapshot_at(cast, &cut, catch_up);
        let quiescent = caught_up && whole;
        if quiescent || attempt >= CAST_DRIFT_CUT_ATTEMPTS {
            break (live, snapshot, quiescent);
        }
    };
    let (header, events) = snapshot.into_cast();
    let started = std::time::Instant::now();
    let mut report = if quiescent {
        analyze(&header, &events, Some(&live), &args, policy)
    } else {
        analyze_replay(&header, &events, live.alt, &args, policy)
    };
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    report.settle_cut(if quiescent { Cut::Quiescent } else { Cut::Racy });
    report.render(ms, None)
}

/// `temporal [tick] [trim]` -> `OK <nbytes>[ trimmed=<k>]\n` then the session's screen RECONSTRUCTED at logical `tick` (default: the latest recorded instant) — with `trim`, minus its trailing all-blank rows (the `text trim` rule; the byte count on the header is the trimmed body's) — the read half of the hydratable temporal spine (`docs/design/HIERARCHICAL_SESSIONS.md` Addendum B), the endpoint the `TemporalRecorder` producer feeds.
pub(crate) fn cmd_temporal(ctx: &SessionCtx, term: &Mutex<Terminal>, rest: &str) -> String {
    const USAGE: &str = "ERR usage: temporal [status | <tick>] [trim]  (tick = µs since \
                         session start; `temporal status` reports the reachable range)\n";
    let (rest, trim) = super::control_query::split_trim_tail(rest);
    let pending = || {
        let spine = ctx
            .temporal
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .spine_watermark();
        spine.pending(&term_lock(term))
    };
    if rest == "status" {
        if trim {
            return USAGE.to_string();
        }
        let pending = pending();
        let rec = ctx.temporal.lock().unwrap_or_else(|p| p.into_inner());
        let enabled = rec.total_events() > 0;
        let reachable_from = rec
            .reachable_from()
            .map_or_else(|| "-".to_string(), |tick| tick.0.to_string());
        return format!(
            "OK enabled={enabled} latest_tick={} reachable_from={reachable_from} keyframes={} \
             live_events={} dropped_events={} lost_resizes={} dropped_input={} \
             dropped_replies={} pending_resizes={} witnessed={} diverged={}{}\n",
            rec.latest_tick().0,
            rec.keyframe_count(),
            rec.live_events(),
            rec.dropped_events(),
            rec.lost_resizes(),
            rec.dropped_input(),
            rec.dropped_replies(),
            if enabled { pending } else { 0 },
            rec.witnessed(),
            rec.diverged(),
            rec.status_tail(),
        );
    }
    let at = match rest {
        "" => None,
        s => match s.parse::<u64>() {
            Ok(t) => Some(aterm_buffer::Ticks(t)),
            Err(_) => return USAGE.to_string(),
        },
    };
    let pending = pending();
    let (enabled, plan, at_end) = {
        let rec = ctx.temporal.lock().unwrap_or_else(|p| p.into_inner());
        let enabled = rec.total_events() > 0;
        let plan = if enabled { rec.replay_plan(at) } else { None };
        (
            enabled,
            plan,
            at.is_none_or(|tick| tick >= rec.latest_tick()),
        )
    };
    if !enabled {
        return "ERR temporal: this session is not recording; set temporal_recording = true in \
                aterm.toml and open a new tab\n"
            .to_string();
    }
    let Some(plan) = plan else {
        return "ERR temporal unreachable\n".to_string();
    };
    let crate::temporal::Fold { term, judged } = plan.fold();
    ctx.temporal
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .judge(judged);
    let pending = if at_end && pending > 0 {
        format!(" pending_resizes={pending}")
    } else {
        String::new()
    };
    let rows = term.rows() as usize;
    let mut body = super::screen_text(&term);
    if trim {
        let sent = super::control_query::trimmed_len(body.lines());
        let keep: usize = body.split_inclusive('\n').take(sent).map(str::len).sum();
        body.truncate(keep);
        return format!(
            "OK {} trimmed={}{pending}\n{}",
            body.len(),
            rows - sent,
            body
        );
    }
    format!("OK {}{pending}\n{}", body.len(), body)
}

const WAITS_FULL: &str = "too many waits open on this session; retry when one ends";

const GRANT_UNKNOWN_OP: &str =
    "ERR unknown op (read-screen|write-input|signal|config-write|clipboard-write|derive-loop)\n";

/// `grant <src-id> <op>` -> mint an edge (src -> this session, op) and return its bearer token hex.
pub(crate) fn cmd_grant(ctx: &SessionCtx, scope: Scope, rest: &str) -> String {
    if !scope.is_owner_class() {
        return "ERR denied\n".to_string();
    }
    let mut it = rest.split_whitespace();
    let (Some(src), Some(op_s)) = (it.next(), it.next()) else {
        return "ERR usage: grant <src-id> <op>\n".to_string();
    };
    let Some(op) = Op::parse(op_s) else {
        return GRANT_UNKNOWN_OP.to_string();
    };
    if let Some(refusal) = reserved_src(src) {
        return refusal;
    }
    let src = SessionId::new(src);
    let tok = {
        let mut edges = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
        edges.grant(src.clone(), ctx.self_id.clone(), op, ctx.nonce)
    };
    session_edge_audit::emit(EdgeAction::Grant, "wire", &src, &ctx.self_id, op.as_str());
    format!("OK {}\n", tok.to_hex())
}

fn reserved_src(src: &str) -> Option<String> {
    aterm_slog::is_reserved_actor_word(src).then(|| {
        format!(
            "ERR reserved: src={src} is a session log actor (human ctl harness bridge aterm \
             vendor -), never a session\n"
        )
    })
}

/// `revoke <edge-hex>` -> remove one edge by its bearer token.
pub(crate) fn cmd_revoke(ctx: &SessionCtx, scope: Scope, rest: &str) -> String {
    if !scope.is_owner_class() {
        return "ERR denied\n".to_string();
    }
    revoke_in(&ctx.edges, &ctx.self_id, rest)
}

/// The `revoke` verb: [`cmd_revoke`] on the connection's own session, and a `revoke <edge-hex>` it finds no row for goes on to the read grants the closed-session index keeps (`session_log::ClosedLog`): a grant a session closed with is revoked by its token as a live one is, from any owner connection (the session it was on has no shell left to ask).
pub(crate) fn cmd_revoke_with_closed(
    ctx: &SessionCtx,
    store: &Store,
    scope: Scope,
    rest: &str,
) -> String {
    const NO_SUCH_EDGE: &str = "ERR no such edge\n";
    let live = cmd_revoke(ctx, scope, rest);
    if live != NO_SUCH_EDGE {
        return live;
    }
    if EdgeToken::from_hex(rest.trim()).is_none() {
        return live;
    }
    let closed = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .closed_logs()
        .read_tables();
    for (sid, reads) in closed {
        let reply = revoke_in(&reads, &sid, rest);
        if reply != NO_SUCH_EDGE {
            return reply;
        }
    }
    live
}

fn revoke_in(edges: &Mutex<EdgeTable>, dst: &SessionId, rest: &str) -> String {
    let rest = rest.trim();
    if let Some(src) = rest.strip_prefix("src=") {
        if src.is_empty() {
            return "ERR usage: revoke <edge-hex> | revoke src=<sid>\n".to_string();
        }
        let src = SessionId::new(src);
        let removed = edges
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .revoke_src(&src);
        if removed == 0 {
            return "ERR no such edge\n".to_string();
        }
        session_edge_audit::emit(EdgeAction::RevokeSrc, "wire", &src, dst, "*");
        return format!("OK {removed}\n");
    }
    let Some(tok) = EdgeToken::from_hex(rest) else {
        return "ERR bad token\n".to_string();
    };
    let removed = {
        let mut edges = edges.lock().unwrap_or_else(|p| p.into_inner());
        let row = edges.edge_of(&tok);
        if edges.revoke(&tok) { row } else { None }
    };
    let Some(edge) = removed else {
        return "ERR no such edge\n".to_string();
    };
    session_edge_audit::emit(
        EdgeAction::Revoke,
        "wire",
        &edge.src,
        &edge.dst,
        edge.op.as_str(),
    );
    "OK\n".to_string()
}

fn parse_connection_args(rest: &str) -> Result<(SessionId, SessionId, Option<ConnectionKind>), ()> {
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

/// `connect dst=<sid> src=<sid> [kind=pull|push|both]` against the process-wide connection store.
pub(crate) fn cmd_connect(store: &Store, scope: Scope, rest: &str) -> String {
    cmd_connect_in(&crate::connections::connections(), store, scope, rest)
}

/// The `connect` verb: set the connection `src → dst` to exactly `kind` (default `both`) through [`crate::connections`], one atomic mint+revoke audited on `session_edge` with origin `wire`.
pub(crate) fn cmd_connect_in(
    conn: &crate::connections::ConnectionStore,
    store: &Store,
    scope: Scope,
    rest: &str,
) -> String {
    if !scope.is_owner_class() {
        return "ERR denied\n".to_string();
    }
    let Ok((dst, src, kind)) = parse_connection_args(rest) else {
        return "ERR usage: connect dst=<sid> src=<sid> [kind=pull|push|both]\n".to_string();
    };
    if let Some(refusal) = reserved_src(src.as_str()) {
        return refusal;
    }
    let kind = kind.unwrap_or(ConnectionKind::Both);
    if src == dst {
        return "ERR self-loop\n".to_string();
    }
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

/// `disconnect dst=<sid> src=<sid> [kind=…]` against the process-wide connection store.
pub(crate) fn cmd_disconnect(store: &Store, scope: Scope, rest: &str) -> String {
    cmd_disconnect_in(&crate::connections::connections(), store, scope, rest)
}

/// The `disconnect` verb: dissolve the connection `src → dst`, only the `kind=` half when given.
pub(crate) fn cmd_disconnect_in(
    conn: &crate::connections::ConnectionStore,
    store: &Store,
    scope: Scope,
    rest: &str,
) -> String {
    if !scope.is_owner_class() {
        return "ERR denied\n".to_string();
    }
    let Ok((dst, src, kind)) = parse_connection_args(rest) else {
        return "ERR usage: disconnect dst=<sid> src=<sid> [kind=pull|push|both]\n".to_string();
    };
    if src == dst {
        return "ERR self-loop\n".to_string();
    }
    let dst_edges = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        match g.by_sid(&dst) {
            Some(h) => Arc::clone(&h.ctx.edges),
            None => match g.closed_logs().by_sid(&dst) {
                Some(closed) => closed.reads(),
                None => return "ERR no such session\n".to_string(),
            },
        }
    };
    let revoked =
        crate::connections::disconnect_kind_in(conn, &src, &dst, &dst_edges, kind, "wire");
    match revoked {
        Some(n) => format!("OK {n}\n"),
        None => "ERR no such connection\n".to_string(),
    }
}

/// `flows [--json]` -> the instance's aggregated session-connection graph: every live edge row across every registered session's table, `OK <n>\n` + one `<src> <dst> <op>` line per row, sorted by `(src, dst, op)`.
pub(crate) fn cmd_flows(store: &Store, scope: Scope, rest: &str) -> String {
    if !scope.is_owner_class() {
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

/// Resolve the `raise <sid>` argument against the registry: the headless-testable half of [`cmd_raise`] (`pub(super)` for that test).
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

/// `raise <sid>` -> raise the window hosting that session and select its tab — the wire twin of the status item's Go / map Enter (`@<sid> tab` drives that window's tabs without raising it; `focus` is a PTY focus event).
pub(crate) fn cmd_raise(
    proxy: &EventLoopProxy<Wake>,
    store: &Store,
    scope: Scope,
    rest: &str,
) -> String {
    if !scope.is_owner_class() {
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

/// `exits [<n>] [since=<id>]` -> the instance's exit ledger, oldest-first, framed `OK <count>\n` + one `exit <id> t=<ms> sid=<sid> local=<n> reason=<…> exit_code=<n|-> by=<sid|human|ctl|bridge|->` line per session that left the registry — the `history`/`timeline` grammar (`<n>` keeps the last n; `since=<id>` keeps rows with id strictly greater) over the closed-session index, whose slots are linked in the write that records the roster's `Exited` row, in its seq order (the row id); bounded by the index's last 512 closes.
pub(crate) fn cmd_exits(store: &Store, rest: &str) -> String {
    let page = match RingPage::parse(rest, "exits") {
        Ok(page) => page,
        Err(usage) => return usage,
    };
    let rows = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        page.copy(g.closed_logs().exits_since(page.since.unwrap_or(0)))
    };
    exits_reply(&rows)
}

fn exits_reply(rows: &[crate::session_log::ClosedExit]) -> String {
    use std::fmt::Write as _;
    #[cfg(test)]
    crate::session_log::encode_probe();
    let mut out = format!("OK {}\n", rows.len());
    for r in rows {
        #[cfg(test)]
        crate::work_counts::ring_row_formatted();
        let exit_code = r
            .exit_code
            .map_or_else(|| "-".to_string(), |c| c.to_string());
        let _ = writeln!(
            out,
            "exit {} t={} sid={} local={} reason={} exit_code={exit_code} by={}",
            r.seq,
            r.t_ms,
            r.sid.as_str(),
            r.local_id,
            r.reason.as_str(),
            pct_encode(r.actor.as_wire()),
        );
    }
    out
}

/// The exit ledger's `by=` for a control-socket `close`: the caller's own sid when the connection carries one.
pub(crate) fn caller_actor(scope: Scope, target: &SessionCtx) -> ExitActor {
    caller_actor_of(scope, Some(target))
}

/// [`caller_actor`] for a request that may have no resolved session (the App lane's `tab` and `invoke`).
pub(crate) fn caller_actor_of(scope: Scope, target: Option<&SessionCtx>) -> ExitActor {
    match scope {
        Scope::Owner => ExitActor::Ctl,
        #[cfg(any(unix, test))]
        Scope::Bridge => ExitActor::Bridge,
        Scope::Edge(presented) => target
            .and_then(|target| {
                target
                    .edges
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .src_of(&presented)
                    .map(|src| ExitActor::Sid(src.as_str().to_string()))
            })
            .unwrap_or(ExitActor::Unknown),
        #[cfg(any(unix, test))]
        Scope::Fleet { .. } => ExitActor::Unknown,
    }
}

/// The turn ledger's `by=` for a `turn` from this connection ([`TurnIo::by`]), from the scope and the driver the dispatch already resolved from it (an edge's source in the target's table, `crate::control::turn_driver`): the caller the exits ledger would name ([`caller_actor`]), so the two ledgers attribute a connection in the same words — `ctl` for an owner-token client, `bridge` for the fabric bridge, an edge-scoped caller's own sid, `-` for an edge whose grant is not in the target's table.
pub(crate) fn turn_by(scope: Scope, driver: Option<&SessionId>) -> TurnBy {
    match scope {
        Scope::Owner => TurnBy::Ctl,
        #[cfg(any(unix, test))]
        Scope::Bridge => TurnBy::Bridge,
        Scope::Edge(_) => {
            driver.map_or(TurnBy::Unknown, |sid| TurnBy::Sid(sid.as_str().to_string()))
        }
        #[cfg(any(unix, test))]
        Scope::Fleet { .. } => TurnBy::Unknown,
    }
}

/// Who typed a turn whose caller is `caller` ([`TurnIo::by`]) and that types under `under`, the drive lease its own connection holds as it begins (the one it hands back as it ends): an owner-token turn under THIS process's window harness's hand ([`crate::harness_host::holder`], the lease the harness's upgrade and relaunch lanes take a tab by for every line they type — the live upgrade's notice and release, a relaunched agent's continuation) is the harness's.
pub(crate) fn typed_by(caller: &TurnBy, under: Option<&crate::Lease>) -> TurnBy {
    let harness_hand = matches!(under, Some(crate::Lease::Drive { holder, .. })
        if *holder == crate::harness_host::holder());
    match caller {
        TurnBy::Ctl if harness_hand => TurnBy::Harness,
        other => other.clone(),
    }
}

/// Record a text the window harness types with `send` and submits with `key enter` into the ledger ([`crate::turn_ledger::STATUS_PRESSED`]): wherever the host fences `send`, the turn-end loop's continuation, done check and answers go out so (`aterm-agent`'s `Session::type_fenced`), never as a `turn`, and they are the harness's turns all the same.
#[allow(
    clippy::too_many_arguments,
    reason = "the dispatch's resolved request and its reply, as every arm takes them"
)]
pub(crate) fn ledger_typed_input(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    scope: Scope,
    verb: &str,
    rest: &str,
    reply: &str,
) {
    let wrote = reply.starts_with("OK") && !reply.starts_with("OK skipped");
    let input = matches!(
        verb,
        "send" | "key" | "ctrl" | "feed" | "mouse" | "paste" | "turn"
    );
    if !wrote || !input {
        return;
    }
    let harness = super::serving_connection().filter(|&conn| {
        let lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
        let own = lease
            .as_ref()
            .filter(|l| l.held_over(crate::metrics::now_us(), Some(conn)));
        typed_by(&turn_by(scope, None), own) == TurnBy::Harness
    });
    let turns = || ctx.turns.lock().unwrap_or_else(|p| p.into_inner());
    match harness {
        Some(conn) if verb == "send" => {
            let arch = crate::turn_ledger::ArchMark::of(&term_lock(term));
            turns().typed(crate::turn_ledger::Typed {
                conn,
                text: crate::turn_ledger::clamp_text(rest),
                started_ms: crate::turn_ledger::now_ms(),
                arch,
            });
        }
        Some(conn) if verb == "key" && rest.trim() == "enter" => {
            let Some(typed) = turns().take_typed().filter(|typed| typed.conn == conn) else {
                return;
            };
            let mut look = ctx.frame.look_frame();
            let (seq, screen_hash) = {
                let booked = crate::park::Lane::SERVED.ignoring_hangup();
                let t = crate::term_lock_whole_when(term, booked, |_| true);
                (t.content_seq(), look.text(&t).fnv())
            };
            drop(look);
            let record = crate::turn_ledger::TurnRecord {
                id: mint_turn_id(),
                started_ms: typed.started_ms,
                dur_ms: crate::turn_ledger::now_ms().saturating_sub(typed.started_ms),
                submitted: true,
                status: crate::turn_ledger::STATUS_PRESSED,
                text: typed.text,
                screen_hash,
                seq,
                arch: typed.arch,
                by: TurnBy::Harness,
                carried: false,
            };
            turns().record(record, mint_turn_id);
        }
        _ => {
            turns().take_typed();
        }
    }
}

/// The session log's `by=` for a request on `scope` against `target` (`docs/DESIGN-session-log-2026-10-15.md` §2.5): who the connection is, never a parameter.
pub(crate) fn log_actor(scope: Scope, target: &SessionCtx) -> aterm_slog::Actor {
    use aterm_slog::Actor;
    match scope {
        Scope::Owner => match crate::control::serving_principal() {
            crate::control::Principal::Harness => Actor::Harness,
            crate::control::Principal::Instance => Actor::Ctl,
        },
        #[cfg(any(unix, test))]
        Scope::Bridge => Actor::Bridge,
        Scope::Edge(presented) => target
            .edges
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .src_of(&presented)
            .and_then(|src| crate::session_log::sid_actor(src.as_str()))
            .unwrap_or(Actor::Nobody),
        #[cfg(any(unix, test))]
        Scope::Fleet { .. } => Actor::Nobody,
    }
}

/// `whoami` -> this session's fabric id and nonce and the connection's effective scope against the session active now.
pub(crate) fn cmd_whoami(ctx: &SessionCtx, scope: Scope) -> String {
    let s = match scope {
        Scope::Owner => "owner".to_string(),
        #[cfg(any(unix, test))]
        Scope::Bridge => "bridge".to_string(),
        Scope::Edge(presented) => {
            let table = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
            match table.authorize(&presented, &ctx.self_id, &ctx.nonce) {
                Some(op) => format!("edge {}", op.as_str()),
                None => "edge unauthorized".to_string(),
            }
        }
        #[cfg(any(unix, test))]
        Scope::Fleet { content, .. } => if content {
            "fleet content"
        } else {
            "fleet facts"
        }
        .to_string(),
    };
    format!("OK {} {} {}\n", ctx.self_id.as_str(), ctx.nonce.to_hex(), s)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_handoff_turn_count_leaves_room_for_turns_in_the_overlap() {
        let _own = super::own_turn_ids();
        let carried = super::turn_ids_for_handoff();
        let overlap = super::mint_turn_id();
        assert!(
            overlap <= carried,
            "an id minted after the manifest ({overlap}) is one the successor, counting \
             on from {carried}, never mints"
        );
        assert!(!super::past_ceiling(carried, carried));
        assert!(super::past_ceiling(carried + 1, carried));
        assert!(!super::past_ceiling(u64::MAX, 0), "no manifest, no ceiling");
    }

    #[test]
    fn a_count_at_the_manifest_limit_never_wedges_the_commit() {
        let max = crate::handoff_carry::MAX_TURN_ID;
        let minted = max + 1;
        let ceiling = minted.saturating_add(TURN_ID_RESERVE).min(max);
        assert_eq!(ceiling, max, "the ceiling is held to the top");
        assert!(!super::past_ceiling(minted, ceiling));
        assert!(!super::past_ceiling(minted + TURN_ID_RESERVE, ceiling));
        let below = max - TURN_ID_RESERVE - 1;
        assert!(!super::past_ceiling(below, below + TURN_ID_RESERVE));
        assert!(
            super::past_ceiling(below + TURN_ID_RESERVE + 1, below + TURN_ID_RESERVE),
            "negative control: past a ceiling below the top still stands down"
        );
    }
    use super::*;
    use aterm_session::{EdgeTable, LaunchNonce};

    #[test]
    fn history_and_timeline_limits_match_retained_wire_rows() {
        use crate::turn_ledger::{ArchMark, LEDGER_CAP, TurnRecord};

        let h = crate::session_store::test_handle(73);
        assert_eq!(cmd_history(&h.ctx, "1 since=0"), "OK 0\n");
        assert_eq!(cmd_timeline(&h.ctx, "1 since=0"), "OK 0\n");

        for i in 1..=(LEDGER_CAP + 7) as u64 {
            h.ctx.turns.lock().unwrap().push(TurnRecord {
                id: i * 2,
                started_ms: i,
                dur_ms: 3,
                submitted: true,
                status: "settled",
                text: format!("turn {i} 🦀").into(),
                screen_hash: i,
                seq: i,
                arch: ArchMark { origin: 7, last: i },
                by: TurnBy::Ctl,
                carried: i.is_multiple_of(2),
            });
        }
        for i in 1..=(aterm_slog::MAX_RECORDS + 7) as u64 {
            h.ctx
                .timeline
                .lock()
                .unwrap()
                .record_hand_row("title-change", format!("title=tab%20{i}"));
        }

        type HistoryQuery = fn(&SessionCtx, &str) -> String;
        let verbs: [(HistoryQuery, Option<usize>, bool); 2] = [
            (cmd_history, Some(LEDGER_CAP), false),
            (cmd_timeline, None, true),
        ];
        for (verb, cap, above_reads_all) in verbs {
            let all = verb(&h.ctx, "");
            let rows: Vec<(u64, &str)> = all
                .lines()
                .skip(1)
                .map(|line| {
                    (
                        line.split_whitespace().nth(1).unwrap().parse().unwrap(),
                        line,
                    )
                })
                .collect();
            if cap.is_none() {
                let floor = aterm_slog::row_floor(crate::session_log::ring_limits()).records;
                assert!(rows.len() > floor, "the view holds more than its row floor");
            }
            let cap = cap.unwrap_or(rows.len());
            assert_eq!(rows.len(), cap, "fixture must reach ring eviction");
            let low = rows[0].0;
            let high = rows.last().unwrap().0;
            assert!(low > 1, "fixture must evict a prefix");

            for n in [0, 1, 2, cap - 1, cap, cap + 1, usize::MAX] {
                for since in [
                    None,
                    Some(0),
                    Some(low - 1),
                    Some(low),
                    Some(high - 1),
                    Some(high),
                    Some(high + 1),
                    Some(u64::MAX),
                ] {
                    let mut expected: Vec<&str> = rows
                        .iter()
                        .filter(|(id, _)| {
                            since.is_none_or(|after| {
                                (above_reads_all && after > high) || *id > after
                            })
                        })
                        .map(|(_, line)| *line)
                        .collect();
                    if n > 0 && expected.len() > n {
                        expected.drain(..expected.len() - n);
                    }
                    let shown = expected.len();
                    let mut want = format!("OK {shown}\n");
                    for line in expected {
                        want.push_str(line);
                        want.push('\n');
                    }
                    let args = match since {
                        Some(id) => format!("{n} since={id}"),
                        None => n.to_string(),
                    };
                    crate::work_counts::take_ring_rows_formatted();
                    assert_eq!(verb(&h.ctx, &args), want, "arguments: {args}");
                    assert_eq!(
                        crate::work_counts::take_ring_rows_formatted(),
                        shown,
                        "only the rows shown are formatted: {args}"
                    );
                }
            }
        }
    }

    #[test]
    fn ring_reads_format_after_their_lock_is_released() {
        use crate::session_log::ENCODE_PROBE;
        use crate::session_store::{ExitActor, ExitReason, new_store, test_handle};
        use crate::turn_ledger::{ArchMark, TurnRecord};

        let store = new_store();
        let h = test_handle(5);
        {
            let mut g = store.write().unwrap();
            g.register(h.clone());
            g.register(test_handle(6));
            g.deregister_local_as(6, ExitReason::UiClose, ExitActor::Human);
        }
        h.ctx.turns.lock().unwrap().push(TurnRecord {
            id: 1,
            started_ms: 1,
            dur_ms: 1,
            submitted: true,
            status: "settled",
            text: "make".into(),
            screen_hash: 1,
            seq: 1,
            arch: ArchMark::default(),
            by: crate::turn_ledger::TurnBy::Ctl,
            carried: false,
        });
        h.ctx
            .timeline
            .lock()
            .unwrap()
            .record_hand_row("title-change", "title=t".to_string());

        let held: Arc<Mutex<Vec<[bool; 3]>>> = Arc::new(Mutex::new(Vec::new()));
        let (ctx, probe_store, probe_held) =
            (Arc::clone(&h.ctx), Arc::clone(&store), Arc::clone(&held));
        ENCODE_PROBE.with(|p| {
            *p.borrow_mut() = Some(Box::new(move || {
                probe_held.lock().unwrap().push([
                    ctx.turns.try_lock().is_err(),
                    ctx.timeline.try_lock().is_err(),
                    probe_store.try_write().is_err(),
                ]);
            }));
        });
        let replies = [
            cmd_history(&h.ctx, ""),
            cmd_timeline(&h.ctx, ""),
            cmd_exits(&store, ""),
        ];
        let probes = std::mem::take(&mut *held.lock().unwrap());
        for reply in &replies {
            assert!(
                reply.starts_with("OK ") && !reply.starts_with("OK 0"),
                "{reply}"
            );
        }
        assert_eq!(
            probes,
            vec![[false; 3]; 3],
            "a ring was formatted under its lock"
        );

        let all = RingPage { n: 0, since: None };
        let under = [
            {
                let ledger = h.ctx.turns.lock().unwrap();
                history_reply(&all.copy(ledger.since(None)), None)
            },
            {
                let tl = h.ctx.timeline.lock().unwrap();
                timeline_reply(&tl.view_copy(all.since, all.n).rows())
            },
            {
                let g = store.read().unwrap();
                exits_reply(&all.copy(g.closed_logs().exits_since(0)))
            },
        ];
        let probes = std::mem::take(&mut *held.lock().unwrap());
        ENCODE_PROBE.with(|p| *p.borrow_mut() = None);
        assert_eq!(
            probes,
            vec![
                [true, false, false],
                [false, true, false],
                [false, false, true]
            ]
        );
        assert_eq!(under, replies);
    }

    #[test]
    fn momentum_wait_read_guards_conform_and_catch_expired_reparking() {
        use std::time::{Duration, Instant};

        let model = aterm_models_session::momentum_wait_read_model();
        let now = Instant::now();
        for (value, floor) in [(0.0, 0.0), (0.000_75, 0.0), (0.000_75, 0.000_7)] {
            for expired in [false, true] {
                let until = if expired {
                    now
                } else {
                    now + Duration::from_secs(1)
                };
                let mut state = model.init_state();
                let met = value <= floor;
                let observe = match (met, expired) {
                    (false, false) => "ReadLive",
                    (false, true) => "ReadExpired",
                    (true, false) => "ReadBelow",
                    (true, true) => "ReadBelowExpired",
                };
                assert!(model.fire(observe, &mut state));
                let chosen = match momentum_read_outcome(value, floor, now, until) {
                    Some(MomentumWait::Below(v)) => {
                        assert_eq!(v, value);
                        "Below"
                    }
                    Some(MomentumWait::Deadline(v)) => {
                        assert_eq!(v, value);
                        "Deadline"
                    }
                    None => "Park",
                    _ => panic!("a pure read cannot exit or fail"),
                };
                for action in ["Below", "Deadline", "Park"] {
                    assert_eq!(model.action_enabled(action, &state), chosen == action);
                }
                let mut post = state.clone();
                assert!(model.fire(chosen, &mut post));
                let (valid, why) = aterm_spec::verify::validate_transition_tiered(
                    &model,
                    &[],
                    &state,
                    &post,
                    Some(chosen),
                    "real momentum read guard",
                );
                assert!(valid, "{why}");
                if expired && !met {
                    let buggy = aterm_spec::interp::with_buggy(&model, 1);
                    assert!(buggy.fire("Park", &mut state));
                    assert!(!model.check_invariant("ExpiredReadingCannotRepark", &state));
                }
            }
        }
    }

    #[test]
    fn momentum_wait_zero_and_low_floors_park_once_until_the_real_reading_matches() {
        use std::cell::Cell;
        use std::time::{Duration, Instant};

        use aterm_effects::typing_momentum::TypingMomentum;

        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let sub = crate::session_watch::Subscription::new();
        for (value, floor) in [(0.000_51, 0.0), (0.000_51, 0.000_1), (0.000_75, 0.000_7)] {
            let at = Instant::now();
            let metric = Cell::new(TypingMomentum::default());
            let reads = Cell::new(0);
            let read = || {
                reads.set(reads.get() + 1);
                if reads.get() > 4 {
                    return Err("immediate-latch read loop".to_string());
                }
                let now = Instant::now();
                if reads.get() == 1 {
                    let mut warm = TypingMomentum::default();
                    warm.set_value(now, value);
                    metric.set(warm);
                }
                Ok((metric.get().value(now), now))
            };
            match park_until_momentum_below(
                &term,
                &|| false,
                &sub,
                &read,
                floor,
                at + Duration::from_secs(2),
            ) {
                MomentumWait::Below(v) => assert!(v <= floor),
                MomentumWait::Unreadable(why) => panic!("{why}"),
                _ => panic!("the real metric must reach {floor}"),
            }
            assert_eq!(reads.get(), 2, "one solving read and one confirmation");
            assert!(term_lock(&term).watch_next_deadline().is_none());
        }
    }

    #[test]
    fn momentum_wait_low_floor_honors_timeout_and_expired_read_backstop() {
        use std::cell::Cell;
        use std::time::{Duration, Instant};

        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let sub = crate::session_watch::Subscription::new();
        for floor in [0.0, 0.000_1, 0.000_7] {
            let reads = Cell::new(0);
            let read = || {
                reads.set(reads.get() + 1);
                if reads.get() > 4 {
                    return Err("immediate-latch read loop".to_string());
                }
                Ok((0.000_75, Instant::now()))
            };
            let until = Instant::now() + Duration::from_millis(20);
            assert!(matches!(
                park_until_momentum_below(&term, &|| false, &sub, &read, floor, until),
                MomentumWait::Deadline(v) if v > floor
            ));
            assert!(
                (1..=2).contains(&reads.get()),
                "at most the solving read and deadline read, without polling"
            );
            assert!(term_lock(&term).watch_next_deadline().is_none());
        }

        let now = Instant::now();
        let reads = Cell::new(0);
        let stale = || {
            reads.set(reads.get() + 1);
            if reads.get() > 4 {
                return Err("expired reading re-parked".to_string());
            }
            Ok((0.000_75, now - Duration::from_secs(10)))
        };
        assert!(matches!(
            park_until_momentum_below(&term, &|| false, &sub, &stale, 0.0, now),
            MomentumWait::Deadline(v) if v > 0.0
        ));
        assert_eq!(reads.get(), 1);
        assert!(term_lock(&term).watch_next_deadline().is_none());
        assert!(matches!(
            park_until_momentum_below(&term, &|| true, &sub, &stale, 0.0, now),
            MomentumWait::Exited
        ));
    }

    fn handle(local_id: u64, term: &Arc<Mutex<Terminal>>) -> SessionHandle {
        let sid = SessionId::generate();
        let nonce = LaunchNonce::generate();
        let observed = crate::session_watch::Observed::default();
        let ctx = Arc::new(crate::SessionCtx {
            sink: Arc::new(aterm_session::sink::SinkWriter::new(-1)),
            output_echo: Arc::new(crate::app_input::OutputEchoTracker::default()),
            modes: crate::mode_mirror_of(term),
            ui_waiting: Arc::default(),
            edges: std::sync::Arc::new(std::sync::Mutex::new(EdgeTable::new())),
            turn_lease: std::sync::Mutex::new(None),
            self_id: sid.clone(),
            nonce,
            cast: Arc::new(std::sync::Mutex::new(crate::cast::CastRecorder::new(
                80, 24,
            ))),
            temporal: Arc::new(std::sync::Mutex::new(
                crate::temporal::TemporalRecorder::new(),
            )),
            byte_fanout: Arc::new(crate::cast::ByteFanout::new()),
            turns: observed.turns,
            meta: std::sync::Mutex::new(crate::session_timeline::SessionMeta::default()),
            timeline: observed.timeline,
            fabric: std::sync::Arc::default(),
            idem: Default::default(),
            rewrap_gauge: std::sync::Arc::default(),
            human_input: Default::default(),
            licence: Default::default(),
            launch: Default::default(),
            reset_lane: Default::default(),
            crash_journal: Default::default(),
            typed: Default::default(),
            bells: Default::default(),
            screen_moves: Default::default(),
            job_feed: Default::default(),
            box_banner: Default::default(),
            update_parked: Default::default(),
            watchers: observed.watchers,
            frame: Default::default(),
            witness: Default::default(),
            fenced_writes: Default::default(),
        });
        SessionHandle {
            sid,
            nonce,
            local_id,
            parent: None,
            state: crate::session_store::SessionState::Alive,
            title: format!("tab-{local_id}"),
            term: term.clone(),
            master: -1,
            shell_identity: aterm_pty::ShellIdentity::unproven(0),
            ctx,
        }
    }

    fn row(local: u64, window: u64, active_tab: bool, window_focused: bool) -> SessionWindowRow {
        SessionWindowRow {
            local,
            window,
            active_tab,
            window_focused,
        }
    }

    const AGENT_COLUMNS: [&str; 23] = [
        "program=",
        "agent=",
        "agent_detail=",
        "agent_rev=",
        "agent_since_ms=",
        "agent_gen=",
        "agent_fp=",
        "program_state=",
        "human_ms=",
        "role=",
        "attention=",
        "user_title=",
        "supervisor=",
        "supervisor_proven=",
        "autopilot=",
        "path_evidence=",
        "copy=",
        "upgrade=",
        "land_at=",
        "task=",
        "pause=",
        "task_state=",
        "task_rev=",
    ];

    fn tail(line: &str) -> String {
        let mut f = line.splitn(3, ' ');
        let local = f.next().unwrap();
        let _sid = f.next().unwrap();
        let rest = f.next().unwrap();
        let rest = rest
            .split_whitespace()
            .filter(|t| !t.starts_with("nonce="))
            .filter(|t| !AGENT_COLUMNS.iter().any(|k| t.starts_with(k)))
            .collect::<Vec<_>>()
            .join(" ");
        format!("{local} {rest}")
    }

    #[test]
    fn await_agent_words_take_walls_and_refuse_typos() {
        let words = parse_agent_words("prompt,wall,wall:usage-session,unknown").expect("valid");
        assert_eq!(
            words,
            vec![
                AgentWord::Verdict("prompt"),
                AgentWord::Wall(None),
                AgentWord::Wall(Some("usage-session".to_string())),
                AgentWord::Verdict("unknown"),
            ]
        );
        let overloaded = aterm_phase::WallKind::Overloaded.wire();
        let session = aterm_phase::WallKind::UsageSession.wire();
        assert!(words[1].matches(overloaded));
        assert!(words[2].matches(session));
        assert!(!words[2].matches(overloaded));
        assert!(!words[1].matches("prompt") && !words[1].matches("wallpaper"));
        for bad in [
            "",
            "promt",
            "limited",
            "limitd",
            "wall:",
            "wall:Rate",
            "wall:a b",
            "prompt,,idle",
        ] {
            assert_eq!(parse_agent_words(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_roster_carries_the_typed_role_attention_and_title() {
        let t0 = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let h = handle(0, &t0);
        {
            let mut m = h.ctx.meta.lock().unwrap();
            m.set("role", Some("reviewer".into()));
            m.set("attention", Some("stuck on CI".into()));
            m.set("title", Some("my build".into()));
        }
        let out = sessions_lines(
            std::slice::from_ref(&h),
            Ok(&[]),
            crate::autopilot::HarnessPosture::default(),
            |_| false,
        );
        let line = out.lines().nth(1).unwrap();
        assert!(
            line.contains(
                " role=reviewer attention=stuck%20on%20CI user_title=my%20build supervisor=- \
                 supervisor_proven=0 "
            ),
            "{line}"
        );
        assert!(line.contains(" meta=1 "), "{line}");
    }

    #[test]
    fn the_roster_carries_each_sessions_task_record() {
        use crate::task_record::{TaskMark, TaskState, mark_session_task};
        let t0 = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let (worker, shell) = (handle(0, &t0), handle(1, &t0));
        mark_session_task(&worker.ctx, 0, &TaskMark::new(TaskState::Working)).expect("unfenced");
        mark_session_task(&worker.ctx, 0, &TaskMark::new(TaskState::Done)).expect("unfenced");
        let out = sessions_lines(
            &[worker, shell],
            Ok(&[]),
            crate::autopilot::HarnessPosture::default(),
            |_| false,
        );
        let rows: Vec<&str> = out.lines().skip(1).collect();
        assert!(
            rows[0].ends_with(" task=- pause=- task_state=done task_rev=2"),
            "{out}"
        );
        assert!(
            rows[1].ends_with(" task=- pause=- task_state=- task_rev=0"),
            "{out}"
        );
    }

    fn three_workers(store: &Store) -> Vec<SessionHandle> {
        use crate::task_record::{TaskMark, TaskState, mark_session_task};
        let workers: Vec<SessionHandle> = (1..=3).map(crate::session_store::test_handle).collect();
        for worker in &workers {
            store.write().unwrap().register(worker.clone());
            mark_session_task(
                &worker.ctx,
                worker.local_id,
                &TaskMark::new(TaskState::Working),
            )
            .expect("unfenced");
        }
        workers
    }

    fn task_reply(reply: &str) -> (&str, Vec<&str>) {
        let mut lines = reply.lines();
        let header = lines.next().expect("a header");
        let states = lines
            .map(|row| {
                row.split_whitespace()
                    .find_map(|t| t.strip_prefix("state="))
                    .expect("state=")
            })
            .collect();
        (header, states)
    }

    /// A row names qualifying request news, independently of the current display
    /// string. Already-terminal/noted baselines stay quiet; captured edges hold.
    #[test]
    fn await_task_rows_name_recorded_news_without_guessing_display_attention() {
        use crate::session_timeline::{MetaEdit, write_attention_owned};
        use crate::task_record::{
            Combine, TaskMark, TaskState, mark_session_task, mark_session_task_fact,
        };
        use aterm_agent::supervise::occupancy::{FactWord, NoteOutcome, RuleId};
        let store = crate::session_store::new_store();
        let workers = three_workers(&store);
        let bit = |line: &str| {
            line.split_whitespace()
                .find_map(|word| word.strip_prefix("task_news="))
                .map(str::to_string)
        };
        let w = &workers[0];
        write_attention_owned(
            &w.ctx,
            "unrelated-owner",
            MetaEdit::Set("an unrelated display message"),
            aterm_slog::Actor::Harness,
        )
        .expect("the display entry");
        mark_session_task(
            &workers[2].ctx,
            workers[2].local_id,
            &TaskMark::new(TaskState::Done),
        )
        .expect("terminal baseline");
        let f = &workers[1];
        let note =
            |word| mark_session_task_fact(&f.ctx, f.local_id, word, std::time::Duration::ZERO);
        assert_eq!(note(FactWord::new(RuleId::HoldRoom, 1)), NoteOutcome::Noted);
        let targets: Vec<_> = workers
            .iter()
            .map(|w| TaskTarget {
                local_id: w.local_id,
                ctx: &w.ctx,
                since: None,
                facts: true,
                fact_since: None,
            })
            .collect();
        let quiet = await_task(&store, &targets, Combine::All, 0);
        assert!(quiet.contains("attention=an%20unrelated"));
        assert!(
            quiet
                .lines()
                .skip(1)
                .all(|row| bit(row).as_deref() == Some("0")),
            "{quiet}"
        );
        let edges: Vec<_> = targets
            .iter()
            .map(|t| {
                let record = task_record_of(t.ctx).expect("a task");
                (record.rev, Some(record.fact_rev))
            })
            .collect();
        let exits: Vec<_> = targets
            .iter()
            .map(|t| exit_probe(&store, t.local_id))
            .collect();
        let mut latched = vec![None; targets.len()];
        write_attention_owned(
            &w.ctx,
            crate::harness_host::ATTENTION_OWNER,
            MetaEdit::Set("a login"),
            aterm_slog::Actor::Harness,
        )
        .expect("the task entry");
        assert_eq!(
            note(FactWord::new(RuleId::WaitNudge, 1)),
            NoteOutcome::Noted
        );
        latch_task_news(&store, &targets, &edges, &exits, &mut latched);
        assert!(latched[..2].iter().all(|row| {
            row.as_deref()
                .is_some_and(|row| bit(row).as_deref() == Some("1"))
        }));
        assert!(latched[2].is_none(), "the terminal baseline is no new edge");
        write_attention_owned(
            &w.ctx,
            crate::harness_host::ATTENTION_OWNER,
            MetaEdit::Clear,
            aterm_slog::Actor::Harness,
        )
        .expect("the task entry clears");
        assert_eq!(note(None), NoteOutcome::Cleared);
        latch_task_news(&store, &targets, &edges, &exits, &mut latched);
        assert!(latched[..2].iter().all(|row| {
            row.as_deref()
                .is_some_and(|row| bit(row).as_deref() == Some("1"))
        }));
        let quiet = await_task(&store, &targets, Combine::All, 0);
        assert!(
            quiet
                .lines()
                .skip(1)
                .all(|row| bit(row).as_deref() == Some("0")),
            "{quiet}"
        );
    }

    #[test]
    fn await_task_answers_news_past_each_targets_rev_once() {
        use crate::task_record::{Combine, TaskMark, TaskState, mark_session_task};
        let store = crate::session_store::new_store();
        let workers = three_workers(&store);
        let wait = |since: [Option<u64>; 3], combine: Combine, timeout_ms: u64| {
            let targets: Vec<TaskTarget<'_>> = workers
                .iter()
                .zip(since)
                .map(|(w, since)| TaskTarget {
                    local_id: w.local_id,
                    ctx: &w.ctx,
                    since,
                    facts: false,
                    fact_since: None,
                })
                .collect();
            await_task(&store, &targets, combine, timeout_ms)
        };
        let quiet = wait([None; 3], Combine::Any, 20);
        assert_eq!(
            task_reply(&quiet),
            ("OK 3 status=timeout", vec!["working"; 3]),
            "{quiet}"
        );
        assert!(
            quiet.contains(&format!(
                "@{} rev=1 state=working agent=- program_state=- attention=- result_bytes=0 \
                 task_news=0\n",
                workers[0].sid.as_str()
            )),
            "{quiet}"
        );

        let mut done = TaskMark::new(TaskState::Done);
        done.result_bytes = 64;
        mark_session_task(&workers[1].ctx, 2, &done).expect("unfenced");
        let any = wait([Some(1); 3], Combine::Any, 60_000);
        assert_eq!(
            task_reply(&any),
            ("OK 3", vec!["working", "done", "working"])
        );
        assert!(
            any.contains(
                " rev=2 state=done agent=- program_state=- attention=- result_bytes=64 \
                 task_news=1\n"
            ),
            "{any}"
        );
        for (since, combine) in [
            ([Some(1); 3], Combine::All),
            ([Some(1), Some(2), Some(1)], Combine::Any),
            ([None; 3], Combine::Any),
        ] {
            let again = wait(since, combine, 20);
            assert_eq!(
                task_reply(&again).0,
                "OK 3 status=timeout",
                "{since:?}: {again}"
            );
        }
        assert_eq!(
            task_reply(&wait([Some(0); 3], Combine::Any, 60_000)).0,
            "OK 3"
        );

        let mut stalled = TaskMark::new(TaskState::Stalled);
        stalled.why = Some("startup");
        mark_session_task(&workers[0].ctx, 1, &stalled).expect("unfenced");
        let mut exited = TaskMark::new(TaskState::Exited);
        exited.exit = Some(1);
        mark_session_task(&workers[2].ctx, 3, &exited).expect("unfenced");
        let all = wait([Some(1); 3], Combine::All, 60_000);
        assert_eq!(
            task_reply(&all),
            ("OK 3", vec!["stalled", "done", "exited"])
        );
    }

    /// A CLAIM IS NO TASK NEWS: a codex's own report (`program_state=`) rides an `await task`
    /// row but never wakes one. A wait parked past a working worker's rev sees the program
    /// report `done`, then `blocked:permission`, and still answers only at its deadline, the
    /// claim in its row and `task_news=0`, and the `task` reply carries it too; the harness's own
    /// `task mark done` then is news.
    #[test]
    fn a_programs_own_report_rides_the_await_task_row_and_never_wakes_it() {
        use crate::task_record::{Combine, TaskMark, TaskState, mark_session_task};
        use aterm_core::terminal::program_state::{
            ProgramBlockedKind, ProgramState, ProgramStateWord,
        };
        let store = crate::session_store::new_store();
        let workers = three_workers(&store);
        let w = &workers[0];
        let one = |timeout_ms: u64| {
            await_task(
                &store,
                &[TaskTarget {
                    local_id: w.local_id,
                    ctx: &w.ctx,
                    since: Some(1),
                    facts: false,
                    fact_since: None,
                }],
                Combine::Any,
                timeout_ms,
            )
        };
        let report = |state, kind| {
            w.ctx.timeline.lock().unwrap().note_program_state(
                Some(ProgramState {
                    state,
                    kind,
                    app: aterm_core::terminal::program_state::ProgramApp::Codex,
                }),
                1,
            );
        };
        let sid = w.sid.as_str();
        let parked = std::thread::scope(|s| {
            let waiter = s.spawn(|| one(2_000));
            let armed = std::time::Instant::now();
            while w.ctx.watchers.count() == 0 {
                assert!(
                    armed.elapsed() < std::time::Duration::from_secs(60),
                    "the waiter never parked"
                );
                std::thread::yield_now();
            }
            report(ProgramStateWord::Done, None);
            report(
                ProgramStateWord::Blocked,
                Some(ProgramBlockedKind::Permission),
            );
            waiter.join().expect("the waiter")
        });
        assert_eq!(
            parked,
            format!(
                "OK 1 status=timeout\n@{sid} rev=1 state=working agent=- \
                 program_state=blocked:permission attention=- result_bytes=0 task_news=0\n"
            ),
        );
        let task = crate::task_record::cmd_task(&w.ctx, w.local_id, Scope::Owner, "");
        assert!(
            task.starts_with("OK task rev=1 state=working ")
                && task.ends_with(" agent=- program_state=blocked:permission attention=-\n"),
            "the task reply carries the claim beside agent=: {task}"
        );
        mark_session_task(&w.ctx, w.local_id, &TaskMark::new(TaskState::Done)).expect("unfenced");
        assert_eq!(
            one(60_000),
            format!(
                "OK 1\n@{sid} rev=2 state=done agent=- program_state=blocked:permission \
                 attention=- result_bytes=0 task_news=1\n"
            ),
            "the record's own news"
        );
    }

    /// `supervisor_proven=` on `ls` is `status supervisor_proven=`: `1` only while the live claim
    /// is a proven self-claim, `0` for an unproven one and for a tab nobody claims.
    #[test]
    fn the_roster_says_whether_the_live_claim_is_proven() {
        let t0 = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let (proven, unproven, plain) = (handle(0, &t0), handle(1, &t0), handle(2, &t0));
        for (h, proof) in [(&proven, true), (&unproven, false)] {
            h.ctx.meta.lock().unwrap().supervisor =
                Some(crate::session_timeline::SupervisorClaim {
                    holder: "codex-alab@4242".to_string(),
                    conn: Some(1),
                    expires_us: None,
                    proven: proof.then(|| crate::self_claim::Proven {
                        pid: 4242,
                        start: "Mon Oct 20 16:37:58 2026".to_string(),
                    }),
                    scoped: None,
                });
        }
        let out = sessions_lines(
            &[proven, unproven, plain],
            Ok(&[]),
            crate::autopilot::HarnessPosture::default(),
            |_| false,
        );
        let rows: Vec<&str> = out.lines().skip(1).collect();
        for (row, want) in rows.iter().zip([
            " supervisor=codex-alab@4242 supervisor_proven=1 autopilot=",
            " supervisor=codex-alab@4242 supervisor_proven=0 autopilot=",
            " supervisor=- supervisor_proven=0 autopilot=",
        ]) {
            assert!(row.contains(want), "{want}: {row}");
        }
    }

    /// THE ROSTER'S FIXED COLUMN ORDER: a `sessions` row is five positional fields (`local sid
    /// parent state title`) and then exactly these keys, in this order, as the SPECS row
    /// roster-line-denotation names them.
    #[test]
    fn a_roster_row_keeps_its_fixed_column_order() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let h = handle(0, &term);
        let out = sessions_lines(
            std::slice::from_ref(&h),
            Ok(&[]),
            crate::autopilot::HarnessPosture::default(),
            |_| false,
        );
        let row = out.lines().nth(1).expect("one row");
        let mut tokens = row.split(' ');
        let positional: Vec<&str> = tokens.by_ref().take(5).collect();
        assert_eq!(
            positional,
            ["0", h.sid.as_str(), "-", h.state.as_str(), "tab-0"],
            "{row}"
        );
        let keys: Vec<&str> = tokens
            .map(|t| t.split_once('=').map_or(t, |(k, _)| k))
            .collect();
        assert_eq!(
            keys,
            [
                "meta",
                "nonce",
                "window",
                "active",
                "wfocus",
                "detail",
                "path",
                "program",
                "agent",
                "agent_detail",
                "agent_rev",
                "agent_since_ms",
                "agent_gen",
                "agent_fp",
                "program_state",
                "human_ms",
                "role",
                "attention",
                "user_title",
                "supervisor",
                "supervisor_proven",
                "autopilot",
                "path_evidence",
                "copy",
                "upgrade",
                "land_at",
                "task",
                "pause",
                "task_state",
                "task_rev",
            ],
            "{row}"
        );
    }

    #[test]
    fn await_task_wakes_on_a_mark_attention_and_a_close() {
        use crate::session_store::SessionState;
        use crate::session_timeline::{MetaEdit, write_attention_owned};
        use crate::task_record::{Combine, TaskMark, TaskState, mark_session_task};
        let store = crate::session_store::new_store();
        let workers = three_workers(&store);
        let one = |worker: &SessionHandle, since: u64| {
            await_task(
                &store,
                &[TaskTarget {
                    local_id: worker.local_id,
                    ctx: &worker.ctx,
                    since: Some(since),
                    facts: false,
                    fact_since: None,
                }],
                Combine::Any,
                60_000,
            )
        };
        std::thread::scope(|s| {
            let waiter = s.spawn(|| one(&workers[0], 1));
            let parked = std::time::Instant::now();
            while workers[0].ctx.watchers.count() == 0 {
                assert!(
                    parked.elapsed() < std::time::Duration::from_secs(60),
                    "the waiter never parked"
                );
                std::thread::yield_now();
            }
            mark_session_task(&workers[0].ctx, 1, &TaskMark::new(TaskState::Done))
                .expect("unfenced");
            let reply = waiter.join().expect("the waiter");
            assert_eq!(task_reply(&reply), ("OK 1", vec!["done"]), "{reply}");
        });
        write_attention_owned(
            &workers[1].ctx,
            crate::harness_host::ATTENTION_OWNER,
            MetaEdit::Set("a login"),
            aterm_slog::Actor::Harness,
        )
        .unwrap();
        let reply = one(&workers[1], 1);
        assert!(
            reply.ends_with(
                " rev=2 state=working agent=- program_state=- attention=a%20login \
                 result_bytes=0 task_news=1\n"
            ),
            "{reply}"
        );
        store
            .write()
            .unwrap()
            .set_state(workers[2].local_id, SessionState::Exited);
        assert_eq!(task_reply(&one(&workers[2], 1)), ("OK 1", vec!["closed"]));
    }

    /// THE FACT WORD IS NO NEWS TO A DEFAULT WAIT (the occupancy design's manager door): a
    /// worker whose supervisor attention stands, waited on past its rev, is not woken by a fact
    /// edit, which never moves `rev`. NEGATIVE CONTROL: `NoteBumpsNews` (the edit moves `rev`
    /// too) wakes it at once.
    #[test]
    fn a_fact_edit_never_wakes_a_default_await_task() {
        use crate::occupancy_host::fault::{Fault, with};
        use crate::session_timeline::{MetaEdit, write_attention_owned};
        use crate::task_record::{Combine, mark_session_task_fact};
        use aterm_agent::supervise::occupancy::{FactWord, NoteOutcome, RuleId};
        let store = crate::session_store::new_store();
        let workers = three_workers(&store);
        let w = &workers[0];
        write_attention_owned(
            &w.ctx,
            crate::harness_host::ATTENTION_OWNER,
            MetaEdit::Set("a login"),
            aterm_slog::Actor::Harness,
        )
        .unwrap();
        let rev = task_record_of(&w.ctx).expect("a record").rev;
        let wait = |timeout_ms| {
            await_task(
                &store,
                &[TaskTarget {
                    local_id: w.local_id,
                    ctx: &w.ctx,
                    since: Some(rev),
                    facts: false,
                    fact_since: None,
                }],
                Combine::Any,
                timeout_ms,
            )
        };
        assert_eq!(task_reply(&wait(20)).0, "OK 1 status=timeout");
        let word = |n| FactWord::new(RuleId::HoldRoom, n);
        let fence = std::time::Duration::ZERO;
        assert_eq!(
            mark_session_task_fact(&w.ctx, w.local_id, word(1), fence),
            NoteOutcome::Noted
        );
        let quiet = wait(20);
        assert_eq!(task_reply(&quiet).0, "OK 1 status=timeout", "{quiet}");
        assert!(
            quiet.contains(" fact=hold-room@v1#1 fact_rev=1 task_news=0\n"),
            "the row shows the word: {quiet}"
        );
        assert_eq!(task_record_of(&w.ctx).map(|t| t.rev), Some(rev));
        with(Fault::NoteBumpsNews, || {
            mark_session_task_fact(&w.ctx, w.local_id, word(2), fence)
        });
        let woken = wait(60_000);
        assert_eq!(
            task_reply(&woken).0,
            "OK 1",
            "CONTROL: a fact edit that moves the rev wakes it: {woken}"
        );
    }

    /// `facts=1`: a waiter parked on a worker's fact word is woken once by its note, a wait
    /// past the noted `fact_rev` is quiet, and a clear wakes none. The parked waiter names its
    /// fact edge (`fact_since=0`): a default edge is read after the waiter subscribes, so a note
    /// landing between the two would be the edge itself, and the answer would hang on the race.
    #[test]
    fn facts_1_is_woken_once_by_a_note_and_never_by_a_clear() {
        use crate::task_record::{Combine, mark_session_task_fact};
        use aterm_agent::supervise::occupancy::{FactWord, NoteOutcome, RuleId};
        let store = crate::session_store::new_store();
        let workers = three_workers(&store);
        let w = &workers[0];
        let wait = |fact_since: Option<u64>, timeout_ms| {
            await_task(
                &store,
                &[TaskTarget {
                    local_id: w.local_id,
                    ctx: &w.ctx,
                    since: None,
                    facts: true,
                    fact_since,
                }],
                Combine::Any,
                timeout_ms,
            )
        };
        let note = |word: Option<FactWord>| {
            mark_session_task_fact(&w.ctx, w.local_id, word, std::time::Duration::ZERO)
        };
        assert_eq!(task_reply(&wait(None, 20)).0, "OK 1 status=timeout");
        std::thread::scope(|s| {
            let waiter = s.spawn(|| wait(Some(0), 60_000));
            let parked = std::time::Instant::now();
            while w.ctx.watchers.count() == 0 {
                assert!(
                    parked.elapsed() < std::time::Duration::from_secs(60),
                    "the waiter never parked"
                );
                std::thread::yield_now();
            }
            assert_eq!(note(FactWord::new(RuleId::HoldRoom, 1)), NoteOutcome::Noted);
            let reply = waiter.join().expect("the waiter");
            assert_eq!(task_reply(&reply).0, "OK 1", "{reply}");
            assert!(
                reply.ends_with(
                    " rev=1 state=working agent=- program_state=- attention=- result_bytes=0 \
                     fact=hold-room@v1#1 fact_rev=1 task_news=1\n"
                ),
                "{reply}"
            );
        });
        assert_eq!(
            task_reply(&wait(Some(1), 20)).0,
            "OK 1 status=timeout",
            "the note was read"
        );
        assert_eq!(
            task_reply(&wait(None, 20)).0,
            "OK 1 status=timeout",
            "armed past the note"
        );
        assert_eq!(note(FactWord::new(RuleId::HoldRoom, 1)), NoteOutcome::Noted);
        assert_eq!(
            task_reply(&wait(Some(1), 20)).0,
            "OK 1 status=timeout",
            "the same word again moves nothing"
        );
        assert_eq!(note(None), NoteOutcome::Cleared);
        let cleared = wait(Some(1), 20);
        assert_eq!(task_reply(&cleared).0, "OK 1 status=timeout", "{cleared}");
        assert!(
            cleared.ends_with(" fact=- fact_rev=2 task_news=0\n"),
            "{cleared}"
        );
        assert_eq!(
            task_reply(&wait(Some(0), 20)).0,
            "OK 1 status=timeout",
            "past an old note, the cleared word is no news"
        );
        assert_eq!(
            note(FactWord::new(RuleId::WaitNudge, 1)),
            NoteOutcome::Noted
        );
        assert_eq!(
            task_reply(&wait(Some(2), 60_000)).0,
            "OK 1",
            "CONTROL: the next note past the clear wakes it"
        );
        let verb = |rest: &str| cmd_await(&w.term, &store, w.local_id, &w.ctx, rest);
        assert!(
            verb("task facts=1 fact_since=2 timeout=60000")
                .ends_with(" fact=wait-nudge@v1#1 fact_rev=3 task_news=1\n"),
            "the single form reads facts=1"
        );
        assert_eq!(
            task_reply(&verb("task facts=1 timeout=20")).0,
            "OK 1 status=timeout"
        );
        assert_eq!(
            verb("task fact_since=2"),
            crate::task_record::AWAIT_TASK_USAGE,
            "a fact edge names facts=1"
        );
    }

    #[test]
    fn the_single_await_task_form_waits_on_its_session() {
        use crate::task_record::{AWAIT_TASK_USAGE, TaskMark, TaskState, mark_session_task};
        let store = crate::session_store::new_store();
        let workers = three_workers(&store);
        let w = &workers[0];
        mark_session_task(&w.ctx, w.local_id, &TaskMark::new(TaskState::Done)).expect("unfenced");
        let await_ = |rest: &str| cmd_await(&w.term, &store, w.local_id, &w.ctx, rest);
        assert!(
            await_("task since=1 timeout=60000").starts_with("OK 1\n@"),
            "the done news"
        );
        assert_eq!(
            task_reply(&await_("task timeout=20")).0,
            "OK 1 status=timeout",
            "armed at rev 2, the news is already read"
        );
        assert_eq!(await_("task @s-other since=1"), AWAIT_TASK_USAGE);
        assert_eq!(await_("task since=x"), AWAIT_TASK_USAGE);
        assert_eq!(await_("timeout=20 task"), AWAIT_TASK_USAGE);
        assert_eq!(await_("timeout 20 task since=1"), AWAIT_TASK_USAGE);
    }

    #[test]
    fn await_task_latches_news_that_comes_and_goes() {
        use crate::task_record::{TaskMark, TaskState, mark_session_task};
        let store = crate::session_store::new_store();
        let workers = three_workers(&store);
        let mark = |worker: &SessionHandle, state: TaskState| {
            mark_session_task(&worker.ctx, worker.local_id, &TaskMark::new(state))
                .expect("unfenced");
        };
        let targets: Vec<TaskTarget<'_>> = workers
            .iter()
            .map(|w| TaskTarget {
                local_id: w.local_id,
                ctx: &w.ctx,
                since: Some(1),
                facts: false,
                fact_since: None,
            })
            .collect();
        let edges = [(1, None); 3];
        let exits: Vec<_> = workers
            .iter()
            .map(|w| exit_probe(&store, w.local_id))
            .collect();
        let mut latched: Vec<Option<String>> = vec![None; 3];
        mark(&workers[0], TaskState::Done);
        latch_task_news(&store, &targets, &edges, &exits, &mut latched);
        let done = latched[0].clone().expect("the done news latches");
        assert!(done.contains(" rev=2 state=done "), "{done}");
        assert!(latched[1].is_none() && latched[2].is_none());
        mark(&workers[0], TaskState::Working);
        mark(&workers[1], TaskState::Done);
        latch_task_news(&store, &targets, &edges, &exits, &mut latched);
        assert_eq!(latched[0].as_deref(), Some(done.as_str()), "the news holds");
        assert!(
            latched[1]
                .as_deref()
                .is_some_and(|row| row.contains(" rev=2 state=done ")),
            "{latched:?}"
        );
        assert!(latched[2].is_none());
        let mut again: Vec<Option<String>> = vec![None; 3];
        latch_task_news(&store, &targets, &edges, &exits, &mut again);
        assert!(again[0].is_none() && again[1].is_some(), "{again:?}");
    }

    /// A PROVEN SELF-CLAIM ADMITS ITS CODEX BEFORE THE SWEEP NAMES THE PROGRAM (CONTRACT §1 of
    /// codex-alab; aterm's `spawn agent=codex` lab leg, 2026-10-23): the worker claimed while
    /// `status` printed `autopilot=-`, so it decided under Cruise in a Land session until the
    /// sweep admitted it. The proof read the claimant leading the foreground group and running an
    /// ALab build, so the session's own word is printed at the claim. NEGATIVE CONTROL: the same
    /// holder's claim unproven prints `-`, as nothing admits it yet.
    #[test]
    fn a_proven_self_claim_prints_its_sessions_word_before_the_sweep_names_it() {
        use aterm_agent::supervise::Autopilot;
        let me = std::process::id();
        let line = |h: &SessionHandle| {
            sessions_lines(
                std::slice::from_ref(h),
                Err("no event loop"),
                crate::autopilot::HarnessPosture::On(Autopilot::Cruise),
                |_| true,
            )
            .lines()
            .nth(1)
            .unwrap()
            .to_string()
        };
        for (local, proven, word) in [(1, true, "land"), (2, false, "-")] {
            let t = Arc::new(Mutex::new(Terminal::new(24, 80)));
            let h = handle(local, &t);
            let _ = crate::session_timeline::apply_autopilot(
                &h.ctx,
                Some("land".to_string()),
                None,
                aterm_slog::Actor::Ctl,
                None,
            );
            crate::session_timeline::claim_supervisor_as(
                &h.ctx,
                &format!("codex-alab@{me}"),
                Some(local),
                None,
                proven.then(|| crate::self_claim::Proven {
                    pid: me,
                    start: "a test identity".to_string(),
                }),
                crate::metrics::now_us(),
                aterm_slog::Actor::Ctl,
            )
            .expect("the claim");
            assert_eq!(
                crate::harness_host::agent_of(h.ctx.timeline.lock().unwrap().agent()),
                None,
                "the sweep has named no program"
            );
            let status = line(&h);
            assert!(
                status.contains(&format!(" autopilot={word} ")),
                "proven={proven}: {status}"
            );
        }
    }

    #[test]
    fn the_roster_path_is_measured_where_a_child_of_the_shell_was_read() {
        use crate::session_status::program::PathVerdict;
        let t0 = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let h = handle(0, &t0);
        let line = |h: &SessionHandle| {
            sessions_lines(
                std::slice::from_ref(h),
                Err("no event loop"),
                crate::autopilot::HarnessPosture::default(),
                |_| true,
            )
            .lines()
            .nth(1)
            .unwrap()
            .to_string()
        };
        let carried = line(&h);
        assert!(carried.contains(" path=frozen program=- "), "{carried}");
        assert!(
            carried.ends_with(
                " path_evidence=carried copy=- upgrade=- land_at=- task=- pause=- task_state=- task_rev=0"
            ),
            "{carried}"
        );
        {
            let mut tl = h.ctx.timeline.lock().unwrap();
            tl.note_foreground_group(7);
            tl.set_leader(8, Some("managed"), Some(PathVerdict::Live));
        }
        let late = line(&h);
        assert!(
            late.contains(" path=frozen program=- ")
                && late.ends_with(
                    " path_evidence=carried copy=- upgrade=- land_at=- task=- pause=- task_state=- \
                     task_rev=0"
                ),
            "a late reading for another group changes nothing: {late}"
        );
        {
            let mut tl = h.ctx.timeline.lock().unwrap();
            tl.set_program(7, Some("claude".into()));
            tl.set_leader(7, Some("foreign"), Some(PathVerdict::Live));
            tl.set_upgrade(Some(aterm_agent::harness::upgrade_drive::Row {
                tab: h.sid.as_str().to_string(),
                to: "2.1.282".into(),
                phase: aterm_agent::harness::upgrade::Phase::Pending,
                behind_since: crate::upgrade_host::now_s() - 120,
                wait: "not-idle:busy".into(),
                ..Default::default()
            }));
        }
        let measured = line(&h);
        assert!(
            measured.contains(" path=live program=claude "),
            "{measured}"
        );
        assert!(measured.contains(" path_evidence=measured:"), "{measured}");
        assert!(
            measured.ends_with(
                " copy=foreign upgrade=pending/2.1.282/not-idle:busy/2m land_at=- task=- \
                 pause=- task_state=- task_rev=0"
            ),
            "{measured}"
        );
        let mut tl = h.ctx.timeline.lock().unwrap();
        assert!(tl.take_path_lowered(), "live: lower the mark");
        assert!(!tl.take_path_lowered(), "owed once");
        tl.set_leader(7, Some("foreign"), Some(PathVerdict::Frozen));
        assert!(!tl.take_path_lowered(), "frozen: nothing is raised");
        assert!(!tl.path_settled_frozen(), "one job is not the shell");
        tl.note_foreground_group(9);
        assert_eq!(tl.copy(), None, "the copy was the old leader's");
        assert!(
            tl.path_reading().is_some(),
            "the PATH is the shell's: it outlives the job"
        );
    }

    #[test]
    fn the_roster_carries_the_published_program_and_agent_verdict() {
        let t0 = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let h = handle(0, &t0);
        let out = sessions_lines(
            std::slice::from_ref(&h),
            Ok(&[]),
            crate::autopilot::HarnessPosture::default(),
            |_| false,
        );
        let line = out.lines().nth(1).unwrap();
        assert!(
            line.contains(" supervisor=- supervisor_proven=0 autopilot=- "),
            "no agent: {line}"
        );
        let (_, cols) = line.split_once(" path=live ").expect("after path=");
        assert!(
            cols.starts_with("program=- agent=- agent_detail=- agent_rev=0 agent_since_ms="),
            "{line}"
        );
        assert!(
            cols.contains(" agent_gen=- agent_fp=- "),
            "no read yet, no stamp: {line}"
        );
        {
            let mut tl = h.ctx.timeline.lock().unwrap();
            tl.note_foreground_group(7);
            tl.set_program(7, Some("claude".into()));
            tl.publish_agent(
                "prompt",
                Some("bash:not-read-only".into()),
                None,
                Some(aterm_phase::Program::Claude),
                crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen { epoch: 2, seq: 40 },
                    fp: 0xab,
                },
            );
        }
        let out = sessions_lines(
            std::slice::from_ref(&h),
            Err("no event loop"),
            crate::autopilot::HarnessPosture::default(),
            |_| false,
        );
        let line = out.lines().nth(1).unwrap();
        assert!(
            line.contains(
                " path=live program=claude agent=prompt agent_detail=bash:not-read-only \
                 agent_rev=1 agent_since_ms="
            ),
            "the failed-hop arm carries them too: {line}"
        );
        assert!(
            line.contains(" agent_gen=2.40 agent_fp=00000000000000ab "),
            "the verdict's stamp rides the row: {line}"
        );
        assert!(
            line.ends_with(
                " agent_fp=00000000000000ab program_state=- human_ms=- role=- attention=- \
                 user_title=- supervisor=- supervisor_proven=0 autopilot=off path_evidence=- \
                 copy=- upgrade=- land_at=- task=- pause=- task_state=- task_rev=0"
            ),
            "{line}"
        );
        crate::app_input::note_person(&h.ctx, crate::human_input::Gesture::Other);
        let out = sessions_lines(
            std::slice::from_ref(&h),
            Err("no event loop"),
            crate::autopilot::HarnessPosture::default(),
            |_| false,
        );
        let line = out.lines().nth(1).unwrap();
        let human = line
            .split_whitespace()
            .find_map(|t| t.strip_prefix("human_ms="))
            .unwrap();
        assert!(human.parse::<u64>().is_ok(), "{line}");
        assert!(
            line.ends_with(
                " supervisor=- supervisor_proven=0 autopilot=off path_evidence=- copy=- \
                 upgrade=- land_at=- task=- pause=- task_state=- task_rev=0"
            ),
            "{line}"
        );
        let cruise =
            crate::autopilot::HarnessPosture::On(aterm_agent::supervise::Autopilot::Cruise);
        let autopilot = |posture| {
            let out = sessions_lines(std::slice::from_ref(&h), Ok(&[]), posture, |_| false);
            out.lines()
                .nth(1)
                .unwrap()
                .split_whitespace()
                .find_map(|t| t.strip_prefix("autopilot="))
                .unwrap()
                .to_string()
        };
        assert_eq!(autopilot(cruise), "cruise");
        h.ctx
            .meta
            .lock()
            .unwrap()
            .set("autopilot", Some("land".into()));
        assert_eq!(autopilot(cruise), "land");
        assert_eq!(
            autopilot(crate::autopilot::HarnessPosture::MasterOff),
            "off",
            "the master switch wins over the word"
        );
    }

    #[test]
    fn sessions_lines_carry_the_placement_columns_from_the_rows() {
        let t0 = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let t1 = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let t2 = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let snapshot = vec![handle(0, &t0), handle(1, &t1), handle(2, &t2)];
        let rows = vec![row(0, 0, true, false), row(1, 3, false, true)];
        let out = sessions_lines(
            &snapshot,
            Ok(&rows),
            crate::autopilot::HarnessPosture::default(),
            |_| false,
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "OK 3");
        assert_eq!(
            tail(lines[1]),
            "0 - alive tab-0 meta=0 window=0 active=1 wfocus=0 detail=- path=live"
        );
        assert_eq!(
            tail(lines[2]),
            "1 - alive tab-1 meta=0 window=3 active=0 wfocus=1 detail=- path=live"
        );
        assert_eq!(
            tail(lines[3]),
            "2 - alive tab-2 meta=0 window=none active=0 wfocus=0 detail=- path=live"
        );
    }

    #[test]
    fn sessions_lines_print_the_title_the_strip_settles_on() {
        let bare = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let fresh = Arc::new(Mutex::new(Terminal::new(24, 80)));
        {
            let mut t = fresh.lock().unwrap();
            t.set_title("C:\\Program Files\\PowerShell\\7\\pwsh.exe");
            t.process(b"\x1b]7;file://localhost/srv/demo-cwd\x07");
        }
        let settled = Arc::new(Mutex::new(Terminal::new(24, 80)));
        settled
            .lock()
            .unwrap()
            .set_title("user@m17-tower: ~\\aterm");
        let running = Arc::new(Mutex::new(Terminal::new(24, 80)));
        running
            .lock()
            .unwrap()
            .set_title("C:\\Windows\\system32\\cmd.exe - ping build-01");
        let named = Arc::new(Mutex::new(Terminal::new(24, 80)));
        named.lock().unwrap().set_title("~\\aterm");
        let snapshot = vec![
            handle(0, &bare),
            handle(1, &fresh),
            handle(2, &settled),
            handle(3, &running),
            handle(4, &named),
        ];
        snapshot[4].ctx.meta.lock().unwrap().user_title = Some("ops shell".to_string());
        let out = sessions_lines(
            &snapshot,
            Err("no event loop"),
            crate::autopilot::HarnessPosture::default(),
            |_| false,
        );
        let titles: Vec<&str> = out
            .lines()
            .skip(1)
            .map(|line| line.split_whitespace().nth(4).unwrap())
            .collect();
        assert_eq!(
            titles,
            [
                "tab-0",
                "/srv/demo-cwd",
                "user@m17-tower:%20~\\aterm",
                "ping%20build-01",
                "ops%20shell"
            ]
        );

        let mut stale = handle(5, &fresh);
        stale.title = "C:\\Windows\\system32\\cmd.exe".to_string();
        let mut empty = handle(6, &fresh);
        empty.title.clear();
        let held = fresh.lock().unwrap();
        let out = sessions_lines(
            &[stale, empty],
            Err("no event loop"),
            crate::autopilot::HarnessPosture::default(),
            |_| false,
        );
        drop(held);
        let titles: Vec<&str> = out
            .lines()
            .skip(1)
            .map(|line| line.split_whitespace().nth(4).unwrap())
            .collect();
        assert_eq!(titles, ["aterm", "aterm"]);
        for line in out.lines().skip(1) {
            assert_eq!(
                line.split_whitespace().nth(5),
                Some("meta=0"),
                "every column keeps its place: {line:?}"
            );
        }
    }

    #[test]
    fn family_prints_the_roster_title() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        {
            let mut t = term.lock().unwrap();
            t.set_title("C:\\Program Files\\PowerShell\\7\\pwsh.exe");
            t.process(b"\x1b]7;file://localhost/srv/demo-cwd\x07");
        }
        let h = handle(0, &term);
        let store = crate::session_store::new_store();
        store.write().unwrap().register(h.clone());
        let family = cmd_family(&h.ctx, &store, Scope::Owner, "");
        assert_eq!(
            family.lines().nth(1),
            Some(format!("self {} alive /srv/demo-cwd", h.sid.as_str()).as_str()),
            "{family}"
        );
        let sessions = sessions_lines(
            &[h],
            Err("no event loop"),
            crate::autopilot::HarnessPosture::default(),
            |_| false,
        );
        assert_eq!(
            sessions
                .lines()
                .nth(1)
                .and_then(|line| line.split_whitespace().nth(4)),
            Some("/srv/demo-cwd"),
            "{sessions}"
        );
    }

    #[test]
    fn sessions_lines_keep_every_line_when_the_hop_fails() {
        let t0 = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let t1 = Arc::new(Mutex::new(Terminal::new(24, 80)));
        t1.lock()
            .unwrap()
            .process(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 30\n\x1b]133;C\x07");
        let snapshot = vec![handle(0, &t0), handle(1, &t1)];
        let out = sessions_lines(
            &snapshot,
            Err("main-thread reply did not arrive within 30s"),
            crate::autopilot::HarnessPosture::default(),
            |_| false,
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "OK 2");
        assert_eq!(
            tail(lines[1]),
            "0 - alive tab-0 meta=0 window=- active=- wfocus=- detail=- path=live"
        );
        assert_eq!(
            tail(lines[2]),
            "1 - alive tab-1 meta=0 window=- active=- wfocus=- detail=sleep path=live"
        );
    }

    #[test]
    fn the_roster_carries_each_sessions_public_launch_nonce() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let h = handle(0, &term);
        let expect = h.nonce.to_hex();
        let out = sessions_lines(
            &[h],
            Err("no event loop"),
            crate::autopilot::HarnessPosture::default(),
            |_| false,
        );
        let row = out.lines().nth(1).expect("one session row");
        assert!(
            row.contains(&format!(" nonce={expect} ")),
            "the roster must carry the launch nonce: {row}"
        );
        assert_eq!(expect.len(), 32, "{expect}");
        let fields: Vec<&str> = row.split_whitespace().collect();
        let meta_at = fields.iter().position(|f| f.starts_with("meta=")).unwrap();
        let nonce_at = fields.iter().position(|f| f.starts_with("nonce=")).unwrap();
        assert_eq!(nonce_at, meta_at + 1, "{row}");
    }

    #[test]
    fn bridge_roster_matches_public_identities_without_per_session_locks() {
        let store = crate::session_store::new_store();
        let t0 = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let t1 = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let h0 = handle(9, &t0);
        let h1 = handle(2, &t1);
        {
            let mut g = store.write().unwrap();
            g.register(h0.clone());
            g.register(h1.clone());
        }
        let full = cmd_sessions_store(&store, None);
        let expected: Vec<String> = full
            .lines()
            .skip(1)
            .map(|row| {
                let mut cols = row.split_whitespace();
                let local = cols.next().unwrap();
                let sid = cols.next().unwrap();
                let nonce = cols.find(|tok| tok.starts_with("nonce=")).unwrap();
                format!("{local} {sid} {nonce}")
            })
            .collect();
        assert_eq!(expected.len(), 2);

        let _term = t0.lock().unwrap();
        let _meta = h0.ctx.meta.lock().unwrap();
        let _timeline = h0.ctx.timeline.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let _ = tx.send(cmd_sessions_bridge(&store));
        });
        let bridge = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("identity-only roster must not wait on terminal, meta or timeline");
        let mut lines = bridge.lines();
        assert_eq!(lines.next(), Some("OK 2"));
        assert_eq!(lines.map(str::to_string).collect::<Vec<_>>(), expected);
        assert!(!bridge.contains("window=") && !bridge.contains("detail="));
        worker.join().unwrap();
    }

    #[test]
    fn sessions_lines_detail_is_the_running_command_or_a_dash() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        term.lock().unwrap().process(
            b"\x1b]133;A\x07$ \x1b]133;B\x07targo --unverified test -p aterm-gui\n\x1b]133;C\x07",
        );
        let snapshot = vec![handle(0, &term)];
        let rows = vec![row(0, 0, true, true)];
        let out = sessions_lines(
            &snapshot,
            Ok(&rows),
            crate::autopilot::HarnessPosture::default(),
            |_| false,
        );
        assert!(
            out.contains(" window=0 active=1 wfocus=1 detail=targo%20test path=live "),
            "{out}"
        );
        assert!(
            !out.contains("aterm-gui"),
            "arguments never reach the wire: {out}"
        );

        let held = term.lock().unwrap();
        let out = sessions_lines(
            &snapshot,
            Ok(&rows),
            crate::autopilot::HarnessPosture::default(),
            |_| false,
        );
        assert!(
            out.contains(" detail=- path=live "),
            "contended reads `-` (and the published columns need no terminal lock): {out}"
        );
        drop(held);

        term.lock().unwrap().process(b"done\n\x1b]133;D;0\x07");
        let out = sessions_lines(
            &snapshot,
            Ok(&rows),
            crate::autopilot::HarnessPosture::default(),
            |_| false,
        );
        assert!(
            out.contains(" detail=- path=live "),
            "complete reads `-`: {out}"
        );
    }

    #[test]
    fn sessions_lines_name_the_frozen_path_tab_as_the_last_column() {
        let t0 = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let t1 = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let snapshot = vec![handle(0, &t0), handle(1, &t1)];
        let rows = vec![row(0, 0, true, true), row(1, 0, false, true)];
        let out = sessions_lines(
            &snapshot,
            Ok(&rows),
            crate::autopilot::HarnessPosture::default(),
            |id| id == 0,
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            tail(lines[1]),
            "0 - alive tab-0 meta=0 window=0 active=1 wfocus=1 detail=- path=frozen"
        );
        assert_eq!(
            tail(lines[2]),
            "1 - alive tab-1 meta=0 window=0 active=0 wfocus=1 detail=- path=live"
        );
        let out = sessions_lines(
            &snapshot,
            Err("no event loop"),
            crate::autopilot::HarnessPosture::default(),
            |id| id == 0,
        );
        assert!(
            out.lines()
                .nth(1)
                .unwrap()
                .contains(" detail=- path=frozen program=-"),
            "the failed-hop arm carries the mark too: {out}"
        );
        let store = crate::session_store::new_store();
        {
            let mut g = store.write().unwrap();
            g.register(handle(7, &t0));
            g.register(handle(8, &t1));
            g.mark_frozen_path(7);
        }
        let out = cmd_sessions_store(&store, None);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "OK 2");
        assert!(
            tail(lines[1]).ends_with(" detail=- path=frozen"),
            "the marked session reads frozen: {out}"
        );
        assert!(
            tail(lines[2]).ends_with(" detail=- path=live"),
            "an unmarked session reads live: {out}"
        );
    }

    fn temporal_ctx() -> crate::SessionCtx {
        let observed = crate::session_watch::Observed::default();
        crate::SessionCtx {
            sink: Arc::new(aterm_session::sink::SinkWriter::new(-1)),
            output_echo: Arc::new(crate::app_input::OutputEchoTracker::default()),
            modes: Arc::default(),
            ui_waiting: Arc::default(),
            edges: std::sync::Arc::new(std::sync::Mutex::new(EdgeTable::new())),
            turn_lease: std::sync::Mutex::new(None),
            self_id: SessionId::generate(),
            nonce: LaunchNonce::generate(),
            cast: Arc::new(std::sync::Mutex::new(crate::cast::CastRecorder::new(
                80, 24,
            ))),
            temporal: Arc::new(std::sync::Mutex::new(
                crate::temporal::TemporalRecorder::new(),
            )),
            byte_fanout: Arc::new(crate::cast::ByteFanout::new()),
            turns: observed.turns,
            meta: std::sync::Mutex::new(crate::session_timeline::SessionMeta::default()),
            timeline: observed.timeline,
            fabric: std::sync::Arc::default(),
            idem: Default::default(),
            rewrap_gauge: std::sync::Arc::default(),
            human_input: Default::default(),
            licence: Default::default(),
            launch: Default::default(),
            reset_lane: Default::default(),
            crash_journal: Default::default(),
            typed: Default::default(),
            bells: Default::default(),
            screen_moves: Default::default(),
            job_feed: Default::default(),
            box_banner: Default::default(),
            update_parked: Default::default(),
            watchers: observed.watchers,
            frame: Default::default(),
            witness: Default::default(),
            fenced_writes: Default::default(),
        }
    }

    #[test]
    fn temporal_trim_byte_frames_the_trimmed_body_and_status_trim_is_usage() {
        let ctx = temporal_ctx();
        let rows: usize = 8;
        let live = Mutex::new(aterm_core::terminal::Terminal::new(rows as u16, 20));
        let input = b"first line\r\nsecond line\r\n";
        let (base, at) = {
            let mut t = term_lock(&live);
            let base = (t.checkpoint(), crate::temporal::SpinePre::take(&t));
            t.process(input);
            base
        };
        {
            let mut rec = ctx.temporal.lock().unwrap();
            rec.record_keyframe(base);
            rec.spine_watermark().seed(at);
            rec.record_raw_in(input);
        }

        let bare = cmd_temporal(&ctx, &live, "");
        assert!(bare.starts_with("OK "), "bare temporal: {bare:?}");

        let trimmed = cmd_temporal(&ctx, &live, "trim");
        let (header, body) = trimmed
            .split_once('\n')
            .expect("a header line then the body");
        let mut tok = header.split_whitespace();
        assert_eq!(tok.next(), Some("OK"));
        let nbytes: usize = tok
            .next()
            .expect("byte count")
            .parse()
            .expect("nbytes is a number");
        assert_eq!(tok.next(), Some("trimmed=6"), "six blank rows dropped");
        assert_eq!(nbytes, body.len(), "header nbytes == body bytes");
        assert_eq!(body.lines().count(), 2, "only the two written rows survive");
        assert!(
            body.trim_end_matches('\n').ends_with("second line"),
            "body ends on the last non-blank row: {body:?}"
        );

        let status_trim = cmd_temporal(&ctx, &live, "status trim");
        assert!(
            status_trim.starts_with("ERR usage: temporal"),
            "status trim is a usage error: {status_trim:?}"
        );
    }

    #[test]
    fn temporal_status_says_where_replay_starts() {
        let live = Mutex::new(aterm_core::terminal::Terminal::new(6, 20));
        let reachable_from = |status: &str| {
            status
                .split_whitespace()
                .find_map(|kv| kv.strip_prefix("reachable_from="))
                .map(str::to_owned)
        };
        let ctx = temporal_ctx();
        let base = term_lock(&live).checkpoint();
        let tick = {
            let mut rec = ctx.temporal.lock().unwrap();
            rec.record_keyframe(base);
            rec.record_raw_in(b"a");
            rec.reachable_from().expect("the keyframe replays")
        };
        let status = cmd_temporal(&ctx, &live, "status");
        assert!(status.starts_with("OK enabled=true "), "{status}");
        assert_eq!(reachable_from(&status), Some(tick.0.to_string()));

        let dead = temporal_ctx();
        dead.temporal.lock().unwrap().record_raw_in(b"a");
        let status = cmd_temporal(&dead, &live, "status");
        assert!(status.starts_with("OK enabled=true "), "{status}");
        assert_eq!(reachable_from(&status).as_deref(), Some("-"));
        assert_eq!(cmd_temporal(&dead, &live, ""), "ERR temporal unreachable\n");
    }

    #[test]
    fn temporal_replays_with_the_recorder_unlocked() {
        use std::cell::Cell;
        use std::rc::Rc;
        let ctx = temporal_ctx();
        let live = Mutex::new(aterm_core::terminal::Terminal::new(6, 20));
        {
            let mut rec = ctx.temporal.lock().unwrap();
            rec.record_keyframe(term_lock(&live).checkpoint());
            rec.record_raw_in(b"a");
        }
        let held = Rc::new(Cell::new(None));
        let _hook = crate::temporal::replay_hook::install({
            let (temporal, held) = (ctx.temporal.clone(), held.clone());
            move || held.set(Some(temporal.try_lock().is_err()))
        });
        assert!(cmd_temporal(&ctx, &live, "").starts_with("OK "));
        assert_eq!(held.take(), Some(false), "replayed with the recorder held");
        let rec = ctx.temporal.lock().unwrap();
        let _ = rec.replay_plan(None).map(crate::temporal::ReplayPlan::run);
        drop(rec);
        assert_eq!(held.take(), Some(true), "the probe sees a held recorder");
    }

    /// `temporal status` counts the live witnesses a `temporal` fold judged: a recorded session's all match; replayed on the live clock one diverges and the next slice records a live keyframe.
    #[test]
    fn temporal_status_counts_the_witnesses_a_replay_judged() {
        use crate::temporal::{InputSlice, SpineMark, SpinePre};
        use aterm_core::terminal::ClockReading;
        let key = |status: &str, key: &str| {
            status
                .split_whitespace()
                .find_map(|kv| kv.strip_prefix(key))
                .map(str::to_owned)
        };
        for live_clock in [false, true] {
            let ctx = temporal_ctx();
            let live = Mutex::new(aterm_core::terminal::Terminal::new(6, 30));
            let watermark = {
                let t = term_lock(&live);
                let mut rec = ctx.temporal.lock().unwrap();
                rec.record_keyframe(t.checkpoint());
                rec.spine_watermark().seed(SpinePre::take(&t));
                rec.spine_watermark()
            };
            let (tx, rx) = crate::temporal::queue(64);
            let mut mark = SpineMark::attach(watermark);
            let t0 = ClockReading::now();
            let mut hold = |bytes: &[u8], ms: u64| {
                let at = ClockReading {
                    monotonic: t0.monotonic + std::time::Duration::from_millis(ms),
                    wall_ms: t0.wall_ms.map(|w| w + ms),
                };
                {
                    let mut t = term_lock(&live);
                    let pre = SpinePre::take(&t);
                    t.process_at(bytes, at);
                    mark.record(&tx, pre, &t, InputSlice::at(bytes, at));
                }
                let mut rec = ctx.temporal.lock().unwrap();
                while let Some(msg) = rx.try_recv() {
                    rec.apply_inline(msg);
                }
            };
            hold(b"\x1b[?2026hframe", 0);
            hold(b"\r\none", 1500);
            let status = cmd_temporal(&ctx, &live, "status");
            assert_eq!(key(&status, "witnessed=").as_deref(), Some("0"), "{status}");
            let _clock = live_clock.then(crate::temporal::replay_clock::on_live_clock);
            assert!(cmd_temporal(&ctx, &live, "").starts_with("OK "));
            let status = cmd_temporal(&ctx, &live, "status");
            assert_eq!(key(&status, "witnessed=").as_deref(), Some("1"), "{status}");
            assert_eq!(
                key(&status, "diverged=").as_deref(),
                Some(if live_clock { "1" } else { "0" }),
                "{status}"
            );
            hold(b"\r\ntwo", 1600);
            let status = cmd_temporal(&ctx, &live, "status");
            assert_eq!(
                key(&status, "keyframes=").as_deref(),
                Some(if live_clock { "2" } else { "1" }),
                "a mismatch takes a live keyframe: {status}"
            );
        }
    }

    #[test]
    fn temporal_status_discloses_dropped_input_and_an_open_break() {
        use crate::temporal::{SpineMark, SpinePre, TemporalTx};
        fn hold(live: &Mutex<Terminal>, mark: &mut SpineMark, tx: &TemporalTx, bytes: &[u8]) {
            static CLOCK: std::sync::OnceLock<aterm_core::terminal::ClockReading> =
                std::sync::OnceLock::new();
            let clock = *CLOCK.get_or_init(aterm_core::terminal::ClockReading::now);
            let mut t = term_lock(live);
            let pre = SpinePre::take(&t);
            t.process_at(bytes, clock);
            mark.record(tx, pre, &t, crate::temporal::InputSlice::at(bytes, clock));
        }
        let key = |status: &str, key: &str| {
            status
                .split_whitespace()
                .find_map(|kv| kv.strip_prefix(key))
                .map(str::to_owned)
        };
        let ctx = temporal_ctx();
        let live = Mutex::new(aterm_core::terminal::Terminal::new(6, 20));
        let (base, at) = {
            let t = term_lock(&live);
            (t.checkpoint(), SpinePre::take(&t))
        };
        let watermark = {
            let mut rec = ctx.temporal.lock().unwrap();
            rec.record_keyframe(base);
            rec.spine_watermark().seed(at);
            rec.spine_watermark()
        };
        let (tx, rx) = crate::temporal::queue(1);
        let drain = || {
            let mut rec = ctx.temporal.lock().unwrap();
            while let Some(msg) = rx.try_recv() {
                rec.apply_inline(msg);
            }
        };
        let mut mark = SpineMark::attach(watermark);
        hold(&live, &mut mark, &tx, b"one\r\n");
        hold(&live, &mut mark, &tx, b"two\r\n");
        mark.reply(&tx);
        drain();
        let status = cmd_temporal(&ctx, &live, "status");
        assert_eq!(
            key(&status, "dropped_input=").as_deref(),
            Some("1"),
            "{status}"
        );
        assert_eq!(
            key(&status, "dropped_replies=").as_deref(),
            Some("1"),
            "{status}"
        );
        assert_eq!(
            key(&status, "reachable_from=").as_deref(),
            Some("-"),
            "{status}"
        );
        assert_eq!(cmd_temporal(&ctx, &live, ""), "ERR temporal unreachable\n");

        hold(&live, &mut mark, &tx, b"three\r\n");
        drain();
        let status = cmd_temporal(&ctx, &live, "status");
        assert_ne!(
            key(&status, "reachable_from=").as_deref(),
            Some("-"),
            "{status}"
        );
        let screen: String = {
            let t = term_lock(&live);
            (0..t.rows() as usize)
                .map(|r| format!("{}\n", crate::control::visible_row(&t, r)))
                .collect()
        };
        let reply = cmd_temporal(&ctx, &live, "");
        assert_eq!(
            reply.split_once('\n').map(|(_, body)| body),
            Some(&screen[..])
        );
    }

    #[test]
    fn temporal_discloses_resizes_still_pending_on_the_spine() {
        use crate::temporal::{SpineMark, SpinePre};
        const ROWS: u16 = 6;
        const COLS: u16 = 20;
        let ctx = temporal_ctx();
        let live = Mutex::new(aterm_core::terminal::Terminal::new(ROWS, COLS));
        let (base, at) = {
            let t = term_lock(&live);
            (t.checkpoint(), SpinePre::take(&t))
        };
        let watermark = {
            let mut rec = ctx.temporal.lock().unwrap();
            rec.record_keyframe(base);
            rec.spine_watermark().seed(at);
            rec.spine_watermark()
        };
        let (tx, rx) = crate::temporal::queue(64);
        let mut mark = SpineMark::attach(watermark);
        let clock = aterm_core::terminal::ClockReading::now();
        let mut slice = |bytes: &[u8]| {
            {
                let mut t = term_lock(&live);
                let pre = SpinePre::take(&t);
                t.process_at(bytes, clock);
                mark.record(&tx, pre, &t, crate::temporal::InputSlice::at(bytes, clock));
            }
            let mut rec = ctx.temporal.lock().unwrap();
            while let Some(msg) = rx.try_recv() {
                rec.apply_inline(msg);
            }
        };
        let mut frame = String::from("\x1b[?1049h\x1b[2J");
        for r in 0..ROWS {
            frame.push_str(&format!(
                "\x1b[{row};1HL{r}\x1b[{row};{COLS}HE",
                row = r + 1
            ));
        }
        frame.push_str("\x1b[4;3H");
        slice(frame.as_bytes());
        let screen = || -> String {
            let t = term_lock(&live);
            (0..t.rows() as usize)
                .map(|r| format!("{}\n", crate::control::visible_row(&t, r)))
                .collect()
        };
        let replayed = |reply: &str| -> (String, String) {
            let (head, body) = reply.split_once('\n').expect("header + body");
            (head.to_string(), body.to_string())
        };
        assert!(
            cmd_temporal(&ctx, &live, "status").contains(" pending_resizes=0 "),
            "nothing pending before the flap"
        );

        {
            let mut t = term_lock(&live);
            t.resize(ROWS, COLS - 1);
            t.resize(ROWS, COLS);
        }
        let status = cmd_temporal(&ctx, &live, "status");
        assert!(status.contains(" pending_resizes=2 "), "{status}");
        let (head, body) = replayed(&cmd_temporal(&ctx, &live, ""));
        assert!(head.ends_with(" pending_resizes=2"), "{head}");
        assert_ne!(
            body,
            screen(),
            "the spine lacks the flap until the next output"
        );

        slice(b"\x1b[1;1HT1\x1b[K\x1b[4;3H");
        let status = cmd_temporal(&ctx, &live, "status");
        assert!(status.contains(" pending_resizes=0 "), "{status}");
        let (head, body) = replayed(&cmd_temporal(&ctx, &live, ""));
        assert!(!head.contains("pending_resizes"), "{head}");
        assert_eq!(body, screen(), "both halves handed over: the live screen");
    }

    fn flapped_session() -> (Arc<Mutex<Terminal>>, Mutex<crate::cast::CastRecorder>) {
        flapped_session_split(true)
    }

    fn quiet_flapped_session() -> (Arc<Mutex<Terminal>>, Mutex<crate::cast::CastRecorder>) {
        flapped_session_split(false)
    }

    fn flapped_session_split(
        split: bool,
    ) -> (Arc<Mutex<Terminal>>, Mutex<crate::cast::CastRecorder>) {
        use std::time::Duration;
        let term = Arc::new(Mutex::new(Terminal::new(8, 20)));
        let cast = Mutex::new(crate::cast::CastRecorder::new(20, 8));
        let mut frame = String::from("\x1b[?1049h\x1b[?2026h\x1b[2J\x1b[H");
        for r in 0..8 {
            frame.push_str(&format!("\x1b[{};1HA{r}", r + 1));
        }
        frame.push_str("\x1b[6;3H\x1b[?2026l");
        use crate::cast::rig;
        rig::burst(&term, &cast, Duration::from_millis(100), frame.as_bytes());
        for (us, rows) in [(1_000_000u64, 7u16), (1_000_700, 8)] {
            if split && rows == 8 {
                rig::burst(
                    &term,
                    &cast,
                    Duration::from_micros(1_000_300),
                    b"\x1b[?1000h",
                );
            }
            rig::resize(&term, &cast, Duration::from_micros(us), rows, 20);
        }
        rig::burst(
            &term,
            &cast,
            Duration::from_millis(1_006),
            b"\x1b[?2026h\x1b[3;1H\x1b[KB2\x1b[6;3H\x1b[?2026l",
        );
        (term, cast)
    }

    #[test]
    fn cmd_cast_drift_frames_lines_and_names_the_flap() {
        let (term, cast) = flapped_session();
        let reply = cmd_cast_drift(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            &cast,
            aterm_core::grid::ResizePolicy::Native,
            "",
        );
        let mut lines = reply.lines();
        let head = lines.next().expect("a header");
        let n: usize = head
            .split_whitespace()
            .nth(1)
            .and_then(|v| v.parse().ok())
            .expect("OK <n>");
        assert_eq!(lines.clone().count(), n, "{reply}");
        assert!(reply.ends_with('\n'));
        for key in [
            "verdict=desync",
            "fidelity=pass",
            "culprit=1.000000",
            "cut=quiescent",
            "dropped=0",
            "folds=2",
        ] {
            assert!(head.split_whitespace().any(|t| t == key), "{key} in {head}");
        }
        assert_eq!(
            lines.next(),
            Some(
                "run t=1.000000 n=2 gap_ms=0.700 geom=20x8>20x7>20x8 net=zero first_out_ms=5.300 \
                 clear_ms=- culprit=1"
            )
        );
        assert_eq!(lines.next(), Some("row 0 live=A1 expected=A0"));
        assert_eq!(
            cmd_cast_drift(
                &term,
                &crate::frame_memo::FrameSlot::default(),
                &cast,
                aterm_core::grid::ResizePolicy::Native,
                "frames"
            ),
            format!("{}\n", aterm_control::cast_drift::USAGE)
        );
    }

    #[test]
    fn cmd_cast_drift_answers_clean_for_a_quiet_flap() {
        let (term, cast) = quiet_flapped_session();
        let reply = cmd_cast_drift(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            &cast,
            aterm_core::grid::ResizePolicy::Native,
            "",
        );
        let head = reply.lines().next().expect("a header");
        for key in ["verdict=clean", "fidelity=pass", "culprit=-"] {
            assert!(head.split_whitespace().any(|t| t == key), "{key} in {head}");
        }
        assert!(!reply.contains(" live="), "no drifted row: {reply}");
    }

    #[test]
    fn cmd_cast_drift_answers_a_racy_cut_from_the_replays() {
        let (term, cast) = flapped_session();
        let moves = std::cell::Cell::new(0u64);
        let reply = cast_drift_with(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            &cast,
            aterm_core::grid::ResizePolicy::Native,
            "",
            CAST_DRIFT_CATCH_UP,
            &|| {
                moves.set(moves.get() + 1);
                let t = std::time::Duration::from_secs(2 + moves.get());
                crate::cast::rig::burst(&term, &cast, t, b"\x1b[1;1Hmoved");
            },
        );
        assert_eq!(
            moves.get(),
            CAST_DRIFT_CUT_ATTEMPTS as u64,
            "bounded retries"
        );
        let head = reply.lines().next().unwrap();
        assert!(
            head.contains(" verdict=desync fidelity=- against=replay "),
            "{head}"
        );
        assert!(head.contains(" culprit=1.000000 "), "{head}");
        assert!(head.contains(" cut=racy "), "{head}");
        assert!(
            !reply.contains(" live=moved"),
            "the moving screen is not read"
        );
    }

    #[test]
    fn cast_drift_calls_no_cut_quiescent_on_a_frame_still_unfinished() {
        use std::time::Duration;
        let (term, cast) = quiet_flapped_session();
        let drift = || {
            let reply = cmd_cast_drift(
                &term,
                &crate::frame_memo::FrameSlot::default(),
                &cast,
                aterm_core::grid::ResizePolicy::Native,
                "",
            );
            reply.lines().next().expect("a header").to_string()
        };
        crate::cast::rig::burst(
            &term,
            &cast,
            Duration::from_secs(2),
            b"\x1b[?2026h\x1b[1;1Hhalf",
        );
        let head = drift();
        assert!(head.contains(" cut=racy "), "{head}");
        crate::cast::rig::burst(&term, &cast, Duration::from_secs(3), b"\x1b[?2026l");
        let head = drift();
        assert!(head.contains(" cut=quiescent "), "{head}");
    }

    #[test]
    fn cast_cut_is_consistent_without_sleeping() {
        use crate::cast::cut_probe;
        let (term, cast) = quiet_flapped_session();
        let waits = cut_probe::waits();
        let reply = cmd_cast_drift(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            &cast,
            aterm_core::grid::ResizePolicy::Native,
            "",
        );
        let head = reply.lines().next().expect("a header");
        assert!(head.contains(" cut=quiescent "), "{head}");
        assert_eq!(
            cut_probe::waits(),
            waits,
            "the cut waited on a recording that held it"
        );

        let _unrecorded = crate::cast::rig::hold(&term, &cast, b"\x1b[2;1Hpending");
        let reply = cast_drift_with(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            &cast,
            aterm_core::grid::ResizePolicy::Native,
            "",
            std::time::Duration::ZERO,
            &|| {},
        );
        let head = reply.lines().next().expect("a header");
        assert!(head.contains(" cut=racy "), "{head}");
        assert_eq!(cut_probe::waits(), waits + CAST_DRIFT_CUT_ATTEMPTS);
    }

    #[test]
    fn cast_cut_waits_for_the_resize_and_burst_the_screen_shows() {
        use std::time::Duration;

        use crate::cast::rig;
        let policy = aterm_core::grid::ResizePolicy::Native;
        let pending = |term: &Mutex<Terminal>, cast: &Mutex<crate::cast::CastRecorder>| {
            let stamp = rig::resize_hold(term, cast, Duration::from_secs(2), 7, 20);
            let diff = b"\x1b[4;1HC3";
            (stamp, rig::hold(term, cast, diff), diff)
        };
        for which in ["resize", "burst"] {
            let (term, cast) = flapped_session();
            let (stamp, cuts, diff) = pending(&term, &cast);
            match which {
                "resize" => cast.lock().unwrap().record_burst(
                    Arc::from(&diff[..]),
                    cuts,
                    Duration::from_secs(3),
                ),
                _ => cast.lock().unwrap().record_resize_stamped(stamp, 20, 7),
            }
            let reply = cast_drift_with(
                &term,
                &crate::frame_memo::FrameSlot::default(),
                &cast,
                policy,
                "",
                Duration::ZERO,
                &|| {},
            );
            let head = reply.lines().next().expect("a header");
            assert!(
                head.contains(" cut=racy "),
                "the {which} is not recorded: {head}"
            );
        }

        let (term, cast) = flapped_session();
        let (stamp, cuts, diff) = pending(&term, &cast);
        let reply = std::thread::scope(|scope| {
            scope.spawn(|| {
                aterm_spec::wait::until_within(
                    aterm_spec::wait::MUST_HAPPEN,
                    "the cut to wait",
                    || (cast.lock().unwrap().waiters() > 0).then_some(()),
                );
                let mut rec = cast.lock().unwrap();
                rec.record_resize_stamped(stamp, 20, 7);
                rec.record_burst(Arc::from(&diff[..]), cuts, Duration::from_secs(3));
            });
            cast_drift_with(
                &term,
                &crate::frame_memo::FrameSlot::default(),
                &cast,
                policy,
                "",
                aterm_spec::wait::MUST_HAPPEN,
                &|| {},
            )
        });
        let head = reply.lines().next().expect("a header");
        assert!(head.contains(" cut=quiescent "), "{head}");
        assert!(head.contains(" fidelity=pass "), "{head}");
    }

    #[test]
    fn cast_drift_slots_cap_the_concurrent_folds() {
        static SLOTS: DriftSlots = DriftSlots {
            busy: Mutex::new(0),
            freed: std::sync::Condvar::new(),
        };
        let short = std::time::Duration::from_millis(20);
        let held: Vec<DriftSlot> = (0..CAST_DRIFT_CONCURRENCY)
            .map(|_| SLOTS.acquire(short).expect("a free slot"))
            .collect();
        assert!(SLOTS.acquire(short).is_none(), "every slot is taken");
        drop(held);
        assert!(SLOTS.acquire(short).is_some(), "released slots are free");
    }
}

#[cfg(test)]
mod exits_tests {
    use aterm_session::{EdgeToken, Op};

    use super::{Scope, caller_actor, caller_actor_of, cmd_exits, turn_by, typed_by};
    use crate::session_store::{ExitActor, ExitReason, RosterChange, new_store, test_handle};
    use crate::turn_ledger::TurnBy;

    #[test]
    fn no_exits_is_ok_zero() {
        let store = new_store();
        assert_eq!(cmd_exits(&store, ""), "OK 0\n");
        store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .register(test_handle(1));
        assert_eq!(cmd_exits(&store, ""), "OK 0\n", "a birth is not an exit");
    }

    #[test]
    fn formats_every_reason_actor_and_exit_code() {
        let store = new_store();
        let mut sids = Vec::new();
        {
            let mut g = store.write().unwrap_or_else(|p| p.into_inner());
            for (i, reason) in ExitReason::ALL.iter().enumerate() {
                let local = i as u64 + 10;
                let h = test_handle(local);
                sids.push(h.sid.as_str().to_string());
                g.register(h);
                let actor = match i % 3 {
                    0 => ExitActor::Sid(format!("s-driver{i}")),
                    1 => ExitActor::Human,
                    _ => ExitActor::Unknown,
                };
                if i % 2 == 0 {
                    g.note_exit_code(local, Some(i32::try_from(i).unwrap()));
                }
                g.deregister_local_as(local, *reason, actor);
            }
        }
        let reply = cmd_exits(&store, "");
        let lines: Vec<&str> = reply.lines().collect();
        assert_eq!(lines[0], format!("OK {}", ExitReason::ALL.len()));
        let ledger: Vec<(u64, u64)> = {
            let g = store.read().unwrap_or_else(|p| p.into_inner());
            g.roster_since(0)
                .filter(|r| r.change == RosterChange::Exited)
                .map(|r| (r.seq, r.t_ms))
                .collect()
        };
        for (i, reason) in ExitReason::ALL.iter().enumerate() {
            let (seq, t) = ledger[i];
            let by = match i % 3 {
                0 => format!("s-driver{i}"),
                1 => "human".to_string(),
                _ => "-".to_string(),
            };
            let code = if i % 2 == 0 {
                i.to_string()
            } else {
                "-".to_string()
            };
            assert_eq!(
                lines[i + 1],
                format!(
                    "exit {seq} t={t} sid={} local={} reason={} exit_code={code} by={by}",
                    sids[i],
                    i + 10,
                    reason.as_str()
                ),
                "row for {reason:?}"
            );
        }
    }

    #[test]
    fn exits_n_formats_only_n_rows() {
        use crate::work_counts::take_ring_rows_formatted;

        let store = new_store();
        {
            let mut g = store.write().unwrap_or_else(|p| p.into_inner());
            for local in 1..=8 {
                g.register(test_handle(local));
                if local % 2 == 0 {
                    g.note_exit_code(local - 1, Some(i32::try_from(local).unwrap()));
                    g.deregister_local_as(local - 1, ExitReason::UiClose, ExitActor::Human);
                }
            }
        }
        let oracle = |since: u64, n: usize| -> String {
            let g = store.read().unwrap_or_else(|p| p.into_inner());
            let mut rows: Vec<String> = g
                .closed_logs()
                .exits_since(since)
                .map(|r| {
                    let exit_code = r
                        .exit_code
                        .map_or_else(|| "-".to_string(), |c| c.to_string());
                    format!(
                        "exit {} t={} sid={} local={} reason={} exit_code={exit_code} by={}",
                        r.seq,
                        r.t_ms,
                        r.sid.as_str(),
                        r.local_id,
                        r.reason.as_str(),
                        crate::control::pct_encode(r.actor.as_wire()),
                    )
                })
                .collect();
            if n > 0 && rows.len() > n {
                rows.drain(..rows.len() - n);
            }
            let mut out = format!("OK {}\n", rows.len());
            for row in rows {
                out.push_str(&row);
                out.push('\n');
            }
            out
        };
        let exited = 4;
        take_ring_rows_formatted();
        assert_eq!(cmd_exits(&store, ""), oracle(0, 0));
        assert_eq!(take_ring_rows_formatted(), exited, "negative control");
        for n in [1, 2, exited - 1, exited, exited + 1] {
            for since in [0, 3, 6, 12, u64::MAX] {
                let args = format!("{n} since={since}");
                let reply = cmd_exits(&store, &args);
                assert_eq!(reply, oracle(since, n), "{args}");
                assert_eq!(
                    take_ring_rows_formatted(),
                    reply.lines().count() - 1,
                    "{args}"
                );
            }
        }
    }

    #[test]
    fn window_cursor_and_usage() {
        let store = new_store();
        {
            let mut g = store.write().unwrap_or_else(|p| p.into_inner());
            for local in 1..=3 {
                g.register(test_handle(local));
                g.deregister_local_as(local, ExitReason::UiClose, ExitActor::Human);
            }
        }
        let all = cmd_exits(&store, "");
        let ids: Vec<u64> = all
            .lines()
            .skip(1)
            .map(|l| l.split(' ').nth(1).unwrap().parse().unwrap())
            .collect();
        assert_eq!(ids.len(), 3);
        let last = cmd_exits(&store, "1");
        assert!(last.starts_with("OK 1\n"), "{last:?}");
        assert!(last.contains(&format!("exit {} ", ids[2])), "{last:?}");
        let newer = cmd_exits(&store, &format!("since={}", ids[1]));
        assert!(newer.starts_with("OK 1\n"), "{newer:?}");
        assert!(newer.contains(&format!("exit {} ", ids[2])), "{newer:?}");
        assert_eq!(cmd_exits(&store, &format!("since={}", ids[2])), "OK 0\n");
        let both = cmd_exits(&store, &format!("5 since={}", ids[0]));
        assert!(both.starts_with("OK 2\n"), "{both:?}");
        assert_eq!(cmd_exits(&store, "3 1"), last);
        assert_eq!(
            cmd_exits(&store, &format!("since=0 since={}", ids[1])),
            newer
        );
        for bad in ["bogus", "since=x", "since=", "1 extra", "-1"] {
            assert_eq!(
                cmd_exits(&store, bad),
                "ERR usage: exits [<n>] [since=<id>]\n",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn caller_actor_is_the_edge_source_never_the_target_and_the_client_kind_for_owner() {
        let store = new_store();
        let caller = test_handle(1);
        let target = test_handle(2);
        let (caller_sid, target_sid) = (
            caller.sid.as_str().to_string(),
            target.sid.as_str().to_string(),
        );
        let tok = target
            .ctx
            .edges
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .grant(
                caller.sid.clone(),
                target.sid.clone(),
                Op::WriteInput,
                target.ctx.nonce,
            );
        let by = caller_actor(Scope::Edge(tok), &target.ctx);
        assert_eq!(by, ExitActor::Sid(caller_sid.clone()));
        assert_ne!(
            by.as_wire(),
            target_sid,
            "the closer is the token's source session, not the session being closed"
        );
        assert_eq!(
            caller_actor(Scope::Owner, &target.ctx),
            ExitActor::Ctl,
            "an owner-token connection carries no session identity, so the row \
             names the kind of client"
        );
        assert_eq!(ExitActor::Ctl.as_wire(), "ctl");
        assert_eq!(
            caller_actor(Scope::Bridge, &target.ctx),
            ExitActor::Bridge,
            "the bridge is a connection with no sid, named apart from a local client"
        );
        assert_eq!(ExitActor::Bridge.as_wire(), "bridge");
        assert_eq!(
            caller_actor(Scope::Edge(EdgeToken::generate()), &target.ctx),
            ExitActor::Unknown,
            "a token the target's table never issued names nobody"
        );
        assert_eq!(caller_actor_of(Scope::Owner, None), ExitActor::Ctl);
        assert_eq!(caller_actor_of(Scope::Bridge, None), ExitActor::Bridge);
        assert_eq!(caller_actor_of(Scope::Edge(tok), None), ExitActor::Unknown);
        assert_eq!(
            caller_actor_of(Scope::Edge(tok), Some(&target.ctx)),
            ExitActor::Sid(caller_sid.clone())
        );
        {
            let mut g = store.write().unwrap_or_else(|p| p.into_inner());
            g.register(caller);
            g.register(target);
            g.deregister_local_as(2, ExitReason::CtlClose, by);
        }
        let reply = cmd_exits(&store, "");
        let row = reply.lines().nth(1).expect("one exit row");
        assert!(row.contains(&format!(" sid={target_sid} ")), "{row}");
        assert!(
            row.ends_with(&format!(" reason=ctl-close exit_code=- by={caller_sid}")),
            "{row}"
        );
    }

    #[test]
    fn a_turns_by_is_the_callers_and_only_the_harness_hand_makes_it_harness() {
        let caller = test_handle(1);
        let target = test_handle(2);
        let caller_sid = caller.sid.as_str().to_string();
        let tok = target
            .ctx
            .edges
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .grant(
                caller.sid.clone(),
                target.sid.clone(),
                Op::WriteInput,
                target.ctx.nonce,
            );
        let by = |scope: Scope| {
            let driver = crate::control::turn_driver(scope, &target.ctx);
            let by = turn_by(scope, driver.as_ref());
            let named = match caller_actor(scope, &target.ctx) {
                ExitActor::Sid(sid) => TurnBy::Sid(sid),
                ExitActor::Ctl => TurnBy::Ctl,
                ExitActor::Bridge => TurnBy::Bridge,
                ExitActor::Human | ExitActor::Unknown => TurnBy::Unknown,
            };
            assert_eq!(by, named, "{scope:?}");
            by
        };
        assert_eq!(by(Scope::Owner), TurnBy::Ctl);
        assert_eq!(by(Scope::Bridge), TurnBy::Bridge);
        assert_eq!(by(Scope::Edge(tok)), TurnBy::Sid(caller_sid.clone()));
        assert_eq!(
            by(Scope::Edge(EdgeToken::generate())),
            TurnBy::Unknown,
            "a token the target's table never issued names nobody"
        );

        let drive = |holder: &str| crate::Lease::Drive {
            holder: holder.to_string(),
            expires_us: u64::MAX,
            conn: Some(1),
            hard: true,
            here: false,
            holder_pid: None,
        };
        let hand = drive(&crate::harness_host::holder());
        assert_eq!(typed_by(&TurnBy::Ctl, Some(&hand)), TurnBy::Harness);
        assert_eq!(TurnBy::Harness.as_wire(), "harness");
        for under in [
            None,
            Some(drive("aterm-harness@1")),
            Some(drive("manager")),
            Some(crate::Lease::Turn {
                id: 9,
                driver: None,
                typing: true,
            }),
        ] {
            assert_eq!(
                typed_by(&TurnBy::Ctl, under.as_ref()),
                TurnBy::Ctl,
                "{under:?}"
            );
        }
        for caller in [TurnBy::Sid(caller_sid), TurnBy::Bridge, TurnBy::Unknown] {
            assert_eq!(typed_by(&caller, Some(&hand)), caller, "{caller:?}");
        }
    }
}

#[cfg(test)]
mod log_actor_tests {
    use super::{cmd_grant, log_actor};
    use crate::control::{Principal, Scope, ServingPrincipal};
    use crate::session_store::test_handle;
    use aterm_session::{EdgeToken, Op, SessionId};
    use aterm_slog::Actor;

    #[test]
    fn log_actor_matrix() {
        let caller = test_handle(1);
        let target = test_handle(2);
        assert_eq!(log_actor(Scope::Owner, &target.ctx), Actor::Ctl);
        {
            let _harness = ServingPrincipal::enter(Principal::Harness);
            assert_eq!(log_actor(Scope::Owner, &target.ctx), Actor::Harness);
            assert_eq!(
                log_actor(Scope::Bridge, &target.ctx),
                Actor::Bridge,
                "the principal labels an Owner connection only"
            );
        }
        assert_eq!(log_actor(Scope::Owner, &target.ctx), Actor::Ctl, "restored");
        let grant = |src: &str| {
            target
                .ctx
                .edges
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .grant(
                    SessionId::new(src),
                    target.sid.clone(),
                    Op::WriteInput,
                    target.ctx.nonce,
                )
        };
        let minted = grant(caller.sid.as_str());
        assert_eq!(
            log_actor(Scope::Edge(minted), &target.ctx).word(),
            caller.sid.as_str()
        );
        for chosen in [
            "harness", "human", "ctl", "bridge", "aterm", "s-boss", "s-1",
        ] {
            let tok = grant(chosen);
            assert_eq!(
                log_actor(Scope::Edge(tok), &target.ctx),
                Actor::Nobody,
                "an edge granted as {chosen:?} is nobody"
            );
        }
        assert_eq!(
            log_actor(Scope::Edge(EdgeToken::generate()), &target.ctx),
            Actor::Nobody
        );
    }

    #[test]
    fn grant_refuses_a_reserved_source() {
        let target = test_handle(3);
        for word in aterm_slog::RESERVED_ACTOR_WORDS {
            let reply = cmd_grant(&target.ctx, Scope::Owner, &format!("{word} write-input"));
            assert!(
                reply.starts_with(&format!("ERR reserved: src={word} ")),
                "{word}: {reply}"
            );
        }
        let ok = cmd_grant(
            &target.ctx,
            Scope::Owner,
            "s-0123456789abcdef0123 write-input",
        );
        assert!(ok.starts_with("OK "), "{ok}");
        let store = crate::session_store::new_store();
        store.write().unwrap().register(target);
        let conn = std::sync::Arc::new(crate::connections::ConnectionTable::default());
        let reply = super::cmd_connect_in(
            &conn,
            &store,
            Scope::Owner,
            &format!("dst={} src=harness", "s-x"),
        );
        assert!(reply.starts_with("ERR reserved: src=harness "), "{reply}");
    }
}

#[cfg(test)]
mod update_anchor_tests {
    use super::{cmd_await, cmd_exits, cmd_timeline};
    use crate::session_store::{
        ExitActor, ExitReason, SessionHandoff, SessionStore, new_store, test_handle,
    };

    #[test]
    fn exits_ids_keep_rising_across_a_handoff() {
        let mut producer = SessionStore::default();
        for local in 1..=3 {
            producer.register(test_handle(local));
        }
        producer.deregister_local_as(3, ExitReason::ShellExit, ExitActor::Human);
        assert_eq!(producer.roster_seq(), 4);
        let manifest = SessionHandoff::from_store(&producer);
        let read = SessionHandoff::from_toml(&manifest.to_toml().unwrap()).unwrap();
        assert_eq!(read.roster_seq, Some(4));

        let successor = new_store();
        successor
            .write()
            .unwrap()
            .continue_roster_seq(read.roster_seq.unwrap());
        for rec in &read.sessions {
            successor
                .write()
                .unwrap()
                .register(test_handle(rec.local_id));
        }
        successor
            .write()
            .unwrap()
            .deregister_local_as(1, ExitReason::ShellExit, ExitActor::Human);
        let page = cmd_exits(&successor, "since=4");
        assert!(page.starts_with("OK 1\n"), "{page}");
        assert!(page.contains("reason=shell-exit"), "{page}");
        let old = SessionHandoff::from_toml(
            "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-old\"\nstate = \"alive\"\ntitle = \"zsh\"\n",
        )
        .expect("an old manifest reads");
        assert_eq!(old.roster_seq, None);
    }

    #[test]
    fn an_adopted_timeline_is_visible_to_a_pre_update_since_anchor() {
        let producer = test_handle(1);
        for i in 0..57 {
            producer
                .ctx
                .timeline
                .lock()
                .unwrap()
                .record_raw_meta_change(format!("n={i}"));
        }
        let producer_timeline = std::sync::Arc::clone(&producer.ctx.timeline);
        let mut store = SessionStore::default();
        store.register(producer);
        let manifest = SessionHandoff::from_store(&store);
        let read = SessionHandoff::from_toml(&manifest.to_toml().unwrap()).unwrap();
        let reserve = crate::session_timeline::HANDOFF_ID_RESERVE;
        assert_eq!(read.sessions[0].timeline_id, 58 + reserve);

        for i in 0..2 {
            producer_timeline
                .lock()
                .unwrap()
                .record_raw_meta_change(format!("overlap={i}"));
        }
        assert!(!producer_timeline.lock().unwrap().past_handoff_ceiling());

        let adopted = test_handle(1);
        crate::spawn::mark_handoff_gap(
            &adopted.ctx.timeline,
            &adopted.ctx.cast,
            &adopted.ctx.temporal,
            Some(7),
            read.sessions[0].timeline_id,
            crate::session_log::LogAdopt {
                ids: read.sessions[0]
                    .log_ids
                    .as_deref()
                    .and_then(crate::session_log::LogCarry::parse),
                seq: read.sessions[0].log_seq,
                lines: None,
                keeper_ep: None,
                mirror: None,
            },
            false,
        );
        adopted
            .ctx
            .timeline
            .lock()
            .unwrap()
            .record_raw_meta_change("after".to_string());
        let page = cmd_timeline(&adopted.ctx, "since=60");
        assert!(page.starts_with("OK 2\n"), "{page}");
        assert!(
            page.contains(&format!("event {} ", 59 + reserve)) && page.contains("kind=handoff"),
            "{page}"
        );
        {
            let mut tl = producer_timeline.lock().unwrap();
            for _ in 0..reserve {
                tl.record_raw_meta_change(String::new());
            }
            assert!(tl.past_handoff_ceiling());
        }
    }

    #[test]
    fn a_seq_anchor_from_another_process_answers_at_once() {
        let handle = test_handle(0);
        let store = new_store();
        handle.term.lock().unwrap().process(b"x");
        let current = handle.term.lock().unwrap().content_seq();
        assert_eq!(
            cmd_await(&handle.term, &store, 0, &handle.ctx, "seq 100000 timeout=0",),
            format!("OK seq {current}\n")
        );
        assert_eq!(
            cmd_await(
                &handle.term,
                &store,
                0,
                &handle.ctx,
                &format!("seq {current} timeout=0"),
            ),
            "OK timeout\n"
        );
    }

    #[test]
    fn await_frame_waits_on_a_half_frame_and_gone_reads_only_whole_ones() {
        const BEFORE: &[u8] =
            include_bytes!("../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308/before.bin");
        const HALF: &[u8] =
            include_bytes!("../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308/half.bin");
        const REST: &[u8] =
            include_bytes!("../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308/rest.bin");
        const END: &[u8] =
            include_bytes!("../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308/end.bin");
        let handle = test_handle(0);
        let store = new_store();
        let feed = |bytes: &[u8]| handle.term.lock().unwrap().process(bytes);
        let await_ = |rest: &str| cmd_await(&handle.term, &store, 0, &handle.ctx, rest);
        let text = || {
            super::super::control_query::cmd_text_json_read(
                &handle.term,
                &crate::frame_memo::FrameSlot::default(),
                super::super::control_query::TextArgs::default(),
                || (None, None, 0),
                None,
                None,
            )
        };
        const GONE: &str = "gone esc.to.interrupt rows 10 49 timeout=0";
        handle.term.lock().unwrap().resize(50, 144);
        feed(b"\x1b[47;3H");
        feed(BEFORE);
        assert!(await_("frame timeout=0").starts_with("OK frame "));
        assert_eq!(await_(GONE), "OK timeout\n", "busy before the split");
        assert!(!text().contains("\"frame\""), "whole: no frame field");

        feed(HALF);
        assert_eq!(
            await_("frame timeout=0"),
            "OK timeout\n",
            "the frame is open"
        );
        assert_eq!(await_(GONE), "OK timeout\n", "the half frame is not read");
        assert!(text().ends_with(",\"frame\":\"open\"}\n"), "{}", text());

        feed(REST);
        assert!(await_("frame timeout=0").starts_with("OK frame "));
        assert_eq!(await_(GONE), "OK timeout\n", "the whole frame is busy");
        assert!(!text().contains("\"frame\""));

        feed(END);
        assert!(await_(GONE).starts_with("OK gone "), "the reply's end");
    }

    #[test]
    fn await_frame_wakes_at_the_hang_detector_in_the_park() {
        use aterm_core::terminal::{ClockReading, FRAME_HANG};
        use std::time::{Duration, Instant};

        const BEFORE: &[u8] =
            include_bytes!("../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308/before.bin");
        const HALF: &[u8] =
            include_bytes!("../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308/half.bin");
        let handle = test_handle(0);
        let store = new_store();
        let text = || {
            super::super::control_query::cmd_text_json_read(
                &handle.term,
                &crate::frame_memo::FrameSlot::default(),
                super::super::control_query::TextArgs::default(),
                || (None, None, 0),
                None,
                None,
            )
        };
        {
            let mut t = handle.term.lock().unwrap();
            t.resize(50, 144);
            t.process(b"\x1b[47;3H");
            t.process(BEFORE);
            let begun = Instant::now()
                .checked_sub(FRAME_HANG - Duration::from_millis(300))
                .expect("an instant a minute back");
            t.process_at(
                HALF,
                ClockReading {
                    monotonic: begun,
                    wall_ms: None,
                },
            );
            assert!(t.sync_frame_unfinished());
        }
        assert!(text().ends_with(",\"frame\":\"open\"}\n"), "{}", text());
        let started = Instant::now();
        let out = cmd_await(&handle.term, &store, 0, &handle.ctx, "frame timeout=120000");
        assert!(out.starts_with("OK frame "), "{out}");
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "woken by the hang detector, not the timeout"
        );
        assert!(
            !text().contains("\"frame\""),
            "past its hang detector the frame reads as it stands"
        );
        assert!(handle.term.lock().unwrap().sync_frame_unfinished());
    }
}

#[cfg(test)]
mod connection_carry_tests {
    use super::{Scope, cmd_grant};
    use crate::session_store::{SessionHandoff, SessionStore, test_handle};

    #[test]
    fn the_carry_holds_each_triple_once() {
        let controller = test_handle(1);
        let target = test_handle(2);
        let (src, dst) = (controller.sid.clone(), target.sid.clone());
        for _ in 0..1_000 {
            assert!(
                cmd_grant(&target.ctx, Scope::Owner, &format!("{src} read-screen"))
                    .starts_with("OK ")
            );
            assert!(
                cmd_grant(&target.ctx, Scope::Owner, "s-controller read-screen").starts_with("OK ")
            );
        }
        for op in ["signal", "write-input"] {
            assert!(
                cmd_grant(&target.ctx, Scope::Owner, &format!("{src} {op}")).starts_with("OK ")
            );
        }
        assert!(cmd_grant(&target.ctx, Scope::Owner, &format!("{dst} signal")).starts_with("OK "));
        let mut store = SessionStore::default();
        store.register(controller);
        store.register(target);

        let carried = SessionHandoff::from_store(&store).connections;
        let triples = carried
            .iter()
            .map(|row| (row.src.as_str(), row.dst.as_str(), row.op.as_str()))
            .collect::<Vec<_>>();
        let mut expected = vec![
            (src.as_str(), dst.as_str(), "read-screen"),
            (src.as_str(), dst.as_str(), "signal"),
            (src.as_str(), dst.as_str(), "write-input"),
            ("s-controller", dst.as_str(), "read-screen"),
            (dst.as_str(), dst.as_str(), "signal"),
        ];
        expected.sort_unstable();
        assert_eq!(
            triples, expected,
            "each distinct triple, once, in `(src, dst, op)` order"
        );
    }
}

#[cfg(test)]
mod update_park_wait_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::{OnHangup, Park, Parked, Stretch, TurnIo, cmd_await, cmd_ready, cmd_turn};
    use crate::session_store::{new_store, test_handle};

    #[test]
    fn idle_waits_do_not_latch_on_a_parked_readers_silence() {
        let handle = test_handle(0);
        let store = new_store();
        handle.term.lock().unwrap().process(b"tick\r\n");
        handle.ctx.update_parked.park_for_test();
        assert_eq!(
            cmd_await(&handle.term, &store, 0, &handle.ctx, "idle 60 timeout=400",),
            "OK timeout\n"
        );
        assert_eq!(
            cmd_ready(&handle.term, &store, 0, &handle.ctx, "400"),
            "OK timeout\n"
        );
        const IDLE: Duration = Duration::from_millis(200);
        let resumer = {
            let ctx = handle.ctx.clone();
            let term = handle.term.clone();
            std::thread::spawn(move || {
                let first = aterm_spec::wait::until_within(
                    aterm_spec::wait::MUST_HAPPEN,
                    "the await arms its idle window",
                    || term.lock().unwrap().watch_next_deadline(),
                );
                aterm_spec::wait::until_within(
                    aterm_spec::wait::MUST_HAPPEN,
                    "the await holds its idle window across the park",
                    || {
                        let t = term.lock().unwrap();
                        match t.watch_next_deadline() {
                            None => Some(None),
                            Some(at) if at >= first + IDLE && at > Instant::now() => {
                                ctx.update_parked.resume_for_test();
                                Some(Some(at))
                            }
                            Some(_) => None,
                        }
                    },
                )
            })
        };
        let reply = cmd_await(
            &handle.term,
            &store,
            0,
            &handle.ctx,
            &format!("idle {} timeout=60000", IDLE.as_millis()),
        );
        let latched = Instant::now();
        let held = resumer.join().unwrap().unwrap_or_else(|| {
            panic!("the await answered {reply:?} while parked, never holding its window open")
        });
        assert!(reply.starts_with("OK idle "), "{reply}");
        assert!(
            latched >= held,
            "a whole window after the last parked look: latched {:?} before it",
            held.saturating_duration_since(latched)
        );
        assert!(
            cmd_await(
                &handle.term,
                &store,
                0,
                &handle.ctx,
                "idle 60 timeout=60000",
            )
            .starts_with("OK idle ")
        );
    }

    #[test]
    fn ready_answers_no_prompt_from_a_parked_readers_parser() {
        let handle = test_handle(0);
        let store = new_store();
        handle.term.lock().unwrap().process(b"\x1b]133;A\x07$ ");
        handle.ctx.update_parked.park_for_test();
        assert_eq!(
            cmd_ready(&handle.term, &store, 0, &handle.ctx, "400"),
            "OK timeout\n",
            "a parked reader's parser says nothing about the shell"
        );
        let resumer = {
            let ctx = handle.ctx.clone();
            let term = handle.term.clone();
            std::thread::spawn(move || {
                aterm_spec::wait::until_within(
                    aterm_spec::wait::MUST_HAPPEN,
                    "the wait arms its settle window",
                    || term.lock().unwrap().watch_next_deadline(),
                );
                term.lock()
                    .unwrap()
                    .process(b"\x1b]133;B\x07make\r\n\x1b]133;C\x07building...");
                ctx.update_parked.resume_for_test();
            })
        };
        let reply = cmd_ready(&handle.term, &store, 0, &handle.ctx, "60000");
        resumer.join().unwrap();
        assert_eq!(
            reply, "OK ready idle\n",
            "the caught-up parser shows `make` running, not the prompt it left"
        );
        handle
            .term
            .lock()
            .unwrap()
            .process(b"\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ");
        assert_eq!(
            cmd_ready(&handle.term, &store, 0, &handle.ctx, "60000"),
            "OK ready prompt\n"
        );
    }

    #[test]
    fn a_turn_across_an_update_park_presses_submit_once() {
        let run = |parked: bool, timeout_ms: u64| -> usize {
            let handle = test_handle(0);
            let store = new_store();
            if parked {
                handle.ctx.update_parked.park_for_test();
            }
            let presses = AtomicUsize::new(0);
            let paste = |_: &str| true;
            let press = |_: &str| {
                presses.fetch_add(1, Ordering::SeqCst);
                true
            };
            let _ = cmd_turn(
                &handle.term,
                &store,
                0,
                &format!("idle=50 timeout={timeout_ms} submit_window=150 presses=3 hello"),
                &handle.ctx,
                &TurnIo {
                    paste: &paste,
                    press: &press,
                    ..TurnIo::paste_only()
                },
            );
            presses.load(Ordering::SeqCst)
        };
        assert_eq!(run(true, 1200), 1, "one Enter into a parked session");
        assert_eq!(
            run(false, aterm_spec::wait::MUST_HAPPEN.as_millis() as u64),
            3,
            "a swallowed press is re-pressed unparked"
        );
    }

    #[test]
    fn a_park_that_begins_mid_slice_is_noticed_within_one_slice() {
        use std::sync::atomic::AtomicBool;

        static SLICES: AtomicUsize = AtomicUsize::new(0);
        fn slice(left: Duration) -> Duration {
            SLICES.fetch_add(1, Ordering::SeqCst);
            left.min(Duration::from_millis(10))
        }
        fn there() -> bool {
            false
        }
        let lane = crate::park::Lane {
            slice,
            hung_up: there,
        };
        let handle = test_handle(0);
        let sub = crate::session_watch::Subscription::single(&handle.ctx.watchers);
        let alive = || false;
        let mut park = Park::new(
            &sub,
            &alive,
            OnHangup::Abort,
            Stretch::Hold(handle.ctx.update_parked.as_ref()),
        );
        park.lane = lane;

        let entered = AtomicBool::new(false);
        let (seen, at_flip) = std::thread::scope(|scope| {
            let flipper = scope.spawn(|| {
                aterm_spec::wait::until_within(
                    aterm_spec::wait::MUST_HAPPEN,
                    "the park's entry look",
                    || entered.load(Ordering::SeqCst).then_some(()),
                );
                handle.ctx.update_parked.park_for_test();
                SLICES.load(Ordering::SeqCst)
            });
            let ended = park.run(
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                |_, parked, _| {
                    entered.store(true, Ordering::SeqCst);
                    (parked.then(|| SLICES.load(Ordering::SeqCst)), None)
                },
                || {},
            );
            (ended, flipper.join().unwrap())
        });
        let Parked::Done(seen_at) = seen else {
            panic!("the park closed without seeing the flip");
        };
        assert!(
            seen_at <= at_flip + 1,
            "the flip was seen {} slices after it",
            seen_at - at_flip
        );

        handle.ctx.update_parked.resume_for_test();
        let mut looks = 0;
        let ended = park.run(
            Instant::now() + Duration::from_millis(100),
            |_, parked, _| {
                looks += 1;
                (parked.then_some(()), None)
            },
            || {},
        );
        assert!(matches!(ended, Parked::Deadline));
        assert_eq!(
            looks,
            2,
            "entry and close, over {} slices",
            SLICES.load(Ordering::SeqCst)
        );
    }
}

#[cfg(test)]
mod park_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::{OnHangup, Park, Parked, Stretch};
    use crate::park::Lane;
    use crate::session_watch::{Subscription, Watchers};

    static SLICES: AtomicUsize = AtomicUsize::new(0);

    fn slice(left: Duration) -> Duration {
        SLICES.fetch_add(1, Ordering::SeqCst);
        left.min(Duration::from_millis(2))
    }

    fn there() -> bool {
        false
    }

    const LANE: Lane = Lane {
        slice,
        hung_up: there,
    };

    fn old_park(sub: &Subscription, until: Instant, mut look: impl FnMut() -> bool) -> usize {
        let mut looks = 0;
        loop {
            looks += 1;
            if look() || Instant::now() >= until {
                return looks;
            }
            let left = until
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1));
            let _ = sub.wait((LANE.slice)(left));
        }
    }

    #[test]
    fn a_hangup_slice_does_not_rerun_the_predicate() {
        let watchers = std::sync::Arc::<Watchers>::default();
        let sub = Subscription::single(&watchers);
        let alive = || false;
        let mut park = Park::new(&sub, &alive, OnHangup::Abort, Stretch::None);
        park.lane = LANE;

        SLICES.store(0, Ordering::SeqCst);
        let mut looks = 0;
        let ended = park.run(
            Instant::now() + Duration::from_millis(100),
            |_, _, _| {
                looks += 1;
                (None::<()>, None)
            },
            || {},
        );
        assert!(matches!(ended, Parked::Deadline));
        let slices = SLICES.load(Ordering::SeqCst);
        assert_eq!(looks, 2, "entry and close, over {slices} slices");

        SLICES.store(0, Ordering::SeqCst);
        let looks = old_park(&sub, Instant::now() + Duration::from_millis(100), || false);
        let slices = SLICES.load(Ordering::SeqCst);
        assert_eq!(looks, slices + 1, "the old loop looked after every slice");
    }
}

#[cfg(test)]
mod turn_halt_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{TurnIo, cmd_turn};
    use crate::session_store::{new_store, test_handle};

    struct Ran {
        reply: String,
        writes: usize,
        presses: usize,
    }

    fn run(line: &str, take_after: Option<usize>, remote: bool) -> Ran {
        let handle = test_handle(0);
        let (term, ctx) = (handle.term.clone(), handle.ctx.clone());
        let sid = handle.sid.as_str().to_owned();
        let store = new_store();
        store.write().unwrap().register(handle);
        let _driving = remote.then(|| {
            crate::fabric::DrivingAs::enter(crate::fabric::Driver {
                node: "0b".repeat(16),
                host: "mac-0b".to_owned(),
            })
        });
        if remote {
            assert_eq!(
                crate::fabric::cmd_hold(
                    &store,
                    &format!("{sid} on"),
                    crate::fabric::HoldIssuer::Remote
                ),
                "OK hold=1\n"
            );
        }
        let take_over = || crate::fabric::person_hold(&*ctx, true, &|| {});
        if take_after == Some(0) {
            take_over();
        }
        let (writes, presses) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let wrote = || {
            if Some(writes.fetch_add(1, Ordering::SeqCst) + 1) == take_after {
                take_over();
            }
            true
        };
        let paste = |_: &str| wrote();
        let key = |_: crate::input::InputEvent| wrote();
        let press = |_: &str| {
            presses.fetch_add(1, Ordering::SeqCst);
            true
        };
        let reply = cmd_turn(
            &term,
            &store,
            0,
            line,
            &ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
                key: &key,
                ..TurnIo::paste_only()
            },
        );
        Ran {
            reply,
            writes: writes.load(Ordering::SeqCst),
            presses: presses.load(Ordering::SeqCst),
        }
    }

    #[test]
    fn a_hold_set_while_a_turn_runs_presses_nothing_more() {
        const TYPED: &str = "typed=1 cadence=1 cap=5 idle=1 presses=1 submit_window=50 \
                             timeout=60000 hello";
        const PLAIN: &str = "idle=1 presses=1 submit_window=50 timeout=60000 hello";
        for remote in [false, true] {
            let cut = run(TYPED, Some(2), remote);
            assert!(
                cut.reply
                    .starts_with("OK 0 turn skipped reason=halted typed=2 pasted=0 submitted=0 "),
                "{}",
                cut.reply
            );
            assert_eq!((cut.writes, cut.presses), (2, 0), "remote={remote}");
            let whole = run(TYPED, None, remote);
            assert!(!whole.reply.contains("reason=halted"), "{}", whole.reply);
            assert_eq!((whole.writes, whole.presses), (5, 1), "remote={remote}");

            let cut = run(PLAIN, Some(1), remote);
            assert!(
                cut.reply
                    .starts_with("OK 0 turn skipped reason=halted submitted=0 "),
                "{}",
                cut.reply
            );
            assert_eq!((cut.writes, cut.presses), (1, 0), "remote={remote}");
            let whole = run(PLAIN, None, remote);
            assert_eq!((whole.writes, whole.presses), (1, 1), "remote={remote}");

            for line in [TYPED, PLAIN] {
                let before = run(line, Some(0), remote);
                assert!(
                    before.reply.starts_with("ERR halted ")
                        && before.reply.contains("origin=person"),
                    "{}",
                    before.reply
                );
                assert_eq!((before.writes, before.presses), (0, 0), "remote={remote}");
            }
        }
    }
}

#[cfg(test)]
mod composer_verify_tests {
    use aterm_core::terminal::Terminal;

    use super::{
        ComposerAfterPress, ComposerKey, composer_after_press, composer_key_once_shown,
        composer_verify_key,
    };

    fn screen(caret_row: &str, col: u16) -> Terminal {
        let rule = "\u{2500}".repeat(80);
        let mut t = Terminal::new(24, 80);
        t.process(
            format!(
                "\x1b[20;1H{rule}\x1b[21;1H{caret_row}\x1b[22;1H{rule}\
                 \x1b[23;1H  \u{23f5}\u{23f5} bypass permissions on (shift+tab to cycle)\
                 \x1b[21;{col}H"
            )
            .as_bytes(),
        );
        t
    }

    #[test]
    fn the_key_is_the_typed_text_the_composer_shows() {
        let typed = screen("\u{276f} edit the README", 18);
        assert_eq!(
            composer_verify_key(&typed, "edit the  README"),
            ComposerKey::Shows("edittheREADME".to_string())
        );
        let empty = screen("\u{276f} ", 3);
        assert_eq!(composer_verify_key(&empty, "edit"), ComposerKey::NotYet);
        let chip = screen("\u{276f} [Pasted text #1 +12 lines] ", 30);
        assert_eq!(
            composer_verify_key(&chip, "a\nlong\npaste"),
            ComposerKey::No
        );
        assert_eq!(composer_verify_key(&typed, " \n"), ComposerKey::No);
        let mut boxed = Terminal::new(24, 80);
        boxed.process(" Select model\r\n \u{276f} 1. Default\r\n   2. Opus".as_bytes());
        assert_eq!(composer_verify_key(&boxed, "edit"), ComposerKey::No);
        let placeholder = screen("\u{276f} Press up to edit queued messages", 3);
        assert_eq!(
            composer_verify_key(&placeholder, "edit"),
            ComposerKey::NotYet
        );
        let drafted = screen("\u{276f} Press up to edit queued messages", 35);
        assert_eq!(
            composer_verify_key(&drafted, "edit"),
            ComposerKey::Shows("edit".to_string())
        );
    }

    #[test]
    fn after_the_enter_the_composer_has_left_holds_or_is_gone() {
        assert_eq!(
            composer_after_press(&screen("\u{276f} ", 3), "edit"),
            ComposerAfterPress::Left
        );
        assert_eq!(
            composer_after_press(
                &screen("\u{276f} Press up to edit queued messages", 3),
                "edit"
            ),
            ComposerAfterPress::Left
        );
        assert_eq!(
            composer_after_press(&screen("\u{276f} edit", 7), "edit"),
            ComposerAfterPress::Holds
        );
        let mut boxed = Terminal::new(24, 80);
        boxed.process(" Select model\r\n \u{276f} 1. Default".as_bytes());
        assert_eq!(
            composer_after_press(&boxed, "edit"),
            ComposerAfterPress::Gone
        );
        let mut torn = screen("\u{276f} edit", 7);
        torn.process(b"\x1b[?2026h\x1b[21;1H\x1b[2K");
        assert_eq!(
            composer_after_press(&torn, "edit"),
            ComposerAfterPress::Unfinished
        );
    }

    #[test]
    fn a_late_echo_is_waited_for_within_the_cap() {
        let script = |looks: Vec<ComposerKey>, moves: Vec<Result<bool, String>>| {
            let mut looks = looks.into_iter().enumerate();
            let mut moves = moves.into_iter();
            let mut waited = Vec::new();
            let got = composer_key_once_shown(
                || {
                    let (seq, seen) = looks.next().expect("a look");
                    (seen, 10 + seq as u64)
                },
                |after| {
                    waited.push(after);
                    moves.next().expect("a wait")
                },
            );
            (got, waited)
        };
        let shows = || ComposerKey::Shows("edit".to_string());
        assert_eq!(
            script(
                vec![ComposerKey::NotYet, ComposerKey::NotYet, shows()],
                vec![Ok(true), Ok(true)]
            ),
            (Ok(Some("edit".to_string())), vec![10, 11])
        );
        assert_eq!(
            script(
                vec![ComposerKey::NotYet, ComposerKey::NotYet],
                vec![Ok(true), Ok(false)]
            ),
            (Ok(None), vec![10, 11]),
            "the cap ends first"
        );
        assert_eq!(
            script(vec![ComposerKey::NotYet, ComposerKey::No], vec![Ok(true)]),
            (Ok(None), vec![10])
        );
        assert_eq!(
            script(
                vec![ComposerKey::NotYet],
                vec![Err("ERR exited\n".to_string())]
            ),
            (Err("ERR exited\n".to_string()), vec![10])
        );
        assert_eq!(
            script(vec![shows()], vec![]),
            (Ok(Some("edit".to_string())), vec![])
        );
    }
}
