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

/// `sessions` -> list the process-wide registry: `OK <n>\n` then one line per session, sorted by local id: `<local> <sid> <parent|-> <state> <title> meta=<1|0> nonce=<hex32> window=<id|none|-> active=<1|0|-> wfocus=<1|0|-> detail=<pct|-> path=<frozen|live> program=<name|-> agent=<word|-> agent_detail=<pct|-> agent_rev=<n> agent_since_ms=<ms> agent_gen=<e.s|-> agent_fp=<hex16|-> wait=<memory|-> wait_gib=<GiB|-> wait_ahead=<n|-> program_state=<word[:kind]|-> … supervisor=<holder|-> supervisor_proven=<0|1> autopilot=<off|cruise|land|-> path_evidence=<measured:<ms>|unconfirmed:<ms>|carried|-> copy=<managed|foreign|-> upgrade=<-|<state>/<to>/<why>/<age>> land_at=<unix s|-> task=<open|landed|theirs|-> pause=<unix s|-> task_state=<working|done|exited|stalled|-> task_rev=<n>`.
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

    let (rows, roster) = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        (g.bridge_roster(), g.roster_seq())
    };
    let mut out = format!("OK {} roster={roster}\n", rows.len());
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
        Ok(t) => crate::session_status::executing_detail(&t, &h.ctx.ran),
        Err(std::sync::TryLockError::Poisoned(p)) => {
            crate::session_status::executing_detail(&p.into_inner(), &h.ctx.ran)
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
            (claim_version, claim_vendor),
            authority,
        ) = {
            let meta = h.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            let opt = |v: Option<&str>| v.map_or_else(|| "-".to_string(), pct_encode);
            let now = crate::metrics::now_us();
            let live = meta.live_supervisor(now);
            (
                u8::from(meta.any_set()),
                opt(live),
                opt(meta.role.as_deref()),
                opt(meta.row_attention()),
                opt(meta.user_title.as_deref()),
                meta.autopilot_word(),
                meta.land_at,
                (
                    live == Some(crate::autopilot::own_holder()),
                    meta.supervisor_proven(now),
                    meta.claim_proven(now),
                ),
                crate::harness_claim::wire_versions(meta.live_scoped(now)),
                meta.supervisor_authority(now).wire_fields(),
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
                format!(
                    "{}{}",
                    timeline.agent().wire_fields(),
                    crate::session_status::program::BuildWait::fields(timeline.build_wait())
                ),
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
             {authority} supervisor={supervisor} supervisor_proven={proven} \
             supervisor_version={claim_version} supervisor_vendor={claim_vendor} \
             autopilot={autopilot} {owner} \
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
            match (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                .then(|| v.parse::<u64>().ok())
                .flatten()
            {
                Some(n) if n >= 1 => ttl_ms = n.min(LEASE_TTL_MAX_MS),
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

/// `await <idle <ms> | seq [<n>] | match <re> [rows <a> <b>] | gone <re> [rows <a> <b>]`; `idle`,
/// `seq`, `gone` and `agent` also answer `OK autopilot <word>` for an `autopilot=` the stored word
/// is not ([`take_autopilot`], [`autopilot_moved`]), at the arm or while parked.
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
                         momentum <floor 0..=1>; each takes [timeout=<ms>], and idle, seq, gone, \
                         agent and log [autopilot=<cruise|land|off|->]\n";

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
    let autopilot = match kind {
        "seq" => take_autopilot(&mut args, 1),
        "idle" | "gone" | "agent" | "log" => take_autopilot(&mut args, 2),
        _ => Ok(None),
    };
    let Ok(autopilot) = autopilot else {
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
                if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                    return USAGE.to_string();
                }
                let Ok(rev) = v.parse::<u64>() else {
                    return USAGE.to_string();
                };
                since = Some(rev);
            }
        }
        return await_agent(store, session, ctx, &words, since, autopilot, timeout_ms);
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
            Ok(wait) => await_log(store, session, ctx, wait, autopilot, timeout_ms),
            Err(usage) => usage.to_string(),
        };
    }

    let now0 = Instant::now();
    let armed = if kind == "match" || kind == "gone" {
        let Some(pat) = args.get(1) else {
            return USAGE.to_string();
        };
        let range = match (args.get(2), args.get(3), args.get(4)) {
            (Some(&"rows"), Some(a), Some(b)) => {
                if a.is_empty()
                    || b.is_empty()
                    || !a.bytes().all(|b| b.is_ascii_digit())
                    || !b.bytes().all(|b| b.is_ascii_digit())
                {
                    return USAGE.to_string();
                }
                match (a.parse::<usize>(), b.parse::<usize>()) {
                    (Ok(start), Ok(end)) => RowRange::Span { start, end },
                    _ => return USAGE.to_string(),
                }
            }
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
        let uint = |s: &&str| {
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse::<u64>().ok())
                .flatten()
        };
        match kind {
            "idle" => {
                let Some(ms) = args.get(1).and_then(uint) else {
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
                let after = args.get(1).and_then(uint).unwrap_or(current);
                if after > current {
                    drop(t);
                    return autopilot_moved(ctx, autopilot)
                        .unwrap_or_else(|| format!("OK seq {current}\n"));
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
    let mut latch = watcher_look(term, id);
    let look = |now: Instant, parked: bool, closing: bool| {
        if let Some(moved) = autopilot_moved(ctx, autopilot) {
            term_lock(term).watch_disarm(id);
            return (Some(moved), None);
        }
        let (seq, next) = latch(now, parked, closing);
        (seq.map(|seq| format!("OK {kind} {seq}\n")), next)
    };
    match park.run(overall, look, disarm_watch(term, id)) {
        Parked::Done(reply) => reply,
        Parked::Deadline => "OK timeout\n".to_string(),
        Parked::Exited => "ERR exited\n".to_string(),
        Parked::HungUp => crate::control::HUNG_UP_REPLY.to_string(),
    }
}

/// Take `idle`'s, `seq`'s, `gone`'s, `agent`'s or `log`'s opt-in `autopilot=<cruise|land|off|->`
/// out of `args[from..]` (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.10, slice 10a): the
/// word the caller last read from `meta`'s `autopilot=`, `-` for none stored. Only past the form's
/// own arguments, so a `gone` pattern that reads `autopilot=…` keeps its meaning. `Ok(None)`
/// without one; `Err` for a word `meta` never prints (a synonym included) or a second token.
fn take_autopilot<'a>(args: &mut Vec<&'a str>, from: usize) -> Result<Option<&'a str>, ()> {
    use aterm_agent::supervise::Autopilot;
    let tail = args.split_off(from.min(args.len()));
    let mut read = None;
    for arg in tail {
        match arg.strip_prefix("autopilot=") {
            None => args.push(arg),
            Some(word)
                if read.is_none()
                    && (word == "-" || Autopilot::ALL.iter().any(|m| m.word() == word)) =>
            {
                read = Some(word);
            }
            Some(_) => return Err(()),
        }
    }
    Ok(read)
}

/// The answer of a wait that carries `autopilot=<read>` ([`take_autopilot`]) once the session's
/// stored word is not `read`: `OK autopilot <stored>`, the word as `meta` prints it. A write of
/// the word records a `meta-change`, which rings the session's watchers, so a parked wait looks
/// again; one without the token (`None`) takes no lock and never answers here. It takes the meta
/// lock alone: its caller holds neither the timeline's nor the terminal's.
fn autopilot_moved(ctx: &SessionCtx, read: Option<&str>) -> Option<String> {
    let read = read?;
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let stored = meta.autopilot.as_deref().unwrap_or("-");
    (stored != read).then(|| format!("OK autopilot {stored}\n"))
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
    autopilot: Option<&str>,
    timeout_ms: u64,
) -> String {
    use std::time::{Duration, Instant};

    let overall = Instant::now() + Duration::from_millis(timeout_ms);
    let sub = crate::session_watch::Subscription::single(&ctx.watchers);
    let exited = exit_probe(store, session);
    let in_the_look = || false;
    let park = Park::new(&sub, &in_the_look, OnHangup::Abort, Stretch::None);
    let look = |_: Instant, _: bool, _: bool| {
        // Outside the timeline's lock (meta → timeline); an exit answers first, as `Park` checks
        // one ahead of the other forms' looks.
        if let Some(moved) = autopilot_moved(ctx, autopilot).filter(|_| !exited(store)) {
            return (Some(moved), None);
        }
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
#[derive(Clone, Copy)]
pub(crate) struct TaskTarget<'a> {
    pub(crate) local_id: u64,
    pub(crate) ctx: &'a SessionCtx,
    pub(crate) since: Option<u64>,
    pub(crate) facts: bool,
    pub(crate) fact_since: Option<u64>,
}

/// One target of an `await task` ([`await_tasks`]): a live session's task ([`TaskTarget`]), or the
/// row of a worker whose tab aterm closed once its task concluded ([`crate::retired_workers`]),
/// which has its news at once: its end, `state=closed`, as a wait armed before the close reads it.
pub(crate) enum Awaited<'a> {
    Live(TaskTarget<'a>),
    Retired(String),
}

/// `await task` on live sessions alone ([`await_tasks`]).
pub(crate) fn await_task(
    store: &Store,
    targets: &[TaskTarget<'_>],
    combine: crate::task_record::Combine,
    timeout_ms: u64,
) -> String {
    let awaited: Vec<Awaited<'_>> = targets.iter().copied().map(Awaited::Live).collect();
    await_tasks(store, &awaited, combine, timeout_ms)
}

/// `await task`: park until the task records of `awaited` have news — a target whose record's rev is past its `since` with a state to read (`done`, `exited`, `stalled`) or an ask raised under a task owner (one beside a standing ask too), never a `kind=wait` entry ([`crate::task_record::TaskRecord::has_news`]), a `facts=1` target whose fact word was noted past its fact edge (a clear is none), whose session closed, or a retired worker's row ([`Awaited::Retired`]), news from the start — at the first such target (`any`) or once every target is one (`all`). A fact edit never moves `rev`, so a wait without `facts=1` never wakes on one. Rows answer in `awaited`'s order.
pub(crate) fn await_tasks(
    store: &Store,
    awaited: &[Awaited<'_>],
    combine: crate::task_record::Combine,
    timeout_ms: u64,
) -> String {
    use std::time::{Duration, Instant};

    use crate::task_record::Combine;

    let overall = Instant::now() + Duration::from_millis(timeout_ms);
    let targets: Vec<TaskTarget<'_>> = awaited
        .iter()
        .filter_map(|target| match target {
            Awaited::Live(target) => Some(*target),
            Awaited::Retired(_) => None,
        })
        .collect();
    let retired = awaited.len() - targets.len();
    let mut sub = crate::session_watch::Subscription::new();
    for target in &targets {
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
        latch_task_news(store, &targets, &edges, &exits, &mut latched);
        let with_news = retired + latched.iter().filter(|latch| latch.is_some()).count();
        let answered = match combine {
            Combine::Any => with_news > 0,
            Combine::All => with_news == awaited.len(),
        };
        (answered.then_some(()), None)
    };
    let tail = match park.run(overall, look, || {}) {
        Parked::Done(()) => "",
        Parked::Deadline | Parked::Exited => " status=timeout",
        Parked::HungUp => return crate::control::HUNG_UP_REPLY.to_string(),
    };
    let mut live = targets
        .iter()
        .zip(&exits)
        .zip(latched)
        .map(|((target, exited), latch)| {
            latch.unwrap_or_else(|| {
                task_wait_row(
                    target,
                    &crate::task_record::task_reading(target.ctx),
                    exited(store),
                    false,
                )
            })
        });
    let mut out = format!("OK {}{tail}\n", awaited.len());
    for target in awaited {
        // `live` holds one row per `Awaited::Live`, in order.
        let row = match target {
            Awaited::Live(_) => live.next().unwrap_or_default(),
            Awaited::Retired(row) => row.clone(),
        };
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
    reading: &crate::task_record::TaskReading,
    closed: bool,
    news: bool,
) -> String {
    let mut row = crate::task_record::await_row(
        target.ctx.self_id.as_str(),
        reading.record.as_ref(),
        reading.agent,
        &reading.program,
        &reading.attention,
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
        let reading = crate::task_record::task_reading(target.ctx);
        let (rev_edge, fact_edge) = *edge;
        let news = reading.record.as_ref().is_some_and(|task| {
            (task.rev > rev_edge && task.has_news())
                || fact_edge.is_some_and(|edge| task.fact_rev > edge && task.fact.is_some())
        });
        if closed || news {
            *latch = Some(task_wait_row(target, &reading, closed, true));
        }
    }
}

fn await_log(
    store: &Store,
    session: u64,
    ctx: &SessionCtx,
    wait: crate::session_log::LogAwait,
    autopilot: Option<&str>,
    timeout_ms: u64,
) -> String {
    await_log_seamed(store, session, ctx, wait, autopilot, timeout_ms, || {})
}

/// [`await_log`], running `after_look` the instant after each look it takes at an ended session and before it decides (production passes nothing): the seam a test lands a retire on, to pin that the look is the waiter's only observation.
///
/// `autopilot` is the word the wait carries ([`take_autopilot`]): a live session whose stored word
/// is another answers `OK autopilot <stored>` ahead of the log's own answer, at the arm or on the
/// write that moves it, as the screen's waits do; an ended session's log answers as before.
pub(crate) fn await_log_seamed(
    store: &Store,
    session: u64,
    ctx: &SessionCtx,
    wait: crate::session_log::LogAwait,
    autopilot: Option<&str>,
    timeout_ms: u64,
    mut after_look: impl FnMut(),
) -> String {
    use std::time::{Duration, Instant};

    use crate::session_log::LogAwaitStep;

    let overall = Instant::now() + Duration::from_millis(timeout_ms);
    let sub = crate::session_watch::Subscription::single(&ctx.watchers);
    let exited = exit_probe(store, session);
    let moved = || autopilot_moved(ctx, autopilot).filter(|_| !exited(store));
    if let Some(reply) = moved() {
        return reply;
    }
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
        if let Some(reply) = moved() {
            return (Some(reply), None);
        }
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

#[cfg(not(test))]
static NEXT_TURN_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn turn_id_count() -> &'static std::sync::atomic::AtomicU64 {
    #[cfg(test)]
    {
        OWN_TURN_IDS.with(|cell| {
            if let Some(own) = cell.get() {
                own
            } else {
                let fresh: &'static std::sync::atomic::AtomicU64 =
                    Box::leak(Box::new(std::sync::atomic::AtomicU64::new(0)));
                cell.set(Some(fresh));
                fresh
            }
        })
    }
    #[cfg(not(test))]
    {
        &NEXT_TURN_ID
    }
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
    handoff_ceiling_for(turn_ids_minted())
}

#[cfg(any(unix, test))]
const fn handoff_ceiling_for(minted: u64) -> u64 {
    let sum = minted.saturating_add(TURN_ID_RESERVE);
    if sum < crate::handoff_carry::MAX_TURN_ID {
        sum
    } else {
        crate::handoff_carry::MAX_TURN_ID
    }
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

/// The typed-prefix cap of `turn typed=1`: at most this many graphemes are pressed one key at a time, the remainder going as the rest's writes ([`rest_writes`]: one write of its keys' bytes where it holds no Tab or line break). 240 at the default cadence is ~17 s of typing, inside the default 240 s deadline with room for the reply; a turn whose keys go slower than its deadline allows writes the rest sooner ([`TypingPace`]).
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

/// `turn typed=1`'s press list for `text`, one event per grapheme ([`typed_key_event`]), pure so the tuple a typed turn presses is pinned beside the tuple a keyboard presses without an event loop. Each event carries the byte offset of its grapheme in `text`: where the writes of the rest start when the turn stops pressing before that key ([`TypingPace`]).
pub(crate) fn typed_key_events(text: &str) -> Vec<(usize, InputEvent)> {
    use aterm_grapheme::GraphemeClusters;
    text.grapheme_indices()
        .filter_map(|(at, g)| typed_key_event(g).map(|ev| (at, ev)))
        .collect()
}

/// A turn's paste of `text`, framed at the drain: the text exactly as the turn was given it. The
/// `paste` verb reads a trailing literal `\n` as a line break
/// ([`crate::control::control_input::paste_text`]); a turn reads none, since it submits with its
/// own Enter and a `typed=1` turn presses a literal `\n` as the two keys it is: a rest whose paste turned it into a CR sent a second Enter where
/// the program had no bracketed paste, and lost the text where it had.
pub(crate) fn turn_paste_event(text: &str) -> InputEvent {
    InputEvent::Paste(text.to_owned(), crate::input::PasteFraming::AtDrain)
}

/// One write of a `typed=1` turn's rest ([`rest_writes`]).
#[derive(Debug)]
pub(crate) enum RestWrite {
    /// A run of text the typing would have pressed as character keys, written as those keys'
    /// bytes in one write ([`rest_run_event`]).
    Run {
        /// The run, with no Tab, line break or other control character in it.
        text: String,
        /// The key of each of its graphemes as the turn's text segments them ([`typed_key_event`]),
        /// the keys typing it presses. A control character typing drops still parts the
        /// graphemes either side of it, which the run's text alone would join.
        keys: Vec<InputEvent>,
    },
    /// A Tab or Enter, pressed as the key the typing presses.
    Key(InputEvent),
}

/// Whether the typed key `ev` is one a rest presses on its own rather than in a run
/// ([`rest_writes`]).
fn presses_in_rest(ev: &InputEvent) -> bool {
    use aterm_types::keyboard::{Key, NamedKey};
    matches!(
        ev,
        InputEvent::Key {
            key: Key::Named(NamedKey::Enter | NamedKey::Tab),
            ..
        }
    )
}

/// The writes of a `typed=1` turn's `rest`, the text after the last key it typed
/// ([`TypingPace`]): what typing it would have sent, in as few writes as that allows. Each run
/// of text between its Tabs and line breaks is one write of the bytes its keys send
/// ([`rest_run_event`]), without the control characters typing drops ([`typed_key_event`]), and
/// each Tab and line break goes as the Tab or Enter key typing presses, on its own: a fenced
/// turn's hold judges what the program drew of each write, and a line break ends what a program
/// reads as one line. The keys typed and the writes of the rest are the whole text as typing it
/// would have gone, byte for byte, in every keyboard mode.
pub(crate) fn rest_writes(rest: &str) -> Vec<RestWrite> {
    use aterm_grapheme::GraphemeClusters;
    let mut writes = Vec::new();
    let mut run = String::new();
    let mut keys = Vec::new();
    let flush = |writes: &mut Vec<RestWrite>, run: &mut String, keys: &mut Vec<InputEvent>| {
        if !keys.is_empty() {
            writes.push(RestWrite::Run {
                text: std::mem::take(run),
                keys: std::mem::take(keys),
            });
        }
    };
    for g in rest.graphemes() {
        match typed_key_event(g) {
            Some(ev) if presses_in_rest(&ev) => {
                flush(&mut writes, &mut run, &mut keys);
                writes.push(RestWrite::Key(ev));
            }
            Some(ev) => {
                run.push_str(g);
                keys.push(ev);
            }
            None => {}
        }
    }
    flush(&mut writes, &mut run, &mut keys);
    writes
}

/// The one write of a rest's run ([`RestWrite::Run`]): the bytes each of its `keys` sends under
/// `keyboard`, the program's keyboard mode as the seam's mirror last read it
/// ([`crate::input::typed_bytes`]), in one [`InputEvent::KeySequence`]. Not a paste: a program
/// may take a bracketed paste otherwise than the keys, and where the cap or the pace cuts the
/// text depends on load. Fish 4.9 escapes each quote of a paste whose token is open in quotes, so
/// `'fix it'` with the rest pasted after `'f` reads `'fix it\'`, a quote left open that the Enter
/// continues (measured on a `lab instance`, 2026-11-06), and vim inserts a paste with no mapping
/// or abbreviation. The keys' bytes are fixed under the mode the mirror holds when the turn
/// writes the run, as a typed key's are when it is pressed and a keyboard's are: a program that
/// changes its keyboard mode after input was written reads that input as encoded before the
/// change, run and key alike.
pub(crate) fn rest_run_event(
    keys: &[InputEvent],
    keyboard: aterm_types::keyboard::KeyboardMode,
) -> InputEvent {
    InputEvent::KeySequence(
        keys.iter()
            .filter_map(|ev| crate::input::typed_bytes(ev, keyboard))
            .flatten()
            .collect(),
    )
}

/// How many writes [`rest_writes`] makes of `text[at..]`, for `at` each grapheme's offset in
/// `text` and its end, ascending by offset: what a typed turn's pace budgets for the rest it
/// would write after a key ([`TypingPace::fits`]), counted in one pass from the end.
fn rest_write_counts(text: &str) -> Vec<(usize, usize)> {
    use aterm_grapheme::GraphemeClusters;
    let graphemes: Vec<(usize, &str)> = text.grapheme_indices().collect();
    let mut counts = Vec::with_capacity(graphemes.len() + 1);
    counts.push((text.len(), 0));
    // The writes from here on, and whether the first of them is a run the text here joins.
    let (mut writes, mut opens_run) = (0usize, false);
    for &(at, g) in graphemes.iter().rev() {
        match typed_key_event(g) {
            Some(ev) if presses_in_rest(&ev) => {
                writes += 1;
                opens_run = false;
            }
            Some(_) => {
                writes += usize::from(!opens_run);
                opens_run = true;
            }
            None => {}
        }
        counts.push((at, writes));
    }
    counts.reverse();
    counts
}

/// THE PACE OF A TYPED TURN: a `turn typed=1` never leaves part of its text in the composer for
/// its deadline. Before each key it asks whether that key, the writes of everything after it
/// ([`rest_writes`]: one run, plus a key and a run for each Tab or line break), the
/// longest wait a typing hold has needed so far, and the submit's share of the turn
/// ([`TypingPace::new`]) still end by the deadline, at the pace its keys have gone so far and never
/// faster than `cadence=`. The moment they would not, the rest of the text goes as those writes
/// (each run its keys' bytes in one write, [`rest_run_event`]), none of which cuts part-way, and
/// the turn goes on to its submit. Typing that keeps delivering is never cut: a drive's turn
/// (`timeout=20000`) to a loaded Claude Code worker whose keys went at about 5 a second answered
/// `skipped reason=deadline typed=106` and left the 106 in its composer, and each retry stacked
/// another (m14 driving m108, 2026-11-04). Before its first key the turn can still
/// end as it found the composer, and does when that key does not fit: `reason=deadline typed=0
/// pasted=0`, never a rest whose submit the deadline leaves no time to see, which a retry would
/// send again. The echo's settle after the rest ends in time for the submit's share
/// ([`press_by`]); a Claude Code composer's echo is still waited for, which alone shows the
/// program took the text before its Enter is judged. A
/// fenced turn's wait for a program that draws nothing of what it was sent is a hang detector,
/// and cuts where it waited ([`TypingHold::held`]); one as long as an earlier wait of the turn
/// still fits, because the pace kept room for it.
struct TypingPace {
    cadence: std::time::Duration,
    /// What the turn keeps after its typing: the echo's settle and the submit's window, at most
    /// half the turn, so a short `timeout=` still types.
    reserve: std::time::Duration,
    /// When the first key was pressed, and the deadline then: what a typing hold gave back to the
    /// deadline since is the program's drawing, no key's.
    first: Option<(std::time::Instant, std::time::Instant)>,
    /// The longest a typing hold has waited for the program in this turn, which the next key's
    /// hold may wait again.
    longest_hold: std::time::Duration,
}

impl TypingPace {
    /// The pace of a turn of `timeout` whose keys go every `cadence`, the echo of its text settling
    /// for `settle` and its submit verified within `submit_window` (none for `submit=none`).
    fn new(
        cadence: std::time::Duration,
        timeout: std::time::Duration,
        settle: std::time::Duration,
        submit_window: Option<std::time::Duration>,
    ) -> Self {
        Self {
            cadence,
            reserve: (settle + submit_window.unwrap_or_default()).min(timeout / 2),
            first: None,
            longest_hold: std::time::Duration::ZERO,
        }
    }

    /// Notes a key pressed at `at`, under `deadline`.
    fn pressed(&mut self, at: std::time::Instant, deadline: std::time::Instant) {
        self.first.get_or_insert((at, deadline));
    }

    /// Notes a typing hold that waited `waited` for the program before a key.
    fn held(&mut self, waited: std::time::Duration) {
        self.longest_hold = self.longest_hold.max(waited);
    }

    /// How long a key takes, after `typed` keys, judged at `now` under `deadline`: their mean
    /// since the first, less what the holds gave back, and never less than the cadence.
    fn per_key(
        &self,
        typed: usize,
        now: std::time::Instant,
        deadline: std::time::Instant,
    ) -> std::time::Duration {
        let Some((first, deadline_then)) = self.first else {
            return self.cadence;
        };
        let given_back = deadline.saturating_duration_since(deadline_then);
        let spent = now
            .saturating_duration_since(first)
            .saturating_sub(given_back);
        let typed = u32::try_from(typed.max(1)).unwrap_or(u32::MAX);
        (spent / typed).max(self.cadence)
    }

    /// Whether the next key, after `typed`, still fits at `now` under `deadline`: its hold's wait
    /// at the longest so far, it, the `rest` writes after it ([`rest_write_counts`]; at least the
    /// one run a rest takes), and the reserve, each key and write at [`TypingPace::per_key`].
    fn fits(
        &self,
        typed: usize,
        rest: usize,
        now: std::time::Instant,
        deadline: std::time::Instant,
    ) -> bool {
        #[cfg(any(test, debug_assertions))]
        if aterm_types::negative_control::on("typing-no-remainder") {
            return now < deadline;
        }
        let per = self.per_key(typed, now, deadline);
        u32::try_from(rest.max(1))
            .ok()
            .and_then(|rest| rest.checked_add(1))
            .and_then(|writes| per.checked_mul(writes))
            .and_then(|both| both.checked_add(self.reserve))
            .and_then(|need| need.checked_add(self.longest_hold))
            .and_then(|need| now.checked_add(need))
            .is_some_and(|end| end <= deadline)
    }
}

/// The typing planner's clock. A live test advances it after each delivered key instead of
/// sleeping for the same simulated service time. Holds use native time and give that time back;
/// after typing, the remaining virtual budget becomes the native budget for the remainder.
struct TypingClock {
    elapsed: std::time::Duration,
    step: std::time::Duration,
}

impl TypingClock {
    fn new(step: std::time::Duration) -> Self {
        Self {
            elapsed: std::time::Duration::ZERO,
            step,
        }
    }

    fn live() -> Self {
        let step = aterm_types::dev_seam!("ATERM_DEBUG_TURN_KEY_CLOCK_STEP_MS")
            .and_then(|ms| ms.to_str().and_then(|ms| ms.parse::<u64>().ok()))
            .map_or(std::time::Duration::ZERO, std::time::Duration::from_millis);
        Self::new(step)
    }

    fn at(&self, native: std::time::Instant) -> std::time::Instant {
        native.checked_add(self.elapsed).unwrap_or(native)
    }

    fn now(&self) -> std::time::Instant {
        self.at(std::time::Instant::now())
    }

    fn delivered(&mut self) {
        self.elapsed = self.elapsed.saturating_add(self.step);
    }

    fn native(&self, virtual_time: std::time::Instant) -> std::time::Instant {
        virtual_time
            .checked_sub(self.elapsed)
            .unwrap_or(virtual_time)
    }
}

/// When the echo's settle after a turn's last write ends at the latest: early enough that the
/// submit's `window` (its share of the turn, as [`TypingPace`] reserves it) still fits before
/// `deadline`, never sooner than the `settle` after the typing ended at `typed`, and never past
/// the deadline. The settle latches only once the program stops drawing, which a working agent's
/// spinner never does, so without this bound it would spend the submit's window. A Claude Code
/// composer's echo wait is not bounded so: it ends once the composer shows the text, and its
/// Enter is judged by that text leaving, which a wait cut short would lose.
fn press_by(
    deadline: std::time::Instant,
    typed: std::time::Instant,
    settle: std::time::Duration,
    window: std::time::Duration,
) -> std::time::Instant {
    let settled = typed.checked_add(settle).unwrap_or(typed);
    deadline.min(deadline.checked_sub(window).unwrap_or(typed).max(settled))
}

/// Split `text` at the `cap`th grapheme ([`TYPED_CAP`] unless the turn said `cap=`): `(typed prefix, the rest [`rest_writes`] writes)`.
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
        let clock = aterm_types::advance_clock::Clock::new(
            aterm_types::dev_seam!("ATERM_DEBUG_TYPING_CLOCK"),
            ctx.self_id.as_str(),
        );
        let mut since = None;
        loop {
            let look = self.look(&term_lock(term), ctx, sent);
            match look {
                Look::Held(cursor) => {
                    if let Some(since) = since {
                        let waited =
                            hold_wait_now(clock.read().now).saturating_duration_since(since);
                        *deadline = deadline.checked_add(waited).unwrap_or(*deadline);
                    }
                    return Ok(cursor);
                }
                Look::Stop(why) => return Err(why),
                Look::Wait => {
                    let reading = clock.read();
                    let now = hold_wait_now(reading.now);
                    if now >= *deadline {
                        return Err("deadline");
                    }
                    #[cfg(any(test, debug_assertions))]
                    if aterm_types::negative_control::on("typing-latency-budget")
                        && since.is_some_and(|since| {
                            now.saturating_duration_since(since)
                                >= std::time::Duration::from_millis(150)
                        })
                    {
                        return Err("negative control: echo latency budget");
                    }
                    since.get_or_insert(now);
                    reading.observed();
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

fn hold_wait_now(native: std::time::Instant) -> std::time::Instant {
    #[cfg(test)]
    if let Some(now) = HOLD_WAIT_FOR_TEST.with(|slot| slot.borrow().as_ref().map(|hand| hand())) {
        return now;
    }
    native
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
    Started(Option<u64>),
    /// The cursor left the row the text was typed on: a program with no command blocks took the line.
    Moved,
}

/// The shell input block standing when Enter is armed, not a later prompt.
#[derive(Clone, Copy)]
struct ShellSubmit {
    started: u64,
    block: Option<u64>,
}

impl ShellSubmit {
    fn capture(t: &Terminal) -> Self {
        Self {
            started: t.started_command_seq(),
            block: t
                .current_block()
                .filter(|b| b.state == aterm_core::terminal::BlockState::EnteringCommand)
                .map(|b| b.id),
        }
    }

    /// One accepted start and the same retained block prove the association; a reset keeps the
    /// block (the shell's marks outlive it), so the claim stands through one.
    /// Eviction or multiple starts retain submit evidence, but no block claim.
    fn block(self, t: &Terminal) -> Option<u64> {
        if self.started.checked_add(1) != Some(t.started_command_seq()) {
            return None;
        }
        let id = self.block?;
        t.block_by_id(id)
            .filter(|b| {
                matches!(
                    b.state,
                    aterm_core::terminal::BlockState::Executing
                        | aterm_core::terminal::BlockState::Complete
                )
            })
            .map(|b| b.id)
    }
}

#[cfg(test)]
#[path = "control_session_shell_submit_tests.rs"]
mod shell_submit_tests;

fn press_look(
    term: &Arc<Mutex<Terminal>>,
    id: aterm_core::terminal::WatchId,
    (base, typed_row): (Option<ShellSubmit>, Option<u16>),
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
        let started = closing && base.is_some_and(|base| t.started_command_seq() > base.started);
        let answer = if started {
            Some(PressLook::Started(base.and_then(|base| base.block(&t))))
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

/// WHAT SHOWED A `turn`'S SUBMIT, its verdict's `submit_by=`: the evidence `submitted=1` rests on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubmitBy {
    /// A proven codex-alab's own report began a turn after a press made between turns
    /// ([`codex_began_a_turn`]).
    Claim,
    /// The typed text left Claude Code's composer, or the composer gave way to a box.
    Composer,
    /// A shell's command block started (an OSC 133/633 `C`).
    Block,
    /// The screen moved: content past the press, or the cursor off the row the text was typed on.
    Screen,
}

impl SubmitBy {
    /// Its wire word: `claim`, `composer`, `block`, `screen`.
    const fn as_str(self) -> &'static str {
        match self {
            SubmitBy::Claim => "claim",
            SubmitBy::Composer => "composer",
            SubmitBy::Block => "block",
            SubmitBy::Screen => "screen",
        }
    }
}

/// THE TURN COUNT A PROVEN CODEX-ALAB'S CLAIM MUST PASS, read from the engine's report as the
/// press is armed ([`ProgramStateReport::turns`]), or `None` while the report standing is in a
/// turn (`working`, `blocked`). Codex takes an Enter then into its running turn or its queue, and
/// a turn it begins within the window can be another line's (one it queued earlier, its own
/// Autopilot's), so no turn begun is this Enter's claim and the screen judges the press.
///
/// [`ProgramStateReport::turns`]: aterm_core::terminal::program_state::ProgramStateReport::turns
fn claim_base(report: aterm_core::terminal::program_state::ProgramStateReport) -> Option<u64> {
    use aterm_core::terminal::program_state::ProgramStateWord as W;
    let in_a_turn = report
        .state
        .is_some_and(|s| matches!(s.state, W::Working | W::Blocked));
    (!in_a_turn).then_some(report.turns)
}

/// WHETHER CODEX BEGAN A TURN SINCE THE PRESS: the engine's report counts a turn begun past
/// `turns`, its count read at the press while codex stood between turns ([`claim_base`]), and the
/// report standing names codex. A `working` after a turn's end (or none) begins one; the same
/// `working` re-sent mid-turn does not, nor does one after a box it answered.
///
/// A REPORT SAYS NO MORE THAN THAT A TURN BEGAN, not which line began it: a turn codex's own
/// Autopilot types at that same moment is taken for the Enter's too.
fn codex_began_a_turn(
    report: aterm_core::terminal::program_state::ProgramStateReport,
    turns: u64,
) -> bool {
    report.turns != turns
        && report
            .state
            .is_some_and(|s| s.app == aterm_core::terminal::program_state::ProgramApp::Codex)
}

/// [`press_look`] for a proven codex-alab between turns: it answers only at codex's own claim
/// ([`codex_began_a_turn`] past `turns`), read from the engine on every wake rather than from the
/// status sweep's copy a sweep behind, and notes in `screen` whether the screen showed the press
/// meanwhile (the watch latched, or the cursor left `typed_row`), which is no submit there: a
/// popup that took the Enter redraws too. It only stops the presses at the window's close.
fn claim_look<'a>(
    term: &'a Arc<Mutex<Terminal>>,
    id: aterm_core::terminal::WatchId,
    typed_row: Option<u16>,
    turns: u64,
    screen: &'a std::cell::Cell<bool>,
) -> impl FnMut(std::time::Instant, bool, bool) -> (Option<()>, Option<std::time::Instant>) + 'a {
    move |now, parked, closing| {
        let mut t = term_lock(term);
        fire_idle_unless_parked(&mut t, now, parked);
        if t.watch_poll(id).is_some() || typed_row.is_some_and(|row| t.cursor().row != row) {
            screen.set(true);
        }
        let claimed = codex_began_a_turn(t.program_state(), turns);
        if claimed || closing {
            t.watch_disarm(id);
        }
        (claimed.then_some(()), t.watch_next_deadline())
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
    /// The person made a gesture since the caller's read (`if-human=`), judged under the lock at
    /// every try: nothing of this press was written.
    Person,
    /// The sink took none of the Enter, or input queued before it had not all gone, until the
    /// submit window ended: nothing of this press was written.
    Busy,
    /// `lease release force` took the turn's slot before a try: nothing of this press was
    /// written.
    Preempted,
    Failed(String),
}

/// The guarded Enter of a turn, tried until `window` while the sink takes none of it or input
/// queued before it has not all gone, and never once `preempted` says the turn's slot is gone,
/// judged before each try and again under the terminal lock the try writes under.
fn guarded_submit(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    guard: &dyn aterm_core::terminal::RowMatch,
    human: Option<u64>,
    window: std::time::Instant,
    mark: Option<&crate::control_input_ack::InputMark>,
    preempted: &(dyn Fn() -> bool + Sync),
) -> GuardedSubmit {
    use super::control_input::GuardedInput;
    use crate::input::Delivery;
    const BUSY_RETRY: std::time::Duration = std::time::Duration::from_millis(5);
    loop {
        if preempted() {
            return GuardedSubmit::Preempted;
        }
        let Some(enter) = crate::control::parse_key("enter") else {
            return GuardedSubmit::Failed("ERR internal: `enter` did not parse\n".to_string());
        };
        let stands = || !preempted();
        let fence = super::control_input::InputFence {
            guard: Some(guard),
            guard_at_cursor: true,
            human,
            standing: Some(&stands),
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
            // A slot lost under the lock is a skip the fence says nothing more of.
            GuardedInput::Skipped | GuardedInput::Changed if preempted() => {
                return GuardedSubmit::Preempted;
            }
            GuardedInput::Skipped | GuardedInput::Changed => return GuardedSubmit::Missed,
            GuardedInput::Person => return GuardedSubmit::Person,
            GuardedInput::Queued | GuardedInput::Pressed(Delivery::BusyZero)
                if std::time::Instant::now() < window =>
            {
                std::thread::sleep(BUSY_RETRY);
            }
            GuardedInput::Queued | GuardedInput::Pressed(Delivery::BusyZero) => {
                return GuardedSubmit::Busy;
            }
            decision @ GuardedInput::Pressed(_) => {
                return GuardedSubmit::Failed(super::control_input::guarded_input_reply(decision));
            }
        }
    }
}

/// `turn`'s usage line.
pub(crate) const TURN_USAGE: &str = "ERR usage: turn [id=<epoch>:<producer>:<seq>] [if-supervisor=<holder>] [idle=<ms>] [timeout=<ms>] [submit=<key|none|guarded:<re>>] [settle=match:<re>|gone:<re>|submitted] [submit_window=<ms>] [presses=<n>] [submit_verify=<auto|seq|block>] [trim=<0|1>] [typed=<0|1>] [cadence=<ms>] [cap=<n>] [yield=<floor>] [if-gen=<epoch>.<seq>] [if-rows=<first>-<last>:<hex16>] [if-human=<n>] [cause=@<off>|<seq>] <text>\n";

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
    /// The person's input count (`status human_seq=`) the caller read the screen at: any gesture
    /// since, a key that moves no fenced row among them (Shift+Tab's mode, an undrawn Ctrl-R),
    /// skips the turn (`reason=person`), judged under the fence's lock and again before its paste
    /// and its first press.
    if_human: Option<u64>,
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
    let mut if_human: Option<u64> = None;
    let mut cause_opt: Option<crate::session_log::CauseRef> = None;
    let is_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let (options, text) = aterm_types::control_verbs::turn_option_words(rest);
    for (k, v) in options {
        match k {
            "idle" => match is_digits(v).then(|| v.parse::<u64>().ok()).flatten() {
                Some(ms) => idle_ms = ms.clamp(1, 600_000),
                None => return Err(USAGE.to_string()),
            },
            "timeout" => match is_digits(v).then(|| v.parse::<u64>().ok()).flatten() {
                Some(ms) => timeout_ms = ms,
                None => return Err(USAGE.to_string()),
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
            "submit_window" => match is_digits(v).then(|| v.parse::<u64>().ok()).flatten() {
                Some(ms) => submit_window_ms = ms.clamp(1, 600_000),
                None => return Err(USAGE.to_string()),
            },
            "presses" => match is_digits(v).then(|| v.parse::<u32>().ok()).flatten() {
                Some(n) if n >= 1 => presses = n.min(10),
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
            "cadence" => match is_digits(v).then(|| v.parse::<u64>().ok()).flatten() {
                Some(ms) => cadence_ms = ms.clamp(1, 2_000),
                None => return Err(USAGE.to_string()),
            },
            "cap" => match is_digits(v).then(|| v.parse::<usize>().ok()).flatten() {
                Some(n) if n >= 1 => typed_cap = n.min(TYPED_CAP_MAX),
                _ => return Err(USAGE.to_string()),
            },
            "yield" => match (!v.starts_with('+'))
                .then(|| v.parse::<f32>().ok())
                .flatten()
            {
                Some(f) if f.is_finite() && (0.0..=1.0).contains(&f) => yield_floor = Some(f),
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
            "if-human" => match is_digits(v).then(|| v.parse::<u64>().ok()).flatten() {
                Some(n) => if_human = Some(n),
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
        if_human,
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

/// `turn [<options>] <text>` (the options: [`TURN_USAGE`]) — one complete human turn against the target session: type `<text>`, submit it with a real keypress, block until the app's response settles, and return the settled screen — `OK <rows> turn submitted=<0|1> status=<settled|timeout|hangup|submitted> seq=<n> id=<n> dur_ms=<n> hash=<hex16>[ submit_by=<claim|composer|block|screen>][ typed=<n> pasted=<n>][ yielded_ms=<ms>][ pressed=<n>][ task_rev=<n>][ agent_rev=<n>][ frame=open][ trimmed=<k>]` then `<rows>` text rows (the same rows `text` prints); `submit_by=` names what showed a verified submit.
pub(crate) fn cmd_turn(
    term: &Arc<Mutex<Terminal>>,
    store: &Store,
    session: u64,
    rest: &str,
    ctx: &SessionCtx,
    io: &TurnIo<'_>,
) -> String {
    let _producer = io.mark.and_then(|mark| mark.supervisor_producer());
    let _native_producer = io.mark.and_then(|mark| mark.native_producer());
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
        if_human,
        text,
        ..
    } = o;
    // A gesture the person made since the caller's read, by the window's count of them.
    let person_moved = || if_human.is_some_and(|seen| ctx.human_input.seq() != seen);

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
    if if_gen.is_some() || if_rows.is_some() || if_human.is_some() {
        let mut look = ctx.frame.look_frame();
        let typing = typed && !text.is_empty();
        let held_open = std::cell::Cell::new(false);
        let waits = |t: &Terminal| {
            held_open.set(typing && if_rows.and_then(|fence| Frame::of(t, fence)).is_some());
            if_rows.is_some() && !held_open.get()
        };
        let terminal = crate::term_lock_whole_when(term, crate::park::Lane::SERVED, waits);
        let unvouched = if_rows.filter(|_| held_open.get());
        if person_moved() {
            let seq = terminal.content_seq();
            return format!("OK 0 turn skipped reason=person submitted=0 seq={seq} id={turn_id}\n");
        }
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

    // A turn stopped after it wrote: what stands of its text, a typed turn's keys and runs or a
    // plain turn's paste, is in the verdict, so its caller reads the text left in place.
    let stopped = |why: &str, (keys, pasted): (usize, usize)| -> String {
        if typed || pasted > 0 {
            typed_cut(term, why, keys, pasted, turn_id)
        } else {
            let seq = term_lock(term).content_seq();
            format!("OK 0 turn skipped reason={why} submitted=0 seq={seq} id={turn_id}\n")
        }
    };
    let halted = |landed: Option<(usize, usize)>| -> Option<String> {
        let refusal = crate::fabric::halt_refusal(ctx, "turn")?;
        Some(match landed {
            None => refusal,
            Some(landed) => stopped("halted", landed),
        })
    };
    // `lease release force` took the turn's slot (a wedged turn's preemption): the turn writes
    // nothing more, and what it wrote stands, as a halt leaves it. A write already past its
    // check still goes; each later one, each try of a guarded Enter included, is checked first.
    let lease_gone = || {
        !matches!(
            ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner()).as_ref(),
            Some(crate::Lease::Turn { id, .. }) if *id == turn_id
        )
    };
    let preempted = |landed: (usize, usize)| -> Option<String> {
        lease_gone().then(|| stopped("preempted", landed))
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
    // Every delivery the turn makes waits for its program whatever the caller does; whether it
    // writes at all is judged here, before its first write: a caller already gone gets nothing
    // written (on the main thread its gate refuses it too), not a text whose Enter never comes.
    let _delivering = crate::control::TurnInput::enter();
    let gone_before_writing = || !wrote.get() && crate::control::caller_hung_up();
    // When the typing ended, which the echo of the text is waited for from.
    let mut typed_end = None;
    // The caret the typing hold read just before the turn's last write, which its echo moves.
    let mut sent_from = None;
    // The same before its last write of text (a character key or a paste), not a Tab or Enter.
    let mut text_from = None;
    if !text.is_empty() {
        if typed {
            let (head, _) = split_typed_cap(text, typed_cap);
            // Where the rest starts: past the cap, or at the first key the pace leaves no room
            // to press ([`TypingPace`]).
            let mut rest_at = head.len();
            let mut pace = TypingPace::new(
                cadence,
                Duration::from_millis(timeout_ms),
                ECHO_SETTLE,
                (submit != "none").then(|| Duration::from_millis(submit_window_ms)),
            );
            let mut clock = TypingClock::live();
            let mut due = clock.now();
            let keys = typed_key_events(head);
            // The writes of the rest after each key: from the next key on, or past the cap.
            let counts = rest_write_counts(text);
            let rest_after: Vec<usize> = (0..keys.len())
                .map(|i| {
                    let next = keys.get(i + 1).map_or(head.len(), |&(at, _)| at);
                    counts
                        .binary_search_by_key(&next, |&(at, _)| at)
                        .ok()
                        .and_then(|found| counts.get(found))
                        .map_or(0, |&(_, writes)| writes)
                })
                .collect();
            for ((at, ev), rest) in keys.into_iter().zip(rest_after) {
                let now = clock.now();
                if due > now {
                    std::thread::sleep(due - now);
                }
                if !pace.fits(typed_keys, rest, clock.now(), deadline) {
                    // Nothing written yet: the turn can still end as it found the composer.
                    if typed_keys == 0 {
                        return typed_cut(term, "deadline", 0, 0, turn_id);
                    }
                    #[cfg(any(test, debug_assertions))]
                    if aterm_types::negative_control::on("typing-no-remainder") {
                        return typed_cut(term, "deadline", typed_keys, 0, turn_id);
                    }
                    rest_at = at;
                    break;
                }
                if exited(store) {
                    return "ERR exited\n".to_string();
                }
                if let Some(h) = &mut hold {
                    let digit = is_digit_key(&ev);
                    let before = deadline;
                    let mut native_deadline = clock.native(deadline);
                    let held = h.held(
                        term,
                        ctx,
                        &sub,
                        sent_from.filter(|_| digit).map(Sent::key),
                        cadence,
                        &mut native_deadline,
                    );
                    deadline = clock.at(native_deadline);
                    match held {
                        Ok(cursor) => {
                            sent_from = Some(cursor);
                            if !presses_in_rest(&ev) {
                                text_from = Some(cursor);
                            }
                            // What it waited, it gave back: the next key keeps room for as much.
                            pace.held(deadline.saturating_duration_since(before));
                        }
                        Err(why) => return typed_cut(term, why, typed_keys, 0, turn_id),
                    }
                }
                if let Some(cut) = halted(committed.is_some().then_some((typed_keys, 0))) {
                    return cut;
                }
                if let Some(cut) = preempted((typed_keys, 0)) {
                    return cut;
                }
                if gone_before_writing() {
                    return KEY_DELIVERY_FAILED.to_string();
                }
                if typed_keys == 0 {
                    note_ctl_input();
                }
                let pressed_at = clock.now();
                due = pressed_at + cadence;
                pace.pressed(pressed_at, deadline);
                let first = !wrote.replace(true);
                if !(io.key)(ev) {
                    return undelivered(first, KEY_DELIVERY_FAILED);
                }
                clock.delivered();
                committed.get_or_insert_with(crate::control::CommittedInput::enter);
                typed_keys += 1;
                if typed_keys == 1
                    && let Some(path) = aterm_types::dev_seam!("ATERM_DEBUG_TURN_KEY_BARRIER")
                {
                    let barrier =
                        aterm_types::advance_clock::Clock::new(Some(path), ctx.self_id.as_str());
                    barrier.read().observed();
                }
            }
            // The rest of the text, past the cap or from the key the pace stopped before, as
            // typing it would have gone: each run its keys' bytes in one write, its Tabs and line
            // breaks pressed.
            let writes = rest_writes(&text[rest_at..]);
            // The pace asks before each key only, and a head of control characters, which
            // `typed_key_events` drops, presses none: the rest's first write is then the turn's
            // first, and goes only where the rest and the submit fit, as a first key does.
            if typed_keys == 0
                && !writes.is_empty()
                && !pace.fits(0, writes.len() - 1, clock.now(), deadline)
            {
                return typed_cut(term, "deadline", 0, 0, turn_id);
            }
            deadline = clock.native(deadline);
            for write in writes {
                if exited(store) {
                    return "ERR exited\n".to_string();
                }
                // A run with a digit in it waits as a typed digit does, for the program to
                // read and draw what went before it: a box mounted meanwhile would take the
                // digit. Typing waits on the key just before a digit, so a run waits on a Tab or
                // Enter the rest pressed just before it only when it starts with the digit, and
                // otherwise on the last text written, as the keys of its own run would have: a
                // composer's Tab may move no caret, and typing waits on that for no later digit.
                let sent = match &write {
                    RestWrite::Run { text, .. }
                        if text.starts_with(|c: char| c.is_ascii_digit()) =>
                    {
                        sent_from
                    }
                    RestWrite::Run { text, .. } if text.contains(|c: char| c.is_ascii_digit()) => {
                        text_from
                    }
                    RestWrite::Run { .. } => None,
                    RestWrite::Key(ev) => sent_from.filter(|_| is_digit_key(ev)),
                };
                match hold
                    .as_mut()
                    .map(|h| h.held(term, ctx, &sub, sent.map(Sent::key), cadence, &mut deadline))
                {
                    Some(Err(why)) => {
                        return typed_cut(term, why, typed_keys, pasted_graphemes, turn_id);
                    }
                    Some(Ok(cursor)) => {
                        sent_from = Some(cursor);
                        if matches!(write, RestWrite::Run { .. }) {
                            text_from = Some(cursor);
                        }
                    }
                    None => {}
                }
                if let Some(cut) = halted(
                    committed
                        .is_some()
                        .then_some((typed_keys, pasted_graphemes)),
                ) {
                    return cut;
                }
                if let Some(cut) = preempted((typed_keys, pasted_graphemes)) {
                    return cut;
                }
                if gone_before_writing() {
                    return KEY_DELIVERY_FAILED.to_string();
                }
                if typed_keys == 0 && pasted_graphemes == 0 {
                    note_ctl_input();
                }
                let first = !wrote.replace(true);
                let delivered = match write {
                    RestWrite::Run { keys, .. } => {
                        (io.key)(rest_run_event(&keys, ctx.modes.keyboard_mode()))
                            .then_some((0, keys.len()))
                    }
                    RestWrite::Key(ev) => (io.key)(ev).then_some((1, 0)),
                };
                let Some((keys, graphemes)) = delivered else {
                    return undelivered(first, KEY_DELIVERY_FAILED);
                };
                committed.get_or_insert_with(crate::control::CommittedInput::enter);
                typed_keys += keys;
                pasted_graphemes += graphemes;
            }
        } else {
            if exited(store) {
                return "ERR exited\n".to_string();
            }
            if let Some(refusal) = halted(None) {
                return refusal;
            }
            if person_moved() {
                return typed_cut(term, "person", 0, 0, turn_id);
            }
            if let Some(cut) = preempted((0, 0)) {
                return cut;
            }
            if gone_before_writing() {
                return PASTE_DELIVERY_FAILED.to_string();
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
        let typed_at = Instant::now();
        typed_end = Some(typed_at);
        // The submit's share of the turn, as the pace reserves it: at most half of `timeout=`.
        let window = if submit == "none" {
            Duration::ZERO
        } else {
            Duration::from_millis(submit_window_ms).min(Duration::from_millis(timeout_ms) / 2)
        };
        match wait_committed(
            id,
            press_by(deadline, typed_at, ECHO_SETTLE, window).min(typed_at + EchoWait::CAP),
        ) {
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

    let mut submit_by: Option<SubmitBy> = None;
    let mut submit_block = None;
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
            let clock = aterm_types::advance_clock::Clock::new(
                aterm_types::dev_seam!("ATERM_DEBUG_ECHO_CLOCK"),
                ctx.self_id.as_str(),
            );
            let look = || {
                let t = crate::term_lock_whole_when(term, finishing, |_| true);
                (composer_verify_key(&t, text), t.content_seq())
            };
            let mut echo = typed_end.map(|typed| EchoWait::after_typing(typed, deadline));
            let wait = |after| {
                let Some(echo) = echo.as_mut() else {
                    return Ok(false);
                };
                let reading = clock.read();
                let Some(until) =
                    echo.look(crate::input_stall::input_unread(&ctx.sink), reading.now)
                else {
                    return Ok(false);
                };
                let until = reading.wait_until(until);
                reading.observed();
                let Some(id) = arm(WatcherSpec::SeqAdvanced { after }) else {
                    return Err(format!("ERR {WAITS_FULL}\n"));
                };
                match wait_committed(id, until) {
                    Parked::Exited => Err("ERR exited\n".to_string()),
                    // A wait nothing drew in looks again: a starved program that reads nothing
                    // draws nothing either.
                    Parked::Done(_) | Parked::Deadline => Ok(true),
                    Parked::HungUp => Ok(false),
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
        // A proven codex-alab between turns says itself when it took the line: its report
        // beginning a turn within the press's window, and nothing else (`claim_base`).
        let by_claim = auto_mode && by_screen && proven_codex_alab(ctx);
        for press_no in 0..presses {
            // A re-press is for an Enter the program read and swallowed; one still unread would
            // only queue another behind it.
            if press_no > 0 && crate::input_stall::input_unread(&ctx.sink) {
                break;
            }
            if press_no > 0 && ctx.update_parked.is_parked() {
                break;
            }
            // What a cut before the Enter leaves standing: the keys typed and the runs written,
            // or what the paste of the text kept (`Terminal::paste_kept_text`).
            let pasted = || {
                if typed {
                    pasted_graphemes
                } else {
                    aterm_grapheme::GraphemeClusters::graphemes(
                        Terminal::paste_kept_text(text).as_str(),
                    )
                    .count()
                }
            };
            if let Some(cut) = halted(committed.is_some().then(|| (typed_keys, pasted()))) {
                if press_no > 0 {
                    break;
                }
                return cut;
            }
            // A gesture since the caller's read stops the turn before its Enter: the text stands
            // where it was put, the person's to send. A re-press follows an Enter that went.
            if person_moved() {
                if press_no > 0 {
                    break;
                }
                return typed_cut(term, "person", typed_keys, pasted(), turn_id);
            }
            if let Some(cut) = preempted((typed_keys, pasted())) {
                if press_no > 0 {
                    break;
                }
                return cut;
            }
            // A text that typed no key and pasted nothing (control characters alone) makes the
            // Enter its first write, judged as any first write is.
            if gone_before_writing() {
                return KEY_DELIVERY_FAILED.to_string();
            }
            let window = deadline.min(Instant::now() + Duration::from_millis(submit_window_ms));
            let (base_started, typed_row, base_turns, mut id) = {
                let mut t = term_lock(term);
                let base = ShellSubmit::capture(&t);
                let row = by_screen.then(|| t.cursor().row);
                let turns = by_claim.then(|| claim_base(t.program_state())).flatten();
                let after = t.content_seq();
                match t.watch(WatcherSpec::SeqAdvanced { after }, Instant::now()) {
                    Some(id) => (base, row, turns, id),
                    None => return format!("ERR {WAITS_FULL}\n"),
                }
            };
            note_ctl_input();
            let first = !wrote.replace(true);
            if let Some(guard) = submit_matcher.as_deref() {
                let outcome =
                    guarded_submit(term, ctx, guard, if_human, window, io.mark, &lease_gone);
                if !matches!(outcome, GuardedSubmit::Pressed | GuardedSubmit::Failed(_)) {
                    term_lock(term).watch_disarm(id);
                    // None of this Enter was written: one that was to be the turn's first write
                    // (a text of control characters alone) leaves the turn unwritten, and its
                    // `id=` goes back, as a fence's skip gives a `key if=` its id back.
                    if first {
                        wrote.set(false);
                    }
                }
                match outcome {
                    GuardedSubmit::Pressed => {}
                    GuardedSubmit::Preempted => {
                        if press_no > 0 {
                            break;
                        }
                        return stopped("preempted", (typed_keys, pasted()));
                    }
                    GuardedSubmit::Person => {
                        if press_no > 0 {
                            break;
                        }
                        return typed_cut(term, "person", typed_keys, pasted(), turn_id);
                    }
                    GuardedSubmit::Missed => {
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
                        // A re-press the sink took none of wrote nothing, as a missed one: an
                        // earlier Enter went, so the verdict says `pressed=<n>`. A first one is
                        // its own reply, never `ERR busy`: where the text is typed, that head
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
            // The Enter is a write like any other: whatever the caller does now, the turn's later
            // hops, a re-press included, are past the main thread's hang-up gate.
            committed.get_or_insert_with(crate::control::CommittedInput::enter);
            let mut advanced = false;
            let mut composer_gone = false;
            // A press the screen showed but codex began no turn for: no submit, and no press more,
            // which would act on what the Enter opened (a popup's pick).
            let mut unclaimed = false;
            if let Some(turns) = base_turns {
                // The claim alone ends the wait and alone is the submit: a redraw (a popup) is no
                // turn codex began.
                let screen = std::cell::Cell::new(false);
                match Park::new(&sub, &exited_now, OnHangup::Finish, stretch).run(
                    window,
                    claim_look(term, id, typed_row, turns, &screen),
                    disarm_watch(term, id),
                ) {
                    Parked::Exited => return "ERR exited\n".to_string(),
                    Parked::Done(()) => submit_by = Some(SubmitBy::Claim),
                    Parked::Deadline | Parked::HungUp => unclaimed = screen.get(),
                }
            } else {
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
                        Parked::Done(PressLook::Started(block)) => {
                            submit_by = Some(SubmitBy::Block);
                            submit_block = block;
                            break;
                        }
                        Parked::Done(PressLook::Moved) => {
                            submit_by = Some(SubmitBy::Screen);
                            break;
                        }
                        Parked::Done(PressLook::Latched) => {
                            advanced = true;
                            let mut t = term_lock(term);
                            if let Some(key) = composer_key.as_deref() {
                                match composer_after_press(&t, key) {
                                    ComposerAfterPress::Left => {
                                        submit_by = Some(SubmitBy::Composer);
                                        break;
                                    }
                                    ComposerAfterPress::Gone => composer_gone = true,
                                    ComposerAfterPress::Holds => composer_gone = false,
                                    ComposerAfterPress::Unfinished => {}
                                }
                            } else if !submit_verify_block {
                                submit_by = Some(SubmitBy::Screen);
                                break;
                            } else if t.started_command_seq() > base_started.started {
                                submit_by = Some(SubmitBy::Block);
                                submit_block = base_started.block(&t);
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
                                    ComposerAfterPress::Left => {
                                        submit_by = Some(SubmitBy::Composer);
                                    }
                                    ComposerAfterPress::Gone => composer_gone = true,
                                    ComposerAfterPress::Holds => composer_gone = false,
                                    ComposerAfterPress::Unfinished => {}
                                }
                            } else if matches!(closed, Parked::HungUp) && submit_verify_block {
                                let t = term_lock(term);
                                if t.started_command_seq() > base_started.started {
                                    submit_by = Some(SubmitBy::Block);
                                    submit_block = base_started.block(&t);
                                }
                            }
                            break;
                        }
                    }
                }
            }
            if submit_by.is_none()
                && advanced
                && auto_mode
                && base_started.started == 0
                && composer_key.is_none()
            {
                submit_by = Some(SubmitBy::Screen);
            }
            if submit_by.is_none() && composer_gone {
                submit_by = Some(SubmitBy::Composer);
            }
            if submit_by.is_some() || unclaimed || Instant::now() >= deadline {
                break;
            }
        }
        unjudged = submit_by.is_none() && pressed > 0;
    }
    let submitted = submit_by.is_some();

    drop(committed);
    if let Some(mark) = io.mark {
        mark.seal_supervisor_input();
    }

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
    if let Some(by) = submit_by {
        use std::fmt::Write as _;
        let _ = write!(verdict, " submit_by={}", by.as_str());
        if by == SubmitBy::Block
            && let Some(block) = submit_block
        {
            let _ = write!(verdict, " block={block}");
        }
    }
    if typed {
        use std::fmt::Write as _;
        let _ = write!(verdict, " typed={typed_keys} pasted={pasted_graphemes}");
    }
    if let Some(ms) = yielded_ms {
        use std::fmt::Write as _;
        let _ = write!(verdict, " yielded_ms={ms}");
    }
    // An Enter no look proved taken is no proof the program left the line: a screen that showed
    // no press, a codex-alab between turns that began no turn, or a composer that still holds the
    // text when the turn's time runs out (its echo can come so late that the Enter goes with
    // little of its window left, and the program may yet take it). The Enters are named, so a
    // caller never types the text again over one that may have gone.
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

/// Whether the tab's agent is a codex-alab aterm has proven ([`crate::harness_host::admitted`]:
/// a proven self-claim, aterm's copy of one, or an ALab image leading the group), whose own
/// OSC 7501 report a `turn` reads as its submit ([`codex_began_a_turn`]).
fn proven_codex_alab(ctx: &SessionCtx) -> bool {
    match crate::harness_host::admitted(ctx) {
        Some(aterm_phase::Program::Codex) => true,
        Some(
            aterm_phase::Program::Claude
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

/// The wait for a Claude Code composer to show the text a `turn` typed. A text its program has not
/// read has no echo yet, so the echo's [`EchoWait::CAP`] runs again from each look that finds the
/// program's input unread and from the first look after it that finds none. The text counts as
/// unread until a look finds it read, so a read no look saw (one inside the echo's settle, which
/// looks at nothing) gets the cap from the first look after it. The wait never runs past a minute
/// after the typing ([`EchoWait::HANG`]), when a program that has not read the text is hung, nor
/// past the turn's deadline.
#[derive(Clone, Copy, Debug)]
struct EchoWait {
    /// Where the wait ends: the cap from the typing until the first look.
    until: std::time::Instant,
    hung_by: std::time::Instant,
    /// What the last look found, or no look yet.
    unread: bool,
}

impl EchoWait {
    const CAP: std::time::Duration =
        std::time::Duration::from_millis(aterm_types::control_verbs::TURN_ECHO_CAP_MS);
    const HANG: std::time::Duration = std::time::Duration::from_secs(60);

    /// The wait for the echo of a text whose typing ended at `typed`, in a turn that ends at
    /// `deadline`.
    fn after_typing(typed: std::time::Instant, deadline: std::time::Instant) -> Self {
        Self {
            until: deadline.min(typed + Self::CAP),
            hung_by: deadline.min(typed + Self::HANG),
            unread: true,
        }
    }

    /// Where the wait ends after a look at `now` that finds the program's input `unread`, or
    /// `None` once that has passed.
    fn look(&mut self, unread: bool, now: std::time::Instant) -> Option<std::time::Instant> {
        let restart = self.unread || unread;
        #[cfg(any(test, debug_assertions))]
        let restart = restart && !aterm_types::negative_control::on("echo-from-typing");
        if restart {
            self.until = self.hung_by.min(now + Self::CAP);
        }
        self.unread = unread;
        (now < self.until).then_some(self.until)
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
                if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(usage());
                }
                page.since = Some(v.parse::<u64>().map_err(|_| usage())?);
            } else if !tok.is_empty()
                && tok.bytes().all(|b| b.is_ascii_digit())
                && let Ok(v) = tok.parse::<usize>()
            {
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
    cmd_meta_supervisor_serving(
        ctx,
        scope,
        conn,
        prove,
        rest,
        crate::harness_claim::served().is_some(),
    )
}

/// [`cmd_meta_supervisor`] by an aterm that serves contract 3 (`serves`) or answers as one
/// predating it (the negative control's seam, [`crate::harness_claim::served`]): then a claim
/// naming `contract=` is the v2 grammar's usage error, and nothing is claimed.
pub(crate) fn cmd_meta_supervisor_serving(
    ctx: &SessionCtx,
    scope: Scope,
    conn: Option<u64>,
    prove: &dyn Fn(&str) -> Option<crate::self_claim::Proven>,
    rest: &str,
    serves: bool,
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
            Some(h) => crate::session_timeline::publish_release_supervisor_held_by(ctx, h, now, by),
            None => crate::session_timeline::publish_release_supervisor(ctx, None, now, by),
        }
        .acknowledge();
        return Some(("OK\n".to_string(), changed));
    }
    let Some(holder) = toks.next() else {
        return Some((USAGE.to_string(), false));
    };
    if holder == "-" || !crate::session_timeline::valid_owner_token(holder) {
        return Some((USAGE.to_string(), false));
    }
    let words: Vec<&str> = toks.collect();
    if crate::harness_claim::refused_before_v3(serves, &words) {
        aterm_log::info!(
            "harness @{}: {holder} claimed under contract=, which this aterm does not serve ({}); \
             refused as the v2 grammar's usage error",
            ctx.self_id,
            crate::harness_claim::NO_CONTRACT3_SEAM
        );
        return Some((USAGE.to_string(), false));
    }
    let words = match ctx.native_policy.read(holder, &words) {
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
                crate::session_timeline::publish_claim_supervisor_as(
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
            let claimed = crate::session_timeline::publish_claim_supervisor_scoped(
                ctx, holder, conn, scoped, now, by,
            );
            if claimed.as_ref().is_ok_and(|c| c.published_ref().changed) {
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
    let claimed = crate::session_timeline::publish_claim_supervisor_as(
        ctx, holder, bound, expires_us, proven, now, by,
    );
    Some(claim_reply(ctx, bound, claimed, proof))
}

/// A claim's reply: `OK` (noting a connection-bound claim, and a proven self-claim's record
/// copy), or `ERR busy supervisor=<holder>`.
fn claim_reply(
    ctx: &SessionCtx,
    bound: Option<u64>,
    claimed: Result<
        crate::session_timeline::SupervisorTransition<crate::session_timeline::Claimed>,
        String,
    >,
    proof: Option<crate::self_claim::Proven>,
) -> (String, bool) {
    match claimed.map(crate::session_timeline::SupervisorTransition::acknowledge) {
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

/// `meta set harness_act <act> [until_us=<n>]` — an agent that supervises itself records an act it took on the session, so who is driving a session is seen per session. AN ADMITTED ACT IS THE HOLDER'S, SHOWN AND BOOKED AS ITS OWN: queued for the window's glass (`SessionTimeline::queue_self_act`: the chip's act and the trail, worded as the holder's) and booked in the session's ledger once the session's locks are let go (`crate::self_claim::book_act`), which Settings ▸ Harness counts under the holder and `aterm harness ledger` lists; a standing face's act told again (`SessionTimeline::self_act_resent`) is answered as before and neither shown nor booked twice. A refusal (`ERR denied`, an expired or busy act) and a lease (`meta prepare harness_act`) do neither.
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
    let resent;
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
            // A submitted TUI turn may already have written its begin before core accepts it.
            // Neither standing face may count that begin into its own mark. The unbounded
            // spelling retains its legacy semantics; the bounded form reads this fresh snapshot
            // under the act barrier, so an earlier working report refuses the publication.
            if crate::harness_task::self_claim_act_face(act)
                .is_some_and(crate::harness_task::holds_until_another_turn)
                && !snapshot.is_some_and(|point| point.left_at_rest)
            {
                return Some(("ERR busy harness_act\n".to_string(), false));
            }
        }
        let face = crate::harness_task::self_claim_act_face(act);
        resent = timeline.self_act_resent(
            act,
            &holder,
            face,
            snapshot.map(|point| point.mark.program_turns),
        );
        timeline.note_harness_act(act, &holder, by);
        if !resent {
            timeline.queue_self_act(crate::session_timeline::SelfAct {
                act: aterm_agent::harness::acts::self_claim_sort(act).act,
                holder: holder.clone(),
            });
        }
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
    drop(meta);
    if !resent {
        crate::self_claim::book_act(ctx.self_id.as_str(), &holder, act);
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

/// Whether `meta <rest>` is a well-formed `set harness_act <act>`: an admitted one queued its act
/// for the window's glass (`HarnessFact::SelfAct`).
pub(crate) fn is_harness_act_set(rest: &str) -> bool {
    matches!(
        harness_act_request(rest),
        Some(Ok(HarnessActRequest::Set { .. }))
    )
}

/// Whether `meta <rest>` is a `set harness_act` that reads the session's counts ([`HarnessActSnapshot`], under `crate::act_barrier`): a face that stands until another turn (its mark and, when leased, its rest check) or an opening act that types a turn ([`aterm_agent::harness::acts::self_claim_types_turn`]). Any other act reads none and waits for no barrier.
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

const HARNESS_HELD_USAGE: &str =
    "ERR usage: meta set harness_held same-reply|no-work | meta unset harness_held\n";

/// The stop a `meta set harness_held <arm>` tells (`Some`), or a `meta unset harness_held`'s lift
/// (`None`); `None` outright for every other `meta` line.
fn harness_held_request(
    rest: &str,
) -> Option<Result<Option<aterm_agent::harness::acts::HeldBy>, &'static str>> {
    let mut toks = rest.split_whitespace();
    let sub = toks.next()?;
    if !matches!(sub, "set" | "unset") || toks.next() != Some("harness_held") {
        return None;
    }
    let held = match sub {
        "set" => toks
            .next()
            .and_then(aterm_agent::harness::acts::HeldBy::of_word)
            .map(Some),
        _ => Some(None),
    };
    Some(
        held.filter(|_| toks.next().is_none())
            .ok_or(HARNESS_HELD_USAGE),
    )
}

/// `meta set harness_held <same-reply|no-work>` / `meta unset harness_held` — an agent that supervises itself tells that ITS OWN TYPING STOPPED, by which arm of ruling F's hold ([`aterm_agent::harness::acts::HeldBy::word`]), or goes on again, so `status held=` says its stop as it says aterm's own. THE STOP IS ITS CLAIM'S: admitted only from the session's standing proven self-claim ([`self_claim_write`]: by the proof `meta set harness_act` takes, that claim's connection or a connection its very process proves), else `ERR denied`; kept with that claim's proof ([`crate::session_timeline::SessionMeta::self_held`]) and gone wherever a wait written under it lapses (the claim released, replaced or gone with its process, its connection closed), so a new claim starts with none; said while the claim stands ([`crate::App::shown_stop`]). `OK` whether or not it moved; only a move is a change, which wakes the window.
pub(crate) fn cmd_meta_harness_held(
    ctx: &SessionCtx,
    conn: Option<u64>,
    prove: &dyn Fn(&str) -> Option<crate::self_claim::Proven>,
    rest: &str,
) -> Option<(String, bool)> {
    let held = match harness_held_request(rest)? {
        Ok(held) => held,
        Err(usage) => return Some((usage.to_string(), false)),
    };
    let Some((holder, moved)) = self_claim_write(ctx, conn, prove, |meta, proven| {
        meta.tell_self_held(proven, held)
    }) else {
        return Some(("ERR denied\n".to_string(), false));
    };
    if moved {
        aterm_log::info!(
            "self-claim @{}: {holder} told its stop: {}",
            ctx.self_id.as_str(),
            held.map_or("-", aterm_agent::harness::acts::HeldBy::word)
        );
    }
    Some(("OK\n".to_string(), moved))
}

/// Claim-scoped continuation observation. It never grants a turn or changes a hold.
pub(crate) fn cmd_meta_harness_activity(
    ctx: &SessionCtx,
    conn: Option<u64>,
    prove: &dyn Fn(&str) -> Option<crate::self_claim::Proven>,
    rest: &str,
) -> Option<(String, bool)> {
    let mut words = rest.split_whitespace();
    if words.next() != Some("set") || words.next() != Some("harness_activity") {
        return None;
    }
    let reason = words.next().and_then(|w| w.strip_prefix("reason="));
    let wait = words.next().and_then(|w| w.strip_prefix("wait_ms="));
    let sequence = words
        .next()
        .and_then(|w| w.strip_prefix("seq="))
        .and_then(|seq| seq.parse::<u64>().ok());
    let valid = reason.is_some_and(|r| {
        matches!(
            r,
            "person"
                | "composing"
                | "dialog"
                | "paused"
                | "working"
                | "ready"
                | "retry"
                | "waiting"
        )
    });
    let wait_ms = match wait {
        Some("-") => Some(None),
        Some(w) => w.parse::<u64>().ok().map(Some),
        None => None,
    };
    if !valid || wait_ms.is_none() || sequence.is_none() || words.next().is_some() {
        return Some((
            "ERR usage: meta set harness_activity reason=<state> wait_ms=<ms|-> seq=<n>\n".into(),
            false,
        ));
    }
    let now = aterm_time::unix_millis().unwrap_or(0);
    let Some((_, changed)) = self_claim_write(ctx, conn, prove, |meta, _| {
        let activity = crate::autopilot_activity::Activity {
            sequence: sequence.unwrap_or(0),
            reason: reason.unwrap_or("waiting").into(),
            until_ms: wait_ms.flatten().map(|ms| now.saturating_add(ms)),
            observed_ms: now,
        };
        let revision = meta.supervisor_revision;
        if meta
            .continuation_activity
            .as_ref()
            .is_some_and(|(rev, old)| *rev == revision && old.sequence >= activity.sequence)
        {
            return false;
        }
        let changed = meta
            .continuation_activity
            .as_ref()
            .is_none_or(|(rev, old)| {
                *rev != revision
                    || old.reason != activity.reason
                    || old.until_ms != activity.until_ms
            });
        meta.continuation_activity = Some((revision, activity));
        changed
    }) else {
        return Some(("ERR denied\n".into(), false));
    };
    Some(("OK\n".into(), changed))
}

/// A WRITE ONLY THE SESSION'S STANDING PROVEN SELF-CLAIM MAKES (`meta set harness_held`, `meta set harness_land`, `meta set harness_needs`, `harness fact`): admitted by the proof `meta set harness_act` takes — the line came on that claim's connection, or `prove` names the very proof of the claim's process — and `write` runs under the meta lock with the claim's proof only where neither [`crate::session_timeline::SessionMeta::supervisor_revision`] nor the live claim's proof moved since the look (a claim that moved between the look and the write admits nothing). The holder and what `write` returned (for a `meta set`, whether it moved what it keeps); `None` for anything else, which the verb answers `ERR denied`.
pub(crate) fn self_claim_write<T>(
    ctx: &SessionCtx,
    conn: Option<u64>,
    prove: &dyn Fn(&str) -> Option<crate::self_claim::Proven>,
    write: impl FnOnce(&mut crate::session_timeline::SessionMeta, &crate::self_claim::Proven) -> T,
) -> Option<(String, T)> {
    let standing = {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        meta.live_claim(crate::metrics::now_us()).and_then(|claim| {
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
    let (holder, proven, _, revision) = standing
        .filter(|(holder, proven, bound, _)| *bound || prove(holder).as_ref() == Some(proven))?;
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    if meta.supervisor_revision != revision
        || meta
            .live_claim(crate::metrics::now_us())
            .and_then(|claim| claim.proven.as_ref())
            != Some(&proven)
    {
        return None;
    }
    let moved = write(&mut meta, &proven);
    Some((holder, moved))
}

const HARNESS_LAND_USAGE: &str = "ERR usage: meta set harness_land since=<ms> nudges=<n> \
     [read=1 legs=\u{2026} open=\u{2026} changed=\u{2026} untracked=\u{2026} stashes=\u{2026} \
     unpushed=\u{2026} unlanded=\u{2026} left=\u{2026}] | meta unset harness_land\n";

/// The keys of a `meta set harness_land` line, each at most once: the two every line carries, then `read`'s group.
const HARNESS_LAND_KEYS: [&str; 11] = [
    "since",
    "nudges",
    "read",
    "legs",
    "open",
    "changed",
    "untracked",
    "stashes",
    "unpushed",
    "unlanded",
    "left",
];

/// The Land hold a `meta set harness_land …` tells (`Some`), or a `meta unset harness_land`'s lift (`None`); `None` outright for every other `meta` line, and the usage for a malformed one ([`land_held_of`]; an unset with any token after it).
fn harness_land_request(
    rest: &str,
) -> Option<Result<Option<aterm_agent::harness::acts::LandHeld>, &'static str>> {
    let mut toks = rest.split_whitespace();
    let sub = toks.next()?;
    if !matches!(sub, "set" | "unset") || toks.next() != Some("harness_land") {
        return None;
    }
    let held = match sub {
        "set" => land_held_of(toks).map(Some),
        _ => toks.next().is_none().then_some(None),
    };
    Some(held.ok_or(HARNESS_LAND_USAGE))
}

/// THE HOLD A `meta set harness_land` LINE'S TOKENS TELL, read strictly: each `key=value` of [`HARNESS_LAND_KEYS`] at most once, in any order; `since=` (unix ms, ASCII digits) and `nudges=` on every line; `read=1` with `legs=` (each [`aterm_agent::harness::acts::HoldLeg::word`], comma-separated) and neither without the other; `open=`, the five repository counts and `left=` (the processes its agent left running, unkept: [`aterm_agent::harness::acts::HoldLeg::Left`]) only beside `read=1`. A count is ASCII digits, held at its field's largest value past it. `open=`'s words are percent-encoded and comma-separated, and only the first four [`aterm_agent::harness::acts::OpenWord::of`] knows are kept, the rest dropped, as the strict reading keeps them. `None` for an unknown token, a token twice, a leg word no leg has, a count or a `since` that is no number, a `read=` other than `1`, a missing `since=` or `nudges=`, `read=1` without `legs=` or `legs=` without it, and `open=` or a count with no `read=1`.
fn land_held_of<'a>(
    toks: impl Iterator<Item = &'a str>,
) -> Option<aterm_agent::harness::acts::LandHeld> {
    use aterm_agent::harness::acts::{HoldLeg, LandHeld, OpenWord};
    let digits = |value: &str| !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit());
    let count = |value: &str| digits(value).then(|| value.parse::<u64>().unwrap_or(u64::MAX));
    let short = |value: &str| count(value).map(|n| u16::try_from(n).unwrap_or(u16::MAX));
    let mut held = LandHeld::default();
    let mut seen = [false; HARNESS_LAND_KEYS.len()];
    for tok in toks {
        let (key, value) = tok.split_once('=')?;
        let at = HARNESS_LAND_KEYS.iter().position(|known| *known == key)?;
        if std::mem::replace(&mut seen[at], true) {
            return None;
        }
        match key {
            "since" => held.since_ms = Some(value.parse::<i64>().ok().filter(|_| digits(value))?),
            "nudges" => held.nudges = count(value).map(|n| u32::try_from(n).unwrap_or(u32::MAX))?,
            "read" if value == "1" => held.read = true,
            "read" => return None,
            "legs" => {
                for word in value.split(',') {
                    held.legs |= HoldLeg::of_word(word)?.bit();
                }
            }
            "open" => {
                let known = value
                    .split(',')
                    .filter_map(|word| OpenWord::of(&aterm_control::wire::pct_decode(word)));
                for (slot, word) in held.open.iter_mut().zip(known) {
                    *slot = Some(word);
                }
            }
            "changed" => held.changed = short(value)?,
            "untracked" => held.untracked = short(value)?,
            "stashes" => held.stashes = short(value)?,
            "unpushed" => held.unpushed = short(value)?,
            "left" => held.left = short(value)?,
            _ => held.unlanded = short(value)?,
        }
    }
    let [since, nudges, read, legs, rest @ ..] = seen;
    (since && nudges && read == legs && (read || !rest.contains(&true))).then_some(held)
}

/// `meta set harness_land since=<ms> nudges=<n> [read=1 legs=… [open=…] [changed=… untracked=… stashes=… unpushed=… unlanded=…] [left=…]]` / `meta unset harness_land` — an agent that supervises itself tells WHY ITS OWN LAND STILL HOLDS, as aterm's loop tells its own ([`aterm_agent::harness::acts::LandHeld`], read by [`land_held_of`]), or that no landing holds, so `status land_held= land_nudges= land_since=`, the menu bar's `landing` and every hold line say its hold as they say aterm's own. THE HOLD IS ITS CLAIM'S, as `meta set harness_held`'s stop is ([`self_claim_write`], else `ERR denied`): kept with that claim's proof ([`crate::session_timeline::SessionMeta::self_land`]), NEVER on aterm's own glass (which stays cleared beside a proven self-claim), and gone wherever that stop goes; shown while the claim stands ([`crate::App::shown_hold`]). `OK` whether or not it moved; only a move is a change, which wakes the window.
pub(crate) fn cmd_meta_harness_land(
    ctx: &SessionCtx,
    conn: Option<u64>,
    prove: &dyn Fn(&str) -> Option<crate::self_claim::Proven>,
    rest: &str,
) -> Option<(String, bool)> {
    let held = match harness_land_request(rest)? {
        Ok(held) => held,
        Err(usage) => return Some((usage.to_string(), false)),
    };
    let Some((holder, moved)) = self_claim_write(ctx, conn, prove, |meta, proven| {
        meta.tell_self_land(proven, held)
    }) else {
        return Some(("ERR denied\n".to_string(), false));
    };
    if moved {
        aterm_log::info!(
            "self-claim @{}: {holder} told its Land hold:{}",
            ctx.self_id.as_str(),
            crate::session_status::land_fields(held)
        );
    }
    Some(("OK\n".to_string(), moved))
}

const HARNESS_NEEDS_USAGE: &str = "ERR usage: meta set harness_needs entry=<pct text> \
     need=<pct need> [need=<pct need> \u{2026}] | meta unset harness_needs\n";

/// The most needs one `meta set harness_needs` line tells: the first 32 of the worker's set, in its order.
pub(crate) const HARNESS_NEEDS_MAX: usize = 32;

/// The NEEDS YOU set a `meta set harness_needs …` tells (`Some`), or a `meta unset harness_needs`'s take-down (`None`); `None` outright for every other `meta` line, and the usage for a malformed one ([`harness_needs_of`]; an unset with any token after it).
fn harness_needs_request(
    rest: &str,
) -> Option<Result<Option<Arc<crate::needs::HarnessNeeds>>, &'static str>> {
    let mut toks = rest.split_whitespace();
    let sub = toks.next()?;
    if !matches!(sub, "set" | "unset") || toks.next() != Some("harness_needs") {
        return None;
    }
    let told = match sub {
        "set" => harness_needs_of(toks).map(|told| Some(Arc::new(told))),
        _ => toks.next().is_none().then_some(None),
    };
    Some(told.ok_or(HARNESS_NEEDS_USAGE))
}

/// Whether `meta <rest>` is a `set` or `unset harness_needs` line: an admitted one that moved what a reader reads is a needs fact for the window (`HarnessFact::Needs`), as aterm's own loop's tell is.
pub(crate) fn is_harness_needs(rest: &str) -> bool {
    harness_needs_request(rest).is_some()
}

/// THE SET A `meta set harness_needs` LINE'S TOKENS TELL, read strictly: `entry=` exactly once (the text of the `owner=needs` entry the set describes) and `need=` one to [`HARNESS_NEEDS_MAX`] times, in the worker's order, each value percent-decoded strictly ([`pct_strict`]), a need at most [`crate::needs::NEED_TOLD_MAX`] bytes and non-empty once folded as every reader folds it ([`crate::needs::fold_clip`], which drops whitespace, control and forbidden format characters); the entry's text kept through the presentation sanitizer ([`crate::needs::HarnessNeeds::told`]). `None` for an unknown token, no `entry=`, `entry=` twice or empty (nothing left once sanitized included), no `need=`, a 33rd, an empty need (nothing left once folded included) or one past `NEED_TOLD_MAX`, a bad percent escape and an escape that decodes to no UTF-8.
fn harness_needs_of<'a>(toks: impl Iterator<Item = &'a str>) -> Option<crate::needs::HarnessNeeds> {
    let mut entry = None;
    let mut needs = Vec::new();
    for tok in toks {
        match tok.split_once('=')? {
            ("entry", value) if entry.is_none() => {
                entry = Some(pct_strict(value).filter(|text| !text.is_empty())?);
            }
            ("need", value) if needs.len() < HARNESS_NEEDS_MAX => {
                needs.push(pct_strict(value).filter(|need| {
                    need.len() <= crate::needs::NEED_TOLD_MAX
                        && !crate::needs::fold_clip(need, crate::needs::NEED_TOLD_MAX).is_empty()
                })?)
            }
            _ => return None,
        }
    }
    crate::needs::HarnessNeeds::told(&needs, &entry?)
}

/// `value` PERCENT-DECODED STRICTLY: every `%` begins two hex digits (either case), any other byte stands for itself, and the bytes decoded are UTF-8; `None` for anything else, where [`aterm_control::wire::pct_decode`] passes a stray `%` through and decodes bad UTF-8 lossily.
fn pct_strict(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        if byte == b'%' {
            let hex = |at: usize| bytes.get(at).and_then(|b| char::from(*b).to_digit(16));
            let (hi, lo) = (hex(at + 1)?, hex(at + 2)?);
            out.push(u8::try_from((hi << 4) | lo).ok()?);
            at += 3;
        } else {
            out.push(byte);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `meta set harness_needs entry=<pct text> need=<pct need> [need=<pct need> …]` / `meta unset harness_needs` — an agent that supervises itself tells EVERY NEED ITS `owner=needs` ENTRY STANDS FOR, whole and in its order ([`harness_needs_of`]), or that the entry went, so the menu bar's rows one level in, the hover's lines and `status needs_told=` say its set as they say aterm's own loop's, where its entry alone gives the first need and a count. THE SET IS ITS CLAIM'S, as `meta set harness_held`'s stop is ([`self_claim_write`], else `ERR denied`): told as aterm's own loop's is (`HarnessNeeds::told` then `ToldNeeds::tell` beside the session's `owner=needs` entry as stored now, [`crate::session_timeline::SessionMeta::tell_self_needs`]), so a set told before its entry stands waits for it and one told for another entry's text is never read; kept with that claim's proof, NEVER in the timeline's set (aterm's own loop's), and gone wherever that stop goes; read while the claim stands ([`crate::needs::told_of`]). `OK` whether or not it moved; only a move of what a reader reads beside the standing entry is a change, which wakes the window.
pub(crate) fn cmd_meta_harness_needs(
    ctx: &SessionCtx,
    conn: Option<u64>,
    prove: &dyn Fn(&str) -> Option<crate::self_claim::Proven>,
    rest: &str,
) -> Option<(String, bool)> {
    let told = match harness_needs_request(rest)? {
        Ok(told) => told,
        Err(usage) => return Some((usage.to_string(), false)),
    };
    let count = told.as_ref().map(|told| told.needs().len());
    let Some((holder, moved)) = self_claim_write(ctx, conn, prove, |meta, proven| {
        meta.tell_self_needs(proven, told)
    }) else {
        return Some(("ERR denied\n".to_string(), false));
    };
    if moved {
        aterm_log::info!(
            "self-claim @{}: {holder} told its NEEDS YOU set: needs={}",
            ctx.self_id.as_str(),
            count.map_or_else(|| "-".to_string(), |n| n.to_string())
        );
    }
    Some(("OK\n".to_string(), moved))
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
    let (autopilot_by, autopilot_since) = autopilot_by_fields(&meta);
    format!(
        "OK title={} user_title={} description={} icon={} role={} attention={} cwd={} state={state} \
         attention_owner={} attention_owners={} attention_kinds={} supervisor={} supervisor_proven={} \
         questions={} agent_cwd={} trusted_cwd={} worker={} autopilot={} land_at={} \
         autopilot_by={} autopilot_since={}\n",
        pct_encode(&title),
        meta_value(meta.user_title.as_deref()),
        meta_value(meta.description.as_deref()),
        meta_value(meta.icon.as_deref()),
        meta_value(meta.role.as_deref()),
        meta_value(meta.attention.as_deref()),
        meta_value(cwd.as_deref()),
        meta_value(attention_owner(&meta)),
        attention_owners(&meta),
        attention_kinds(&meta),
        meta_value(meta.live_supervisor(crate::metrics::now_us())),
        u8::from(meta.claim_proven(crate::metrics::now_us())),
        meta_value(meta.questions.as_deref()),
        meta_value(agent_cwd.as_deref()),
        meta_value(trusted_cwd.as_deref()),
        worker.map_or("-", aterm_phase::Program::name),
        meta_value(meta.autopilot.as_deref()),
        crate::autopilot::land_at_field(meta.land_at),
        autopilot_by,
        autopilot_since,
    )
}

/// A `meta` field's value as `meta` prints it: `-` for none, `%2D` for a stored `-` (so it never
/// reads as none), anything else percent-encoded.
pub(crate) fn meta_value(v: Option<&str>) -> String {
    match v {
        None => "-".to_string(),
        Some("-") => "%2D".to_string(),
        Some(v) => pct_encode(v),
    }
}

/// THE PRESENCE ROW'S MEANING FIELDS, as `meta` prints them (critic item E05):
/// `role=<pct|-> attention=<pct|-> user_title=<pct|->`, the three the Fabric bridge folds into a
/// session's presence ([`meta_value`] each). Every `sessions status` row carries them, and so does
/// an `events` subscription's `sub` ack, so the bridge's attach asks no `@<sid> meta`.
pub(crate) fn presence_meta_tokens(meta: &crate::session_timeline::SessionMeta) -> String {
    format!(
        "role={} attention={} user_title={}",
        meta_value(meta.role.as_deref()),
        meta_value(meta.attention.as_deref()),
        meta_value(meta.user_title.as_deref()),
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
    let events =
        crate::cast_scrub::scrubbed(&events, header.evicted == 0 && header.handoff.is_none());
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
        match tok
            .strip_prefix("count=")
            .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|v| v.parse::<usize>().ok())
        {
            Some(n) if n >= 1 => count = n.min(240),
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

/// How long a `cast drift` waits for a free slot before it answers busy. A unit test's holders
/// are its sibling tests, which finish, so its callers wait the hang detector.
#[cfg(not(test))]
const CAST_DRIFT_SLOT_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
#[cfg(test)]
const CAST_DRIFT_SLOT_WAIT: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;

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

/// `temporal [tick] [trim]` -> `OK <nbytes>[ trimmed=<k>]\n` then the session's screen RECONSTRUCTED at logical `tick` (default: the latest recorded instant) — with `trim`, minus its trailing all-blank rows (the `text trim` rule; the byte count on the header is the trimmed body's) — the read half of the hydratable temporal spine (`docs/design/HIERARCHICAL_SESSIONS.md` Addendum B), the endpoint the `TemporalRecorder` producer feeds; the fold posts its verdicts on the pending witnesses it passes ([`crate::temporal::TemporalRecorder::judge`]). `temporal judge` -> `OK judged=<n> diverged=<n> pending=<n> unjudged_evicted=<n>\n`: every kept chain's pending witnesses judged ([`crate::temporal::judge_all`]), the first two counted in this call, the last two read after it.
pub(crate) fn cmd_temporal(ctx: &SessionCtx, term: &Mutex<Terminal>, rest: &str) -> String {
    const USAGE: &str = "ERR usage: temporal [status | judge | <tick>] [trim]  (tick = µs since \
                         session start; `temporal status` reports the reachable range; \
                         `temporal judge` judges every pending witness)\n";
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
             dropped_replies={} pending_resizes={} witnessed={} diverged={} pending={} \
             unjudged_evicted={} pool_used={} pool_limit={}{}{}\n",
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
            rec.pending(),
            rec.unjudged_evicted(),
            crate::temporal::pool().used(),
            crate::temporal::pool().limit(),
            rec.status_tail(),
            rec.writer_status(),
        );
    }
    if rest == "judge" {
        if trim {
            return USAGE.to_string();
        }
        let _consumer = crate::temporal::consumer_demand();
        let counts = crate::temporal::judge_all(&ctx.temporal);
        return format!(
            "OK judged={} diverged={} pending={} unjudged_evicted={}\n",
            counts.judged, counts.diverged, counts.pending, counts.unjudged_evicted
        );
    }
    let at = match rest {
        "" => None,
        s => {
            if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
                return USAGE.to_string();
            }
            match s.parse::<u64>() {
                Ok(t) => Some(aterm_buffer::Ticks(t)),
                Err(_) => return USAGE.to_string(),
            }
        }
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
    let _consumer = crate::temporal::consumer_demand();
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

/// `exits [<n>] [since=<id>]` -> the instance's exit ledger, oldest-first, framed `OK <count>\n` + one `exit <id> t=<ms> sid=<sid> local=<n> reason=<…> exit_code=<n|-> by=<sid|human|ctl|bridge|harness|-> left=<n|->` line per session that left the registry — the `history`/`timeline` grammar (`<n>` keeps the last n; `since=<id>` keeps rows with id strictly greater) over the closed-session index, whose slots are linked in the write that records the roster's `Exited` row, in its seq order (the row id); bounded by the index's last 512 closes. `left=` is what the leftover view's record of that close (`closed`, [`exits_closed`]) says the tab left running that `end` takes, and `-` where the view keeps no record of it.
pub(crate) fn cmd_exits(
    store: &Store,
    rest: &str,
    closed: &[aterm_vitals::left::ClosedTab],
) -> String {
    let page = match RingPage::parse(rest, "exits") {
        Ok(page) => page,
        Err(usage) => return usage,
    };
    let rows = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        page.copy(g.closed_logs().exits_since(page.since.unwrap_or(0)))
    };
    exits_reply(&rows, closed)
}

/// The closed tabs the newest held leftover view keeps a record of (in memory, no read):
/// `exits`' `left=`.
pub(crate) fn exits_closed() -> Vec<aterm_vitals::left::ClosedTab> {
    crate::sys_host::SYS
        .held_now()
        .and_then(|held| held.snap.left.clone())
        .map(|view| view.closed.clone())
        .unwrap_or_default()
}

fn exits_reply(
    rows: &[crate::session_log::ClosedExit],
    closed: &[aterm_vitals::left::ClosedTab],
) -> String {
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
        let left = closed
            .iter()
            .rev()
            .find(|t| t.sid == r.sid.as_str())
            .map_or_else(|| "-".to_string(), |t| t.left.to_string());
        let _ = writeln!(
            out,
            "exit {} t={} sid={} local={} reason={} exit_code={exit_code} by={} left={left}",
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

/// The exit ledger's `by=` for a control-socket `close`: [`caller_actor_of`] with its session.
pub(crate) fn caller_actor(scope: Scope, target: &SessionCtx, store: &Store) -> ExitActor {
    caller_actor_of(scope, Some(target), store)
}

/// WHO ASKS on this connection, as the exit ledger and the tab marks name an act's actor (a
/// close's, a spawn's, a `tab` or `invoke` gesture's): a drive from another Mac its proven node
/// and its requester's session (`fleet:<head>:<sid>`, [`serving_drive`]); aterm's own harness
/// token `harness`; an owner-token client run from a tab that tab's session, found by the
/// connection's peer process and its ancestors
/// ([`owner_tab_sid`]), which is how a manager running `aterm ctl` in its own tab is named; the
/// fabric bridge `bridge`; an edge-scoped caller its own sid by `target`'s grant table; `ctl`
/// for an owner-token client in no tab, and unknown otherwise.
pub(crate) fn caller_actor_of(
    scope: Scope,
    target: Option<&SessionCtx>,
    store: &Store,
) -> ExitActor {
    caller_actor_as(scope, target, store, || {
        crate::typed_by::Who::client(crate::control::serving_peer_pid())
    })
}

/// [`caller_actor_of`], `client` naming the owner-token connection's process (read only for
/// such a connection): the connection's peer, or a test's process and lineage.
pub(crate) fn caller_actor_as(
    scope: Scope,
    target: Option<&SessionCtx>,
    store: &Store,
    client: impl FnOnce() -> crate::typed_by::Who,
) -> ExitActor {
    if let Some((node, sid)) = serving_drive() {
        return ExitActor::fleet_drive(&node, sid.as_deref());
    }
    match scope {
        Scope::Owner => match crate::control::serving_principal() {
            crate::control::Principal::Harness => ExitActor::Harness,
            crate::control::Principal::Instance => {
                owner_tab_sid(&client(), store).map_or(ExitActor::Ctl, ExitActor::Sid)
            }
        },
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

/// The sid of the live session whose shell is `who`'s process or an ancestor of it
/// (`crate::typed_by::tab_session_of`), over the sessions `store` registers now.
pub(crate) fn owner_tab_sid(who: &crate::typed_by::Who, store: &Store) -> Option<String> {
    let shells: Vec<(u64, i32, String)> = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .live_handles()
        .map(|h| (h.local_id, h.shell_identity.pid, h.sid.as_str().to_string()))
        .collect();
    let pids: Vec<(u64, i32)> = shells
        .iter()
        .map(|(local, pid, _)| (*local, *pid))
        .collect();
    let local = crate::typed_by::tab_session_of(who, &pids)?;
    shells
        .into_iter()
        .find(|(of, ..)| *of == local)
        .map(|(.., sid)| sid)
}

/// The turn ledger's `by=` for a `turn` from this connection ([`TurnIo::by`]), from the scope and the driver the dispatch already resolved from it (an edge's source in the target's table, `crate::control::turn_driver`): `fleet:<head>:<sid>` for a fleet drive's line ([`TurnBy::fleet`], [`serving_drive`]), `ctl` for any other owner-token client, `bridge` for the fabric bridge, an edge-scoped caller's own sid, `-` for an edge whose grant is not in the target's table. The exits ledger ([`caller_actor`]) names an edge, the bridge and an owner client in no tab in these same words, and a fleet drive the same `fleet:<head>:<sid>`; it names further what this ledger leaves `ctl` (an owner client run from a tab, as that tab's session; aterm's harness token), which the turn ledger's readers ([`typed_by`]) name their own way.
pub(crate) fn turn_by(scope: Scope, driver: Option<&SessionId>) -> TurnBy {
    // A fleet drive's admitted line is served as Owner (`crate::control::drive_admission`): the
    // Mac it serves for is the typist, never this Mac's owner token.
    if let Some((node, sid)) = serving_drive() {
        return TurnBy::fleet(&node, sid.as_deref());
    }
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

/// The proven node of the drive this connection serves and the session its requester line names
/// (`None` where it names none): what every record of the drive names (`fleet:<head>:<sid>`,
/// [`aterm_slog::Actor::fleet_drive`]). The session is the driving Mac's claim within that node,
/// attribution only: no gate reads it.
fn serving_drive() -> Option<(String, Option<String>)> {
    let driver = crate::fabric::serving_driver()?;
    let sid =
        crate::fabric::serving_requester().and_then(|requester| requester.sid().map(str::to_owned));
    Some((driver.node, sid))
}

/// The session log's `by=` for a request on `scope` against `target` (`docs/DESIGN-session-log-2026-10-15.md` §2.5): who the connection is, never a parameter. A fleet drive is `fleet:<head>:<sid>` ([`serving_drive`]), the word the turn ledger ([`turn_by`]) and the exits ledger ([`caller_actor`]) name it by.
pub(crate) fn log_actor(scope: Scope, target: &SessionCtx) -> aterm_slog::Actor {
    use aterm_slog::Actor;
    if let Some((node, sid)) = serving_drive() {
        return Actor::fleet_drive(&node, sid.as_deref());
    }
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

/// The generation [`needs_moved`] moves, which every needs pusher waits on ([`NEEDS_MOVED`]).
static NEEDS_GENERATION: Mutex<u64> = Mutex::new(0);

/// Woken each time [`NEEDS_GENERATION`] moves.
static NEEDS_MOVED: std::sync::Condvar = std::sync::Condvar::new();

/// How long a needs pusher waits for the generation to move before it builds its row again
/// anyway, writing it only where it changed: the backstop for a change `NeedAges::observe` did
/// not see.
#[cfg(unix)]
const NEEDS_BACKSTOP: std::time::Duration = std::time::Duration::from_secs(60);

/// How long a needs pusher waits for the node job's `go`, and each of its frames may take to
/// write: aterm-net's `HANDSHAKE_WITHIN`, as the node job's own hand-overs; a unit test's node
/// job is its own, which answers, so it waits the hang detector.
#[cfg(all(unix, not(test)))]
const NEEDS_HANDSHAKE: std::time::Duration = aterm_net::fleet::HANDSHAKE_WITHIN;
#[cfg(all(unix, test))]
const NEEDS_HANDSHAKE: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;

/// Say this window's needs may have moved (a session's published need, hold, wait, agent class
/// or task, as `NeedAges::observe` saw it move, `App::age_session_need`): every needs pusher
/// builds its row again and writes it where it changed. Never waits on a pusher.
pub(crate) fn needs_moved() {
    let mut generation = NEEDS_GENERATION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *generation = generation.wrapping_add(1);
    drop(generation);
    NEEDS_MOVED.notify_all();
}

/// The needs generation now.
#[cfg(unix)]
fn needs_generation() -> u64 {
    *NEEDS_GENERATION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The needs generation once it is no longer `seen`, or after `within` whatever it is.
#[cfg(unix)]
fn needs_generation_after(seen: u64, within: std::time::Duration) -> u64 {
    let generation = NEEDS_GENERATION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (generation, _) = NEEDS_MOVED
        .wait_timeout_while(generation, within, |now| *now == seen)
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *generation
}

/// This window's needs row at `now_unix_ms` (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.14),
/// read from each session in `store` that has not exited and whose sid is minted, as the survey
/// reads it (`survey::session_row`) and the Fleet menu reads a peer's survey (`fleet_glance`):
/// a need where its published verdict names one, `harness` where that need is a harness's own
/// ask and not the program's report, its manager the sid its task came from, and its since the
/// survey age the Fleet menu ages it by (`task_ms` for `stalled`, none for `ask` or for
/// `frozen` and `stopped` on a non-agent, else `agent_ms`) taken from now; a hold where one
/// stands, by the Mac that holds it where that is a node id; and each manager with how many of
/// these sessions' tasks came from it. Ids, words and times only, each list sorted by sid.
#[cfg(unix)]
pub(crate) fn needs_row(store: &Store, now_unix_ms: u64) -> aterm_metrics::row::NeedsRow {
    use aterm_metrics::row::{HeldEntry, ManagerEntry, NeedEntry, NeedsRow, Sid, Word};
    use aterm_types::needs::NeedWord;

    let snapshot = store.read().unwrap_or_else(|p| p.into_inner()).snapshot();
    let mut row = NeedsRow {
        at: now_unix_ms / 1000,
        ..NeedsRow::default()
    };
    let mut managers = std::collections::BTreeMap::<Sid, u32>::new();
    for h in snapshot
        .iter()
        .filter(|h| !matches!(h.state, SessionState::Exited))
    {
        let Some(sid) = Sid::parse(h.sid.as_str()) else {
            continue;
        };
        let (need, held, worker_of, task_since, is_agent, agent_ms) = {
            let timeline = h.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let agent = timeline.agent();
            // A reader that names no state (codex's) ages by what the program says itself.
            let agent_ms = match timeline.program_shown().filter(|_| agent.word == "unknown") {
                Some(shown) => shown.age_ms(),
                None => agent.age_ms(),
            };
            let verdict = timeline.need();
            let task = timeline.task();
            (
                verdict
                    .need
                    .as_ref()
                    .map(|need| (need.word, need.report.is_none() && need.harness.is_some())),
                verdict.held,
                task.and_then(|task| Sid::parse(&task.from)),
                task.map(|task| task.since_unix_ms),
                agent.reader.is_some(),
                agent_ms,
            )
        };
        if let Some(manager) = worker_of {
            let workers = managers.entry(manager).or_default();
            *workers = workers.saturating_add(1);
        }
        if let Some(hold) = held.and_then(|hold| Word::new(hold.word())) {
            let by = h.ctx.fabric.holds().remote().and_then(|remote| {
                aterm_metrics::subject::Node::parse(&remote.node.to_ascii_lowercase())
            });
            row.held.push(HeldEntry {
                sid,
                hold,
                by,
                since: None,
            });
        }
        let Some((word, harness)) = need else {
            continue;
        };
        let Some(need_word) = Word::new(word.word()) else {
            continue;
        };
        let age_ms = match word {
            NeedWord::Stalled => task_since.map(|since| now_unix_ms.saturating_sub(since)),
            NeedWord::Ask => None,
            NeedWord::Frozen | NeedWord::Stopped if !is_agent => None,
            _ => Some(agent_ms),
        };
        row.needs.push(NeedEntry {
            sid,
            word: need_word,
            harness,
            worker_of,
            since: age_ms.map(|age| now_unix_ms.saturating_sub(age) / 1000),
        });
    }
    row.needs.sort_by_key(|need| need.sid);
    row.held.sort_by_key(|held| held.sid);
    row.managers = managers
        .into_iter()
        .map(|(sid, workers)| ManagerEntry { sid, workers })
        .collect();
    row
}

/// Serve the node job's read of this window's needs on `stream`
/// (`node_serve::RelayHeader::Needs`) on a thread of its own ([`push_needs`]): `false` where no
/// control server runs here whose sessions it could read, or the thread did not start.
#[cfg(unix)]
pub(crate) fn attach_needs_stream(stream: aterm_uds::CtlStream) -> bool {
    let Some(context) = super::BRIDGE_CONTEXT.get().cloned() else {
        return false;
    };
    crate::qos::spawn_lane_thread(
        "aterm-needs-push".to_string(),
        crate::qos::Role::Background,
        move || push_needs(&context.store, stream),
    )
    .is_ok()
}

/// The needs row to write next: `store`'s at `now_unix_ms` ([`needs_row`]), each standing need's
/// since kept as `sent` said it ([`steady_since`]), where it says other than `sent`, the row last
/// written, `at` aside, which it then becomes; `None` where it says the same, whenever it was
/// written.
#[cfg(unix)]
fn changed_needs(
    store: &Store,
    now_unix_ms: u64,
    sent: &mut Option<aterm_metrics::row::NeedsRow>,
) -> Option<aterm_metrics::row::NeedsRow> {
    let mut row = needs_row(store, now_unix_ms);
    if let Some(last) = sent.as_ref() {
        steady_since(&mut row, last);
        let timeless =
            |r: &aterm_metrics::row::NeedsRow| aterm_metrics::row::NeedsRow { at: 0, ..r.clone() };
        if timeless(last) == timeless(&row) {
            return None;
        }
    }
    *sent = Some(row.clone());
    Some(row)
}

/// Keep each need's since as `last` said it where `last` lists the same session with the same
/// need and a since at most a second off: a since is now on the wall clock less an age on the
/// continuous one, cut to a second, so two builds of one standing need can differ by a second
/// with nothing changed.
#[cfg(unix)]
fn steady_since(row: &mut aterm_metrics::row::NeedsRow, last: &aterm_metrics::row::NeedsRow) {
    for need in &mut row.needs {
        let was = last
            .needs
            .iter()
            .find(|was| was.sid == need.sid && was.word == need.word)
            .and_then(|was| was.since);
        if let (Some(was), Some(now)) = (was, need.since)
            && was.abs_diff(now) <= 1
        {
            need.since = Some(was);
        }
    }
}

/// Once the node job writes `go` on `stream` (within [`NEEDS_HANDSHAKE`]), write it this window's
/// needs row ([`needs_row`] of `store`) as one frame (`node_serve::write_needs_frame`), and again
/// each time the row changes, `at` aside ([`changed_needs`]): the row is built again each time
/// [`needs_moved`] moves the generation, and at least every [`NEEDS_BACKSTOP`]. Ends, hanging up,
/// once the node job has: a write fails, or after a wake the stream reads its end (or anything
/// at all, which the node job never writes past `go`).
#[cfg(unix)]
fn push_needs(store: &Store, mut stream: aterm_uds::CtlStream) {
    use std::io::Read as _;

    use aterm_uds::hangup::{Awake, PeerState, hang_up, peer_state};

    let _ = stream.set_read_timeout(Some(NEEDS_HANDSHAKE));
    let mut go = [0u8; 3];
    if Awake(&stream).read_exact(&mut go).is_err() || &go != b"go\n" {
        hang_up(&stream);
        return;
    }
    let _ = stream.set_read_timeout(None);
    let _ = stream.set_write_timeout(Some(NEEDS_HANDSHAKE));
    let mut seen = needs_generation();
    let mut sent = None;
    loop {
        let now_unix_ms = aterm_time::unix_millis().unwrap_or(0);
        if let Some(row) = changed_needs(store, now_unix_ms, &mut sent)
            && crate::fleet_listen::node_serve::write_needs_frame(&mut stream, &row).is_err()
        {
            break;
        }
        seen = needs_generation_after(seen, NEEDS_BACKSTOP);
        if peer_state(&stream) != PeerState::Idle {
            break;
        }
    }
    hang_up(&stream);
}

#[cfg(test)]
#[path = "control_session_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "control_session_exits_tests.rs"]
mod exits_tests;

#[cfg(test)]
#[path = "control_session_log_actor_tests.rs"]
mod log_actor_tests;

#[cfg(test)]
#[path = "control_session_update_anchor_tests.rs"]
mod update_anchor_tests;

#[cfg(test)]
#[path = "control_session_connection_carry_tests.rs"]
mod connection_carry_tests;

#[cfg(test)]
#[path = "control_session_update_park_wait_tests.rs"]
mod update_park_wait_tests;

#[cfg(test)]
#[path = "control_session_park_tests.rs"]
mod park_tests;

#[cfg(test)]
#[path = "control_session_turn_halt_tests.rs"]
mod turn_halt_tests;

#[cfg(test)]
#[path = "control_session_composer_verify_tests.rs"]
mod composer_verify_tests;

#[cfg(test)]
#[path = "control_session_typing_pace_tests.rs"]
mod typing_pace_tests;

#[cfg(all(test, unix))]
#[path = "control_session_needs_tests.rs"]
mod needs_tests;
