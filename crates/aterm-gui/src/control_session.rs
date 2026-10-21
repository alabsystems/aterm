// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session-graph, drive and capability-authority verbs: the roster (`sessions`, `who`,
//! `family`, `raise`), the waits (`ready`, `await`), `lease` and `turn`, the ledgers and
//! recordings (`history`, `meta`, `timeline`, `cast`, `temporal`, `exits`), and the authority
//! graph (`edges`, `grant`, `revoke`, `connect`, `disconnect`, `flows`, `whoami`). `Scope` and
//! the cross-session auth stay in `control.rs`.

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
    AUTOPILOT_USAGE, MetaEdit, MetaField, MetaWriteError, apply_meta_value, questions_usage,
    write_session_meta,
};
use crate::turn_ledger::TurnBy;
use crate::{SessionCtx, term_lock};

/// `sessions` -> list the process-wide registry: `OK <n>\n` then one line per
/// session, sorted by local id: `<local> <sid> <parent|-> <state> <title> meta=<1|0>
/// window=<id|none|-> active=<1|0|-> wfocus=<1|0|-> detail=<pct|-> identity=<name|->
/// path=<frozen|live> program=<name|-> agent=<word|-> agent_detail=<pct|->
/// agent_rev=<n> agent_since_ms=<ms> … supervisor=<holder|-> autopilot=<off|cruise|land|->
/// path_evidence=<measured:<ms>|unconfirmed:<ms>|carried|-> copy=<managed|foreign|->
/// upgrade=<-|<state>/<to>/<why>/<age>> land_at=<unix s|-> task=<open|landed|done|spent|theirs|->
/// pause=<unix s|-> task_state=<working|done|exited|stalled|-> task_rev=<n>`.
///
/// The store snapshot is cloned out before formatting, so this never holds the
/// registry lock across a `Terminal` lock.
///
/// - `meta=1` iff any user metadata (`meta set title|description|icon|role|attention|questions|autopilot`)
///   is set, so a fleet driver knows which sessions to `@<sid> meta`.
/// - `window=`/`active=`/`wfocus=` come from one main-thread hop for the whole roster
///   ([`session_window_rows`]): the window the session is reported under (the `dims` rule),
///   whether it is that window's active tab, whether that window is front. `none` means no
///   window hosts it; `-` means the hop failed, and the line is still printed.
/// - `detail=` is the sanitized running command
///   ([`crate::session_status::executing_detail`]), `-` when idle or the lock is contended.
/// - `identity=` is the agent identity the session was spawned under (`spawn identity=`), `-`
///   for the human's own config or an adopted shell without the field.
/// - `path=<frozen|live>` says whether the shell fronts aterm's managed `agents/` on PATH:
///   measured from a program the shell started where it can be, else the registry's adoption
///   mark ([`crate::session_store::SessionStore::has_frozen_path`]); `path_evidence=` says which
///   ([`crate::session_timeline::SessionTimeline::owner_columns`]). `copy=` names the build an
///   agent in front runs (`managed` or `foreign`), `upgrade=` the live upgrade of its
///   conversation.
/// - `program=` … `agent_fp=` are the status sweep's publication on the session timeline, as
///   `status` prints them (`-` until the sweep has seen the session, `agent=-` when not an
///   identified agent).
/// - `autopilot=`, right after `supervisor=`, is the session's effective Autopilot when it runs
///   an agent the harness supervises (its own word, else `[harness] autopilot`, and `off` under
///   the master switch or with no host; [`crate::autopilot::wire_field`]), and `-` otherwise.
/// - `land_at= task=`, after the owner's columns, report the instant a timed Cruise lands (unix
///   seconds) and the task's face under Land while this window's host holds the live claim (or a
///   proven self-claim), `-` otherwise ([`crate::autopilot::land_at_field`],
///   [`crate::autopilot::task_field`]).
/// - `pause=`, after `task=`, is the unix second the loop's pause after short turns ends
///   ([`crate::autopilot::pause_field`]).
/// - `task_state= task_rev=`, last, report the session's task record ([`crate::task_record`]),
///   `-`/`0` when none, named apart from `task=` (the task's face under Land).
///
/// Instance state, answered with no terminal. `proxy` is `None` only where there
/// is no event loop to ask (a worker-thread test): the window columns then read
/// `-`, never a guess.
pub(crate) fn cmd_sessions_store(store: &Store, proxy: Option<&EventLoopProxy<Wake>>) -> String {
    // The frozen-path marks are read under the SAME guard as the snapshot, so a
    // session cannot be listed with a mark taken from a different registry state.
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
    // An empty roster skips the hop, so `OK 0` is answerable before the event loop is up.
    let rows = if snapshot.is_empty() {
        Ok(Vec::new())
    } else {
        session_window_rows(proxy)
    };
    // A failed hop costs the window columns (`-`) whatever its reason.
    let rows = rows.as_deref().map_err(|_| "placement hop failed");
    sessions_lines(&snapshot, rows, posture, |id| frozen.contains(&id))
}

/// `sessions bridge`: the roster's local-id, sid and launch-nonce triples, without the
/// placement hop or any per-session lock. The Fabric bridge asks every two seconds and fences
/// admission and status with them.
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

/// The batch must fail well inside the bridge's two-second roster cadence.
const BRIDGE_STATUS_HOP_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// `sessions status`: a Lines-framed roster of the status fields the Fabric bridge consumes,
/// agent verdict included, in one main-thread wake. The bridge checks each row's sid and nonce
/// against its preceding `sessions bridge` read. A busy timeline writer marks only its row
/// `agent_deferred=1` (the event loop never waits on it); a late hop is an `ERR`, and the bridge
/// keeps its prior samples.
pub(crate) fn cmd_sessions_status(proxy: &EventLoopProxy<Wake>) -> String {
    match super::control_media::call_main_within(proxy, BRIDGE_STATUS_HOP_TIMEOUT, |reply| {
        Wake::ReadSessionStatuses { reply }
    }) {
        Ok(rows) => rows,
        Err(e) => format!("ERR {e}\n"),
    }
}

/// The placement hop's deadline. Discovery (`aterm ctl ls`, the menu-bar fleet scan) dials each
/// instance with a 2 s deadline, so a busy main thread must cost `window=-` well inside it
/// rather than a false "could not reach": far above a healthy event-loop turn (sub-millisecond),
/// a quarter of that deadline.
const PLACEMENT_HOP_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// The main-thread hop behind the roster's window columns, bounded by
/// [`PLACEMENT_HOP_TIMEOUT`] so a busy main thread costs the window columns, never the listing.
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

/// The sanitized running command of `h`'s engine, `None` when idle or when the
/// lock is contended — the roster runs on the control thread and must never
/// park behind a PTY reader for a column.
fn session_detail(h: &SessionHandle) -> Option<String> {
    match h.term.try_lock() {
        Ok(t) => crate::session_status::executing_detail(&t),
        Err(std::sync::TryLockError::Poisoned(p)) => {
            crate::session_status::executing_detail(&p.into_inner())
        }
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

/// The `<title>` column of `sessions` and `family`, read live off the engine rather than the
/// store's copy, which only the active tab's title publisher feeds. First rung that answers:
///
/// 1. the operator's `meta set title`;
/// 2. the OSC title minus a console program path
///    ([`crate::tab_label::without_console_program_path`]);
/// 3. the native cwd, `~`-abbreviated as the tab strip paints it;
/// 4. the store's title, program path dropped, when the engine lock is contended (the roster
///    never parks behind a PTY reader) or there is neither title nor cwd;
/// 5. "aterm": an empty token would shift every later column for a whitespace-splitting reader.
///
/// Unlike the strip ([`crate::tab_label::informative`]), a prompt's `user@host:` prefix stays:
/// the roster is read by programs, and the prefix is part of what the program set.
fn roster_title(h: &SessionHandle) -> String {
    // Rung 1, a leaf lock dropped before the term lock.
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

/// The `sessions` wire body for `snapshot`: the store columns, then the
/// placement columns from `rows` — or `-` for all three window columns when the
/// rows could not be read (`Err`), with every line still printed. `detail=` is
/// the engine's, so it is answered either way; so is the title
/// ([`roster_title`]). `frozen_path` answers the registry's adoption mark per
/// local id (`path=frozen` when true) — a registry fact, so it is answered
/// either way too; so is `autopilot=`, under the window's `posture`.
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
        // One read of the session's metadata for every column after the title
        // (which [`roster_title`] resolves on its own): the `meta=` bit, the
        // live supervisor claim, the typed role, attention and user title a
        // fleet reader classifies by, and the session's Autopilot word.
        let (
            has_meta,
            supervisor,
            role,
            attention,
            user_title,
            word,
            land_at,
            (claimed_here, proven),
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
                ),
            )
        };
        // A session with no row is DETACHED (`window=none`), a fact; the hop's
        // failure is the `Err`, printed `-`, never a placement.
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
        // Already in the grammar `[a-z0-9][a-z0-9._-]*`, which the codec leaves as it is.
        let identity = h
            .identity
            .as_deref()
            .map_or_else(|| "-".to_string(), pct_encode);
        // Program and agent verdict after `path=`: the status sweep's
        // publication on the session timeline (a leaf lock, no hop), the same
        // fields `status` prints. From the same read: `path=` itself (measured
        // where a child of the shell was read, else the carried mark), the
        // owner's columns after `autopilot=` (`path_evidence= copy= upgrade=`),
        // which `land_at= task=` follow, and `task_state= task_rev=` last
        // after `pause=` ([`crate::task_record`]).
        let (agent, (path, owner), task_record, (supervised_agent, group), face, pause) = {
            let timeline = h.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            (
                timeline.agent().wire_fields(),
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
        // The agent the host admits (never a vendor `codex`, whose `autopilot=`
        // is `-`), read after the timeline's leaf lock.
        let supervised_agent = crate::harness_host::admitted_as(&h.ctx, supervised_agent, group);
        let autopilot = crate::autopilot::wire_field(posture, word, supervised_agent);
        // `land_at= task=` (design §9.3.5, §9.4.1), after the owner's columns: a timed
        // Cruise's instant and the task's face for an agent session, from
        // the same two reads.
        let land_at =
            crate::autopilot::land_at_field(land_at.filter(|_| supervised_agent.is_some()));
        // The face shows under this host's own claim and beside a proven
        // self-claim, whose acts tell it.
        let task = crate::autopilot::task_field(face, claimed_here || proven, supervised_agent);
        // `pause=` (§10.5.1) after `task=`: the loop's pause after short
        // turns, from the same read.
        let pause = crate::autopilot::pause_field(pause, claimed_here, supervised_agent);
        // `human_ms=<ms|->`: how long since a person last had a hand on the
        // session through a window — the same atomic `status` reads.
        let human = h.ctx.human_input.wire(crate::metrics::now_us());
        // Then the typed `role=`, `attention=` and `user_title=` (the escalated
        // set and the operator a fleet glance needs, in the roster itself rather
        // than one `@sid meta` hop per session) and `supervisor=<holder|->`, the
        // live supervisor claim, so a fleet read shows which agent tabs something
        // is answering for — all from the one meta read above — and beside it
        // `autopilot=`, the agent tab's effective Autopilot.
        let _ = writeln!(
            out,
            "{} {} {} {} {} meta={has_meta} nonce={nonce} window={window} active={active} \
             wfocus={wfocus} detail={detail} identity={identity} path={path} {agent} \
             human_ms={human} role={role} attention={attention} user_title={user_title} \
             supervisor={supervisor} autopilot={autopilot} {owner} land_at={land_at} task={task} \
             pause={pause} {task_record}",
            h.local_id,
            h.sid.as_str(),
            parent,
            h.state.as_str(),
            title,
            nonce = h.nonce.to_hex(),
        );
    }
    out
}

/// `who` -> the presence readout for the whole instance: one line per session
/// naming who is driving it and how many peers are watching it, so a human or an
/// orchestrator can always see the hand and the eye on a fleet. Framed `OK <n>`
/// then one `<local> <sid> driving=<turn-id|-> watchers=<n> turns=<n> <state>
/// nonce=<hex32> fgpgid=<pid|->` line per session (sorted by local id, like
/// `sessions`). The
/// public launch nonce comes from the same registry snapshot as the sid, so a
/// client that keys on a launch can read it here without asking the
/// `sessions` roster for a window-placement hop. `driving` is the
/// session's live turn lease (`Some(id)` while a `turn` is mid-flight, `-` when
/// idle — the one-driver-per-session mutex); `watchers` is the live `subscribe`
/// count on that session; `turns` is its ledger depth. Owner-only + self-scoped
/// like `sessions` (a fleet-wide readout, not a per-target query). Read-side
/// (pure observation of lease + subscriber + ledger state).
///
/// NOTE: `watchers` counts every live `subscribe` registration, which includes a
/// session's OWN in-flight `turn` (it registers a settle-watcher while running),
/// so a session being driven reads `driving=<id> watchers>=1`. That is honest —
/// the driver is watching for the reply — not double-counting an external peer.
/// `fgpgid` is the PTY master's foreground process group, or `-` when the
/// kernel cannot read it. The harness's live upgrade and relaunch
/// (`upgrade_drive`'s host roster) pair it with a Claude process's own
/// group and controlling-terminal foreground group; unlike the process's
/// environment, these kernel identities remain readable on macOS.
pub(crate) fn cmd_who(store: &Store) -> String {
    let snapshot = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        g.snapshot()
    };
    // An OS probe must never hold the registry mutex. Masters
    // belong to the cloned handles; a concurrently closed one simply reads -.
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

/// The auto-holder tag mint for a `lease acquire` with no `holder=`: a unique `drv-<n>`.
static NEXT_LEASE_TAG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Default / max cooperative-lease TTL (ms). Short by default so a crashed driver
/// frees the coordination slot quickly; capped so a hold cannot wedge it for long.
const LEASE_TTL_DEFAULT_MS: u64 = 30_000;
const LEASE_TTL_MAX_MS: u64 = 600_000;

/// `lease [status] | lease acquire [ttl=<ms>] [holder=<name>] [hard] [here] |
/// lease release [holder=<name>] [force] [here]` — the explicit drive lease. One holder at a
/// time, TTL-expiring, surfaced in `who` as `driving=lease:<holder>`. It is
/// mutually exclusive with any live lease (a different holder's `acquire`, and
/// another connection's `turn`, are refused while it is held) and self-expiring.
/// COOPERATIVE by default: it does NOT hard-block raw `send`/`key`/`feed` — those
/// stay governed by the `turn` lease — the coordination signal cooperating
/// agents check before driving. `hard` makes it a hold: every OTHER
/// connection's write verbs are refused `ERR busy lease=<holder>` as under a
/// `turn`, while the holder's own connection writes on — its own `turn` holds
/// the slot for the turn and hands the lease back after it
/// ([`crate::Lease::write_block`]). `here` keeps either act to the asking
/// connection: an acquire never moves a live lease held over another one,
/// even under the same holder, and a release never frees one — so two
/// lanes of one process under one holder name cannot take or free each
/// other's hand. A lease taken `here` is that connection's: it ends with
/// it ([`release_connection_lease`]), so a hand whose holder hung up, or
/// whose give-back was lost with its connection, never waits out its TTL.
/// A person's keyboard is never blocked.
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

/// `lease acquire [ttl=<ms>] [holder=<name>] [hard] [here]`: take (or, for the
/// same holder, renew) the lease — over the connection that asks, which a
/// renewal moves it to, `hard` or cooperative as this acquire says. Refuses a
/// live `turn` or a DIFFERENT holder's live lease; steals a lapsed one. With
/// `here` the same holder's live lease held over ANOTHER connection is
/// refused as a different holder's is, never moved: only this connection's
/// own is renewed, and the lease is bound to this connection: it ends when
/// the connection does ([`release_connection_lease`]). Returns the granted
/// `holder` so an unnamed caller learns the auto-tag it must release with.
fn lease_acquire<'a>(ctx: &SessionCtx, args: impl Iterator<Item = &'a str>) -> String {
    let mut ttl_ms = LEASE_TTL_DEFAULT_MS;
    let mut holder: Option<String> = None;
    let mut hard = false;
    let mut here = false;
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
            crate::Lease::Drive { .. } => {} // same holder — renew below
        }
    }
    *lease = Some(crate::Lease::Drive {
        holder: holder.clone(),
        expires_us: now + ttl_ms.saturating_mul(1000),
        conn: asking,
        hard,
        here,
        // Who took it, as the kernel says: read now, while this connection's
        // own request is served (Darwin's `LOCAL_PEERPID` names its last
        // writer). Never the `holder=` text.
        holder_pid: super::serving_peer_pid(),
    });
    drop(lease);
    if here && asking.is_some() {
        // The connection's end sweeps it, as it sweeps a bound claim.
        super::note_connection_claim();
    }
    // The presence rim follows the lease: one wake per change of hand.
    crate::presence::post_lease_changed(&ctx.self_id);
    format!(
        "OK lease acquired holder={holder} ttl_ms={ttl_ms} expires_in_ms={ttl_ms}{}\n",
        if hard { " hard=1" } else { "" }
    )
}

/// `lease release [holder=<name>] [force] [here]`: drop the cooperative lease. The
/// holder releases its own (name must match; with `here`, only while it is held
/// over the asking connection); `force` steals any cooperative hold; a lapsed
/// lease releases freely. A `turn`'s hard lease is owned by its own drop guard
/// and is never released here.
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
    /// The action decided under an immutable read, applied after the borrow ends.
    enum Act {
        None,
        /// Release, and wake the preempted turn's driving session too (its
        /// band's `▸ @<sid>` reads this lease): the wedged turn's own guard
        /// finds another id, or none, when it drops, and posts nothing.
        Release(Option<aterm_session::SessionId>),
        Refuse(String),
    }
    let now = crate::metrics::now_us();
    let mut lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
    let act = match lease.as_ref() {
        None => Act::None,
        Some(crate::Lease::Turn { id, driver, .. }) => {
            if force {
                // Force-preempt a wedged turn whose driver crashed: a deliberate operator
                // override, like `signal`. Safe because the turn's `LeaseGuard` clears only its
                // own id, so a fresh turn in the freed slot is not stomped when it returns.
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
            // Every move of a lease posts a wake to every session whose facts
            // read it: a preempted turn's driver too, or its presence facts
            // (`status hand=`, the hover's who-drives line) keep a lease mark
            // that has moved until something else re-senses it.
            if let Some(driver) = driver {
                crate::presence::post_lease_changed(&driver);
            }
            "OK lease released\n".to_string()
        }
        Act::Refuse(e) => e,
    }
}

/// A drive lease was freed: a text the harness typed under the hand and
/// never submitted is no turn (a later Enter is another act's,
/// [`ledger_typed_input`]), and the presence rim follows the lease.
fn lease_freed(ctx: &SessionCtx) {
    ctx.turns
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take_typed();
    crate::presence::post_lease_changed(&ctx.self_id);
}

/// CONNECTION `conn` ENDED: the drive lease it took `here`
/// ([`crate::Lease::Drive`]'s `here`) ends with it, live or lapsed, as
/// [`lease_release`] frees it. A hand a stop cut short (the turn-end loop's
/// connection is the one its interrupter hangs up), or one whose give-back
/// was lost with a connection that dropped, is free at once, not after its
/// TTL — while it stood, every other connection's writes were `ERR busy
/// lease=…`, under a hand the window shows no row for. A lease
/// taken without `here`, or over another connection, stays: the upgrade's
/// and relaunch's hand rides a reconnect, its renewal moving it.
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

/// `edges --json` / `grants --json` -> `{"edges":[{"src":"..","dst":"..","op":".."}],
/// "dst":"<self>"}`: the edges [`cmd_edges`] lists, as one object.
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

/// `family [<sid>]` -> the target's place in the session hierarchy, from the registry's
/// `parent` links. Header `OK\n`, then:
///   `self <sid> <state> <title>`
///   `parent <sid|-> ...`            (one line; `-` sid when the node is a root)
///   `child <sid> <state> <title>`  (zero or more, sorted by local id)
///
/// Titles are pct-encoded as in `sessions`. With no argument the target is the resolved
/// session; an explicit `<sid>` is Owner-only (`control::need_of`). An unknown target is
/// `ERR no such session`.
///
/// For Owner scope, connection rows follow (`pushes`, `pushed-by`, `pulls`, `pulled-by`, one per
/// peer, same shape): `write-input` makes a push, anything else a pull. Outbound rows scan other
/// sessions' tables, which is why a scoped caller gets none; it reads its own inbound table
/// with `edges`.
pub(crate) fn cmd_family(ctx: &SessionCtx, store: &Store, scope: Scope, rest: &str) -> String {
    // Target sid: an explicit argument, else the resolved session's own id. Both
    // are gated by the dispatch: the explicit form (arbitrary-node enumeration) is
    // Owner-only there (the table's `family` form), so a scoped edge never gets here
    // with one.
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
    // The title column is `sessions`' own ([`roster_title`]), so one session
    // never reads under two names across the two roster verbs.
    let line = |kind: &str, h: &crate::session_store::SessionHandle| {
        format!(
            "{kind} {} {} {}\n",
            h.sid.as_str(),
            h.state.as_str(),
            pct_encode(&roster_title(h)),
        )
    };
    // The body comes first so the header can carry its line count: the client frames a bare
    // `OK` as a one-line status and would drop the rows.
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
    // Connection rows, Owner only. Each table is locked after the store guard is released:
    // never a store lock across a table lock.
    if scope.is_owner_class() {
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
        // Inbound (peer → this node): rows in the target's own table, grouped by src. A foreign
        // src (wire-granted, never registered) still prints, `unknown -` for state and title.
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

/// How often a wait re-reads the park while its session's reader is parked
/// for an update: the kernel's idle deadline says nothing then, so the wait
/// polls the park instead, to take up its own clock the moment output flows
/// again.
const PARKED_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// Fire the idle watchers that are due, unless the session's reader is parked for an update
/// (`SessionCtx::update_parked`): then their clocks are held, since that silence is the host's,
/// not the program's, which may still be printing into the kernel queue.
fn fire_idle_unless_parked(t: &mut Terminal, now: std::time::Instant, parked: bool) {
    if parked {
        t.watch_restart_idle(now);
    } else {
        t.watch_expire(now);
    }
}

/// The exit check every parked verb shares: whether `session` was in the
/// registry is read here, at arm, and the returned probe answers "has it
/// ended?" against the store it is handed. A session that was registered but is
/// later gone (deregistered during teardown) is dead — the `Wake::Exit` notify
/// can race the deregistration, so the woken thread may see `None`. Treating
/// "was registered, now None" as exited closes that race; a headless unit term
/// (never registered) stays Alive on `None`.
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

/// `ready [timeout_ms]` -> block until the target session is alive and idle, then
/// `OK ready <reason>\n`; `OK timeout\n` if it does not become ready in time
/// (default 30 000 ms, capped at 600 000); `ERR exited\n` if the session has
/// exited (it will never become ready). Lets an agent chain sessions — spawn one,
/// `ready` on it, then drive it — without busy-polling a screen read.
///
/// Exactly two ready reasons are emitted:
///   * `prompt` — the newest OSC-133 block is at a fresh prompt (`PromptOnly`)
///     or a finished command (`Complete`): the shell is waiting for input. The
///     precise "prompt-end" signal, used when shell integration is present.
///     Never read while the session's reader is parked for an update, which
///     lasts until the resumed reader has parsed what queued meanwhile.
///   * `idle`   — the kernel's `IdleFor` watcher latched: `content_seq` held
///     stable across the settle window (output stopped changing). This is the
///     fallback for a session with no in-flight completed block — covering plain
///     shells (no shell integration) and the between-commands case alike.
///
/// Fully event-driven (no poll): arms an `IdleFor` watcher and parks on the
/// session's watchers ([`Park`]), woken by output, by the exit's timeline
/// record and by the idle deadline, so an exit is reported promptly.
pub(crate) fn cmd_ready(
    term: &Arc<Mutex<Terminal>>,
    store: &Store,
    session: u64,
    ctx: &SessionCtx,
    rest: &str,
) -> String {
    use std::time::{Duration, Instant};

    use aterm_core::terminal::{BlockState, WatcherSpec};

    // Accept a bare `<ms>` (original) or `timeout=<ms>` (k=v, aligning with the
    // rest of the wait family); anything else is the usage line, never the
    // default ([`aterm_control::wire::wait_ms`]).
    let timeout_ms = match rest.trim() {
        "" => aterm_control::wire::WAIT_DEFAULT_MS,
        t => match aterm_control::wire::wait_ms(t.strip_prefix("timeout=").unwrap_or(t)) {
            Some(ms) => ms,
            None => return "ERR usage: ready [<ms>|timeout=<ms>]\n".to_string(),
        },
    };
    let now0 = Instant::now();
    let deadline = now0 + Duration::from_millis(timeout_ms);
    // The settle window without shell integration: the kernel's `IdleFor` resets on every
    // content advance (`observe_at`), so it latches only after `SETTLE` of true quiet.
    const SETTLE: Duration = Duration::from_millis(60);

    // The lifecycle state of this resolved session, by its local id. `None` (not in
    // the registry — e.g. a headless unit term) is treated as Alive — unless the
    // session was registered at arm and is now gone (deregistered on teardown),
    // which is a dead session the exit's ring woke us for ([`exit_probe`]).
    let gone = exit_probe(store, session);

    // Arm one idle watcher up front; the engine auto-resets its deadline on output.
    let idle_id = term_lock(term).watch(WatcherSpec::IdleFor { dur: SETTLE }, now0);
    // Subscribe so output wakes us (block-state changes ride content_seq, so the
    // ring covers the shell-integration path too) — event-driven, no poll.
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
        // Shell-integration fast path: newest block prompt/complete => ready
        // prompt (read directly so an already-ready session returns at once).
        let prompt = matches!(
            t.all_blocks().last().map(|b| b.state),
            Some(BlockState::PromptOnly | BlockState::Complete)
        );
        fire_idle_unless_parked(&mut t, now, parked); // host-injected idle fire
        let settled = idle_id.and_then(|id| t.watch_poll(id)).is_some();
        // Parked, the parser is behind the program: what it wrote since waits
        // unread in the kernel, so neither the last prompt nor the silence is
        // its answer until the resumed reader has parsed that
        // ([`crate::spawn::UpdatePark`], which ends only then).
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
        // The kernel idle deadline is always armed here, so block-state
        // transitions are re-checked within the settle window.
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

/// `await <idle <ms> | seq [<n>] | match <re> [rows <a> <b>] | gone <re> [rows <a> <b>]
/// | block | inbox since=<id> [kinds=<k,...>] [topic=<t>]> [timeout <ms>]` — block until the
/// Observation Kernel (L0) latches the predicate, then return `OK <kind> <seq>`;
/// `OK timeout` if the overall deadline elapses; `ERR exited` if the session dies.
/// (`await consent [kind=debugger] [timeout=<ms>]` is dispatched to `control_privacy` before this
/// handler is reached; the usage line names it so the alternation stays complete.)
///
/// `gone` latches when no visible row in range matches: an agent's turn often ends with a row
/// leaving (Claude Code's `esc to interrupt` footer) while its composer glyph stays on screen
/// and the screen can sit still mid-turn, so neither `match` nor `idle` says the turn is over.
/// Like `match` and `seq` it is level-triggered.
///
/// This is the L1 exposure of the core primitive. The CORRECTNESS — no-silent-
/// loss for content/match/block, and a deterministic idle deadline — lives in the
/// kernel (`observe_at` at the `post_process` seam; the deadline is
/// model-checked by `idle_deadline_model`). This verb only *waits*, and it
/// is **event-driven**: it registers a subscriber and parks on its wake, so it
/// sleeps until a REAL event arrives —
///   * an output burst   (the PTY reader rings the session's watchers) for
///     content/match/gone/block predicates,
///   * the next idle deadline (the exact `IdleFor` fire instant, via
///     `watch_next_deadline`) for `await idle`,
///   * session exit       (the `state-change` or `closing` record rings them)
///     → `ERR exited`,
///   * the overall timeout (the ultimate liveness backstop) → `OK timeout`.
///
/// A slice is not an event: on a control lane serving a socket connection the
/// park is cut into [`crate::control::HANGUP_POLL`] (200 ms) slices
/// ([`crate::park`]), and a slice that ends with no wake asks only whether the
/// caller hung up (→ `ERR hangup`, and the lane goes back) and whether the
/// reader's update park moved: no store, terminal or predicate read. CPU stays
/// ~0% while parked; an in-process caller (no socket) parks exactly as long as
/// the events say.
pub(crate) fn cmd_await(
    term: &Arc<Mutex<Terminal>>,
    store: &Store,
    session: u64,
    ctx: &SessionCtx,
    rest: &str,
) -> String {
    use std::time::{Duration, Instant};

    use aterm_core::terminal::{RowRange, WatcherSpec};

    const USAGE: &str = "ERR usage: await <idle <ms>|seq [<n>]|match <re> [rows <a> <b>]|\
                         gone <re> [rows <a> <b>]|block|inbox since=<id> [kinds=<k,...>] [topic=<t>]|\
                         agent <word>[,<word>...] [since=<rev>]|task [@<sel>[:<rev>][,...]] [any|all] [since=<rev>]|\
                         log need|open=<lvl>|close=<lvl>|kind=<k> [since=<seq> ep=<e>]> \
                         [timeout <ms>] | await consent [kind=debugger] [timeout=<ms>] | await momentum <floor 0..=1> [timeout <ms>]\n";

    // Split off an optional `timeout <ms>` anywhere in the args; the rest is the
    // predicate + its arguments.
    let mut toks = rest.split_whitespace();
    let mut timeout_ms = aterm_control::wire::WAIT_DEFAULT_MS;
    let mut args: Vec<&str> = Vec::new();
    while let Some(tok) = toks.next() {
        // `timeout <ms>` or `timeout=<ms>`; a bare trailing `timeout` is an argument, and an
        // `<ms>` that is not a number is the usage line, never the default.
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

    // `await inbox` parks on the session's inbox condvar and arms nothing in the engine, so it
    // is answered before the arms below, which take the terminal lock.
    if kind == "inbox" {
        return crate::fabric::cmd_await_inbox(ctx, &args[1..], timeout_ms);
    }

    // `await agent <word>[,<word>…] [since=<rev>]` parks on the SERVER'S
    // published agent verdict (`status agent=`), not on the screen: no watcher
    // is armed and no terminal lock is taken. LATCHED — a verdict already in
    // the set answers at once, with `since=` only one published past that rev
    // — and woken by the `agent` record's ring when the verdict moves.
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

    // `await task [since=<rev>]` parks on THIS session's task record, like
    // `await agent` on its verdict: no watcher, no terminal lock. The
    // selector-less list form (`await task @a,@b …`) never reaches here — the
    // dispatcher resolves its targets first (`control::dispatch_await_task`).
    // `task` is the FIRST word, as the list form's: the reply is Lines-framed
    // and a client tells that from the second word of the request
    // (`aterm_types::control_verbs::framing_of`), so `await timeout=5 task`
    // would answer rows to a reader expecting one line.
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
        };
        return await_task(store, &[target], parsed.combine, timeout_ms);
    }

    // `await log need|open=<lvl>|close=<lvl>|kind=<k> [since=<seq> ep=<e>]` parks on
    // the session log, woken by every producer's ring (no watcher
    // armed, no terminal lock taken).
    if kind == "log" {
        return match crate::session_log::LogAwait::parse(&args[1..]) {
            Ok(wait) => await_log(store, session, ctx, wait, timeout_ms),
            Err(usage) => usage.to_string(),
        };
    }

    let now0 = Instant::now();
    // `match`/`gone` compile an untrusted (bounded) regex in `aterm-observe` before the terminal
    // lock is taken, so a crafted pattern cannot stall rendering, the PTY reader or other verbs;
    // only the install runs under the lock.
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
        // A span off the current grid is named now rather than burning the timeout.
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
                // Default `after` = the current content_seq (wait for the NEXT change).
                let current = t.content_seq();
                let after = args
                    .get(1)
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(current);
                // An `<n>` above the current count was read off another grid or another
                // process (an update's successor restarts the count): either way the screen
                // changed, so latch now with the seq to anchor on next.
                if after > current {
                    return format!("OK seq {current}\n");
                }
                t.watch(WatcherSpec::SeqAdvanced { after }, now0)
            }
            "block" => t.watch(WatcherSpec::BlockComplete, now0),
            _ => return USAGE.to_string(),
        }
    };
    let Some(id) = armed else {
        return format!("ERR {WAITS_FULL}\n");
    };

    let overall = now0 + Duration::from_millis(timeout_ms);

    // Register on THIS session's watchers: the PTY reader's burst and the exit's
    // timeline record ring them the instant output lands or the session dies —
    // so content predicates (`seq`/`match`/`gone`/`block`) and
    // exit detection are event-driven with NO fixed-interval poll. `await idle`
    // parks straight to the kernel's `next_deadline`. The single-slot notify is
    // lossless (a wake that arrives between two `wait`s stays pending), and the
    // overall timeout is the ultimate backstop — so no re-query is needed.
    let sub = crate::session_watch::Subscription::single(&ctx.watchers);

    // Whether the session was in the registry at arm ([`exit_probe`]).
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

/// The words `await agent` accepts — `status agent=`'s vocabulary, the walls
/// aside.
const AGENT_WORDS: [&str; 7] = [
    "busy", "prompt", "question", "idle", "survey", "unknown", "-",
];

/// One `await agent` word: a verdict of [`AGENT_WORDS`], `wall` for any `wall:<kind>` verdict,
/// or `wall:<kind>` for that one (`aterm_phase::WallKind::name`'s kinds, and the server's own
/// `unresponsive`). A well-formed `<kind>` no wall carries parks to its timeout rather than
/// being refused, since a newer reader may name one this build does not. `limited` is
/// `ERR usage` like any unknown word: `wall` or `wall:<kind>` says which walls are meant.
#[derive(Clone, Debug, PartialEq, Eq)]
enum AgentWord {
    /// One of [`AGENT_WORDS`].
    Verdict(&'static str),
    /// `wall` (`None`) or `wall:<kind>`.
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

/// `prompt,question,wall:rate` → the words, each checked against
/// [`AGENT_WORDS`] or the wall shape (a `<kind>` of 1..=32 bytes of
/// `[a-z0-9_-]`); `None` for an empty list or an unknown word (a typo must
/// not wait out the timeout).
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

/// `await agent`: answer `OK agent <word> rev=<n>` the moment the session's
/// published verdict is one of `words` (at once if it already is) — with
/// `since`, only a verdict whose rev is past it, so the one a `turn`'s Enter
/// found (its `agent_rev=`) is no answer however soon the wait is armed —
/// `OK timeout` at the deadline, `ERR exited` when the session ends. Parks on
/// the session's watchers, which the timeline's `agent` record rings when the
/// sweep publishes a new verdict. A verdict in the set answers before the
/// exit does, so the exit is read inside the look, after the verdict.
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
    // The exit is read in the look, after the verdict.
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

/// One target of an `await task` ([`await_task`]): the session, and the task
/// rev it waits past — its own `@<sel>:<rev>` or the request's `since=`, else
/// (`None`) its rev when the wait is armed.
pub(crate) struct TaskTarget<'a> {
    /// The session's process-local id (its exit probe's key).
    pub(crate) local_id: u64,
    /// The session.
    pub(crate) ctx: &'a SessionCtx,
    /// The rev it waits past.
    pub(crate) since: Option<u64>,
}

/// `await task`: park until the task records of `targets` have news — a
/// target whose record's rev is past its `since` with a state to read (`done`,
/// `exited`, `stalled`) or the supervisor's attention raised
/// ([`crate::task_record::TaskRecord::has_news`]), or whose session closed —
/// at the first such target (`any`) or once every target is one (`all`).
/// EDGE-TRIGGERED on the rev: a target's `since` defaults to its rev at arm,
/// so news a manager already read does not answer again, and a wait re-armed
/// with the revs it was answered with (`@<sel>:<rev>`) misses nothing between
/// the two.
///
/// LATCHED within the wait: a target that showed news keeps it, and the row
/// it showed, until the wait answers — news that came and went between two
/// wakes (the supervisor's attention raised and cleared, a `done` moved back
/// to `working`) still counts toward `all`. A wait armed again reads every
/// record as it stands then.
///
/// Answers `OK <n>` and one [`crate::task_record::await_row`] per target, in
/// request order — a session named twice answers twice; at the deadline the
/// same rows under `OK <n> status=timeout` (the Lines framing's timeout:
/// aterm-ctl exits 124 on it); `ERR hangup` when the caller hung up. Parks on
/// the targets' watchers, which a mark's `task-change` row
/// ([`crate::task_record::mark_session_task`]), an attention write's
/// `meta-change` row and a session's exit ring — no poll. The same lists ring
/// on every output burst, so a look first compares each unlatched target's
/// rev and news under its one timeline lock, and builds rows only for news and
/// for the answer.
pub(crate) fn await_task(
    store: &Store,
    targets: &[TaskTarget<'_>],
    combine: crate::task_record::Combine,
    timeout_ms: u64,
) -> String {
    use std::time::{Duration, Instant};

    use crate::task_record::Combine;

    let overall = Instant::now() + Duration::from_millis(timeout_ms);
    // Registered BEFORE the edges are read: a mark landing in between is a
    // ring below, never a missed edge. A session named twice is watched once.
    let mut sub = crate::session_watch::Subscription::new();
    for target in targets {
        let _ = sub.watch(&target.ctx.watchers);
    }
    let edges: Vec<u64> = targets
        .iter()
        .map(|t| {
            t.since
                .unwrap_or_else(|| task_record_of(t.ctx).map_or(0, |task| task.rev))
        })
        .collect();
    let exits: Vec<_> = targets
        .iter()
        .map(|t| exit_probe(store, t.local_id))
        .collect();
    let mut latched: Vec<Option<String>> = vec![None; targets.len()];
    // A closed session is a target's news, latched in the look: no exit ends
    // this park.
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
    // The look latched once more as the window closed, so a timeout's rows
    // are the targets as they stood then.
    let tail = match park.run(overall, look, || {}) {
        Parked::Done(()) => "",
        Parked::Deadline | Parked::Exited => " status=timeout",
        Parked::HungUp => return crate::control::HUNG_UP_REPLY.to_string(),
    };
    let mut out = format!("OK {}{tail}\n", targets.len());
    for ((target, exited), latch) in targets.iter().zip(&exits).zip(latched) {
        let row = latch.unwrap_or_else(|| {
            task_wait_row(target, task_record_of(target.ctx).as_ref(), exited(store))
        });
        out.push_str(&row);
        out.push('\n');
    }
    out
}

/// A session's task record, cloned off its timeline leaf.
fn task_record_of(ctx: &SessionCtx) -> Option<crate::task_record::TaskRecord> {
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .task()
        .cloned()
}

/// `target`'s [`crate::task_record::await_row`] for `record`, with the
/// session's agent word and attention as they read now.
fn task_wait_row(
    target: &TaskTarget<'_>,
    record: Option<&crate::task_record::TaskRecord>,
    closed: bool,
) -> String {
    let (agent, attention) = crate::task_record::agent_and_attention(target.ctx);
    crate::task_record::await_row(
        target.ctx.self_id.as_str(),
        record,
        agent,
        &attention,
        closed,
    )
}

/// One wake of an [`await_task`]: LATCH each target not latched yet that has
/// news now — its session closed (`exits`), or its record's rev past its edge
/// with news — with the row it shows. A latched target is never read again,
/// so its news holds until the wait answers. The read is the cheap one: under
/// the timeline lock, the rev and the news; the record is cloned, and the row
/// built, only for news.
fn latch_task_news<F: Fn(&Store) -> bool>(
    store: &Store,
    targets: &[TaskTarget<'_>],
    edges: &[u64],
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
            timeline
                .task()
                .filter(|task| task.rev > *edge && task.has_news())
                .cloned()
        };
        if closed || news.is_some() {
            let record = news.or_else(|| task_record_of(target.ctx));
            *latch = Some(task_wait_row(target, record.as_ref(), closed));
        }
    }
}

/// `await log` (phase 3A, design §7.2; ruling R2): answer the first record past
/// the cursor that satisfies the wait — `OK seq=<n> ep=<e>[ gap=<why>:<lost>]
/// <record>` — or a gap with no match (`OK gap why= lost= resync_to= ep=`, whose
/// `resync_to` a wait re-armed there reads past), `OK timeout` at the deadline,
/// `ERR exited` once the session ended and its close record is read. With no
/// `since=`, `need` latches on the observed needs the fold holds open and every
/// other wait starts at the head. Parks on the session's watchers, which every
/// session-log producer rings where it takes the log's owed wake
/// (`SessionTimeline::slog_take_wake`): no poll. The waiter
/// holds the session's `ctx`, so it reads the closed session's own log after the
/// store forgot it. Each check takes the timeline's leaf lock alone.
///
/// A shell that exited on its own is `Exited` at once (`exit_session_logical`)
/// but its log's close (`close - spawn out=shell-exit code=`) is written by the
/// retire's deregister, which comes after and wakes this waiter. So once the
/// session ended, the waiter takes one look under the timeline's lock
/// (`SessionLog::await_look`): the log past the cursor and whether the close is
/// still due. The exit path sets that before `Exited`; the close clears it, and
/// so does the end of the exit handling, which wakes the waiters (for an exit
/// an update handoff deferred, the end of the attempt's replay).
/// While it is due the waiter parks on for that close rather than answer `ERR
/// exited` over the record the wait asked for; otherwise it answers at once —
/// the close is already in what it read, or none is coming (a kept pane, a
/// held pane restored across an update). No second read follows the look, so
/// a retire landing after it cannot slip between two of them.
fn await_log(
    store: &Store,
    session: u64,
    ctx: &SessionCtx,
    wait: crate::session_log::LogAwait,
    timeout_ms: u64,
) -> String {
    await_log_seamed(store, session, ctx, wait, timeout_ms, || {})
}

/// [`await_log`], running `after_look` the instant after each look it takes
/// at an ended session and before it decides (production passes nothing):
/// the seam a test lands a retire on, to pin that the look is the waiter's
/// only observation.
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
    // The exit is read in the look, after the log: an ended session's own
    // records answer first.
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
            // One look (`SessionLog::await_look`): the close and the flag it
            // clears are written in one hold of the timeline's lock, and the
            // look reads both in one, so a look that finds the close not due
            // has the close in what it read, if it was ever written.
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
            // Due: the retire (or the end of the exit handling) rings this
            // waiter, its wake latched since it registered.
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

/// Process-wide `turn` id mint: every turn gets a unique id, reported on the
/// reply status line (`id=<n>`) and named by the lease's `ERR busy turn=<n>` so
/// a refused writer can tell WHICH exchange it collided with. It holds the last
/// id minted. A self-update handoff carries it ([`turn_ids_minted`] on the way
/// out, [`raise_turn_ids`] on the way in), and so does a crash: every journaled
/// session's crash-restore region is given it as it moves
/// (`crash_restore::note_turn_ids`), and the window that restores them continues
/// above it (`crash_restore::continue_carried_turns`). So ids keep rising across
/// the update or the crash, and a `subscribe … since-turn=<n>` or `history
/// since=<n>` anchor taken before it still means "after turn n".
static NEXT_TURN_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Mint the next turn id (see [`NEXT_TURN_ID`]), and give it to every journaled
/// session's crash-restore region before the caller hands it to anyone (it
/// mints under the session's ledger lock, before the lease or a reply names it).
fn mint_turn_id() -> u64 {
    let id = NEXT_TURN_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    #[cfg(unix)]
    crate::crash_restore::note_turn_ids();
    id
}

/// [`mint_turn_id`], for a crash-restore test that holds the region to every id minted.
#[cfg(all(test, unix))]
pub(crate) fn mint_turn_id_for_test() -> u64 {
    mint_turn_id()
}

/// The last turn id minted — by this process, or by the one it continued the
/// count from ([`raise_turn_ids`]); 0 before either — for the handoff manifest.
pub(crate) fn turn_ids_minted() -> u64 {
    NEXT_TURN_ID.load(std::sync::atomic::Ordering::Relaxed)
}

/// Continue the turn-id count from `minted` (an earlier process's [`turn_ids_minted`]). Never
/// lowers it, nor raises it past `handoff_carry::MAX_TURN_ID`: a carried count is input, and
/// one at `u64::MAX` would overflow the next `+ 1` and break the rising ids.
pub(crate) fn raise_turn_ids(minted: u64) {
    NEXT_TURN_ID.fetch_max(
        minted.min(crate::handoff_carry::MAX_TURN_ID),
        std::sync::atomic::Ordering::Relaxed,
    );
    #[cfg(unix)]
    crate::crash_restore::note_turn_ids();
}

/// How many turn ids a self-update leaves this process between the park's manifest and its
/// `_exit` ([`turn_ids_for_handoff`]). The parent serves the control socket until Commit, so a
/// `turn` in that overlap mints an id; the successor counts on from above the reserve, so no id
/// names two turns. Only minting more than this in one overlap stands the Commit down
/// ([`turn_ids_past_handoff_ceiling`]), a lossless retry.
pub(crate) const TURN_ID_RESERVE: u64 = 1 << 12;

/// The turn-id count a handoff manifest carries: ids minted so far plus [`TURN_ID_RESERVE`],
/// within what a manifest holds. The caller keeps it on its attempt
/// (`HandoffAttemptArbiter::set_turn_id_ceiling`) for [`turn_ids_past_handoff_ceiling`].
#[cfg(any(unix, test))]
pub(crate) fn turn_ids_for_handoff() -> u64 {
    turn_ids_minted()
        .saturating_add(TURN_ID_RESERVE)
        .min(crate::handoff_carry::MAX_TURN_ID)
}

/// Whether this process has minted a turn id past `ceiling`, the count the
/// attempt's handoff manifest carried: the successor would mint it again, so
/// the Commit stands down (and the next attempt carries a fresh count). A
/// `ceiling` of 0 — an attempt whose manifest was never written — is none.
///
/// Nor is a ceiling at the top of what a manifest carries
/// (`handoff_carry::MAX_TURN_ID`): the successor continues from there whatever
/// this process minted ([`raise_turn_ids`] holds the count to it), so standing
/// down protects no id — and since the next attempt's ceiling is the top again,
/// it would refuse every later Commit, an update that never lands. A count gets
/// there only from a carried count no aterm minted; the successor says so
/// (`seamless::take_incoming`).
#[cfg(unix)]
pub(crate) fn turn_ids_past_handoff_ceiling(ceiling: u64) -> bool {
    past_ceiling(turn_ids_minted(), ceiling)
}

/// [`turn_ids_past_handoff_ceiling`]'s rule: `ceiling` 0 is none, and so is
/// one at the top of the manifest's range.
#[cfg(any(unix, test))]
const fn past_ceiling(minted: u64, ceiling: u64) -> bool {
    ceiling > 0 && ceiling < crate::handoff_carry::MAX_TURN_ID && minted > ceiling
}

/// Input delivery for [`cmd_turn`], resolved by the dispatch: the active tab's path delivers
/// through the event loop (`Wake::Input`, so window side effects run), a cross-session target
/// through the source-blind seam (`seam_egress`). `cmd_turn` never touches an input API itself,
/// so both paths stay byte-indistinguishable.
pub(crate) struct TurnIo<'a> {
    /// Deliver the message text into the target's input path with PASTE
    /// semantics (control bytes stripped by `format_paste`; bracketed-paste
    /// framing only when the app itself enabled the mode).
    /// `false` means the bounded egress did not accept the complete paste; the
    /// turn stops before submit.
    pub paste: &'a dyn Fn(&str) -> bool,
    /// Press a named key (the `key` verb vocabulary, e.g. "enter"). `false`
    /// means the name did not parse — reported as a usage error.
    pub press: &'a dyn Fn(&str) -> bool,
    /// Press one pre-built key event (`turn typed=1`'s per-grapheme press, built by
    /// [`typed_key_events`]) through the same route `press` takes. `false` means the route
    /// refused it, as [`TurnIo::paste_only`] refuses every one.
    pub key: &'a dyn Fn(InputEvent) -> bool,
    /// One reading of the target's typing momentum (`trail status momentum=`) and when it was
    /// read, for `turn yield=<floor>`. `Err` names why there is none, and the turn refuses the
    /// yield rather than type over a ribbon it could not see.
    pub momentum: &'a dyn Fn() -> Result<(f32, std::time::Instant), String>,
    /// WHO is driving: the session the connection's edge token was granted to
    /// (`EdgeTable::src_of` on the resolved target's table — the one that
    /// authorized this request), or `None` for an Owner-class caller (the
    /// per-instance token, the bridge), neither of which is a session.
    /// Recorded in [`crate::Lease::Turn`] for the length of the turn; `status
    /// hand=` and the tab's hover name the driver from it.
    /// Resolved by the dispatch site like the routes above, because only the
    /// dispatch has the scope.
    pub driver: Option<aterm_session::SessionId>,
    /// WHO is calling, for the turn's ledger record ([`turn_by`]: the
    /// connection's identity, `ctl`, `bridge` or the edge's sid). The turn
    /// refines it by the lease it types under ([`typed_by`]: the window
    /// harness's own hand makes an owner-token turn `harness`), because only
    /// the turn sees that lease. Resolved by the dispatch site, like
    /// [`Self::driver`]. The turn ledger's `by=`; the session log's is
    /// [`Self::actor`].
    pub by: TurnBy,
    /// The session log's `by=` for the turn this exchange opens: the
    /// connection's ([`log_actor`]: `harness`, `ctl`, `bridge`, an edge's
    /// minted sid, else `-`), never a parameter. Resolved by the dispatch,
    /// which has the scope; `ctl` for the routes with no connection (the
    /// keyed replay path).
    pub actor: aterm_slog::Actor,
    /// A leading `cause=` the dispatch took and checked: the
    /// turn this exchange opens names it. `cmd_turn` takes one among its own
    /// options too, and refuses a second.
    pub cause: Option<aterm_slog::ShortId>,
    /// Shared receipt observer for the actual production routes; bool alone is not byte evidence.
    pub mark: Option<&'a crate::control_input_ack::InputMark>,
}

/// The end of turn `id`'s INPUT: its lease stops saying the driver may type
/// ([`crate::Lease::driver_may_type`]) — only while the slot still holds
/// this turn, never a turn that preempted it — and the presence is woken, so
/// the session's slot reads the lease's end now. `true` when this turn's
/// lease was the one changed.
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
    /// The two routes a plain `turn` needs and none of the even-hand ones: a
    /// base for `..TurnIo::paste_only()` at a test site that pastes and presses
    /// only. `typed=1` and `yield=` against it answer `ERR`, never a silent
    /// downgrade to a paste.
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
        }
    }
}

/// The typed-prefix cap of `turn typed=1`: at most this many graphemes are pressed one key at a
/// time, the remainder going as one paste. 240 at the default cadence is ~17 s of typing,
/// inside the default 240 s deadline with room for the reply.
pub(crate) const TYPED_CAP: usize = 240;

/// The most graphemes `turn typed=1 cap=<n>` may ask to have pressed one key at a time. The
/// default cap bounds time; a faster caller asks for room with `cap=`, since an agent that reads
/// a long paste as pasted (Claude Code's `<pasted_content>`, not its person's words) reads the
/// same text pressed as its person's. 4096 keys at a harness notice's 2 ms is ~8 s.
pub(crate) const TYPED_CAP_MAX: usize = 4096;

/// The default inter-key interval of `turn typed=1`, in ms (~14 cps). Held exactly, it is the
/// typed turn's one honest signature: no hand keeps a cadence to the millisecond, and the seam
/// is told nothing else.
pub(crate) const TYPED_CADENCE_MS: u64 = 70;

/// The US-layout BASE key SHIFT composes `c` from — the inverse of
/// `aterm_types::keyboard::shifted_character`, so a typed turn presses the
/// same `(Key::Character(base), SHIFT)` a keyboard presses for a capital or a
/// shifted symbol and the encoder recomposes the glyph itself. `None` when
/// `c` is not a shifted glyph on that layout.
fn unshift_us(c: char) -> Option<char> {
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

/// Whether `base` is a key `aterm_winit_keymap::base_layout_key_for` names —
/// the US-QWERTY identity a physical press carries in the Kitty alternate-key
/// field — so a typed turn's `base_layout` matches a US keyboard's exactly.
fn has_us_base_layout(base: char) -> bool {
    base.is_ascii_lowercase()
        || base.is_ascii_digit()
        || matches!(
            base,
            '`' | '-' | '=' | '[' | ']' | '\\' | ';' | '\'' | ',' | '.' | '/'
        )
}

/// One grapheme of a typed turn as the key event a keyboard builds for it: a capital or shifted
/// symbol is its base key under SHIFT (the encoder composes the glyph), space, line break and
/// tab are their named keys, and a multi-scalar cluster is a committed `Text` run (the IME
/// path). Other control characters are dropped: the paste path strips them and no key types
/// them.
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

/// `turn typed=1`'s press list for `text`, one event per grapheme
/// ([`typed_key_event`]), pure so the tuple a typed turn presses is pinned
/// beside the tuple a keyboard presses without an event loop.
pub(crate) fn typed_key_events(text: &str) -> Vec<InputEvent> {
    use aterm_grapheme::GraphemeClusters;
    text.graphemes().filter_map(typed_key_event).collect()
}

/// Split `text` at the `cap`th grapheme ([`TYPED_CAP`] unless the turn said
/// `cap=`): `(typed prefix, pasted remainder)`. The remainder is empty for a
/// text inside the cap.
pub(crate) fn split_typed_cap(text: &str, cap: usize) -> (&str, &str) {
    use aterm_grapheme::GraphemeClusters;
    match text.grapheme_indices().nth(cap) {
        Some((at, _)) => text.split_at(at),
        None => (text, ""),
    }
}

/// What a fenced typed turn is held to between its keys. `if-gen=`/`if-rows=` are checked once,
/// before the first key, and cannot be asked again (the turn's own echo moves the generation
/// and its rows at every key), so the hold checks what they stand for:
///
/// - `person`: the session's person count ([`crate::human_input::HumanInputStamp::seq`]),
///   which a controller's keys never move, so a moved count is a person whose keys would be
///   mixed into the text.
/// - `epoch`: the screen's invalidation epoch ([`crate::control::ScreenGen`]), moved by every
///   screen switch and RIS, never by an echo.
/// - `frame`: armed by an `if-rows=` fence whose rows are an input frame ([`Frame::of`]); other
///   rows leave the hold to person and epoch, so the verb stays app-agnostic. A box that mounts
///   between two keys on the same screen (a permission box) moves neither count and would take
///   the text's digits as answers, so the typing is held to the frame ([`Frame::holds`]) and a
///   miss stops it as `changed`. A frame the program has not finished drawing (DEC 2026,
///   [`Terminal::sync_frame_unfinished`]) is not judged: the key waits. A digit, the selector
///   of every numbered box, also waits until the program has read every key before it
///   ([`keys_unread`]) and the caret has moved since the previous key.
///
/// Not seen: keys already sent when a box mounts, before its first finished frame (about one
/// render's worth, 7-8 keys at `cadence=2`), and a digit's gap between the program reading a
/// key and drawing the box it raises. A card drawn above an intact composer leaves the frame
/// whole. A turn fenced by `if-gen=` alone is held to person and epoch only: without a frame
/// the host cannot tell a box's drawing from the turn's own echo.
///
/// A one-key card over the composer (Claude Code's session survey, rating cards, the transcript
/// ask) reads the composer's value, answering only when it is exactly one of the card's keys,
/// changed since the card mounted and left alone 400 ms. A typed text's first key is its whole
/// value only until the next key, so it answers a card only if it opens with the card's key and
/// its second key is held back 400 ms. Its first key would still close an open card unanswered,
/// as a person's does, so the driver never types while one is open (`wait:survey`).
struct TypingHold {
    epoch: u64,
    person: u64,
    frame: Option<Frame>,
}

/// The input frame an `if-rows=` fence names, as the fence found it: its first row (the top
/// rule), its last (the bottom rule), and the first grapheme of the row after the first (the
/// caret, Claude Code's `❯`; see `aterm_phase::phase::composer_rules`). It can stop falsely, a
/// cut whose words stand and whose owner is told, when the program retitles its top rule
/// mid-text, the text grows past the pane and pushes the top rule off, or the frame stays torn
/// to the deadline.
struct Frame {
    top: String,
    bottom: String,
    caret: String,
}

impl Frame {
    /// The frame the fence's rows are, read under the fence's lock hold and judged by the rows
    /// alone: a rule ([`aterm_phase::phase::is_rule`]) over a row opening with a caret, down to
    /// a rule. Armed wherever the cursor sits now, since a fence checked mid-repaint finds the
    /// program's write position. `None` for any other rows (a shell prompt, a single line).
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

    /// Whether the cursor on `row` is still in the frame: the row under it is the bottom rule
    /// (typing only appends) and the nearest top rule at or above it sits over a row opening
    /// with the caret. The frame may move and grow, and rows outside it may change. A box's
    /// title sits under its top border (` Bash command`), so a border that matches the
    /// composer's rule is caught by the caret. A frame with no caret never holds.
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

/// One look at a [`TypingHold`], under one hold of the terminal lock.
enum Look {
    /// Go on: the cursor as it stood.
    Held(aterm_core::grid::Cursor),
    /// Not judged yet: the program's frame is torn, or a digit's previous
    /// key has not moved the caret.
    Wait,
    /// Stop the typing, and the verdict's reason.
    Stop(&'static str),
}

impl TypingHold {
    /// The hold as the fence found the screen (the caller holds its lock),
    /// with the frame `if-rows=` names ([`Frame::of`]).
    fn of(
        terminal: &Terminal,
        ctx: &SessionCtx,
        rows: Option<aterm_types::control_verbs::RowsFence>,
    ) -> Self {
        Self {
            epoch: crate::control::screen_gen(terminal).epoch,
            person: ctx.human_input.seq(),
            frame: rows.and_then(|fence| Frame::of(terminal, fence)),
        }
    }

    /// One look: a person's hand (`person`, asked first — the more useful
    /// word) or another screen (`changed`) stops the typing; then, with a
    /// frame, a torn one waits, a finished one the cursor has left stops it
    /// (`changed`), and a digit's caret not yet moved off `sent_from` (the
    /// cursor the key before it went out from) waits.
    fn look(
        &self,
        terminal: &Terminal,
        ctx: &SessionCtx,
        sent_from: Option<aterm_core::grid::Cursor>,
    ) -> Look {
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
        } else if !frame.holds(terminal, usize::from(cursor.row)) {
            Look::Stop("changed")
        } else if (sent_from.is_some() && keys_unread(ctx)) || sent_from == Some(cursor) {
            // The key before it is still unread by the program, or it has
            // not yet moved the caret off where it went out from.
            Look::Wait
        } else {
            Look::Held(cursor)
        }
    }

    /// The hold judged before a key, the remainder's paste or the Enter: the
    /// cursor it held at and whether it waited first, or why the typing
    /// stops. A [`Look::Wait`] parks OUTSIDE the lock on the turn's
    /// subscription (`sub`), so the program's write that finishes its frame
    /// or moves the caret looks again at once, and for at most one `tick`,
    /// which bounds the wait for keys to be read (that has no wake), all
    /// within the turn's deadline (`deadline`) — never a key pressed on a
    /// frame nobody could judge.
    fn held(
        &self,
        term: &Arc<Mutex<Terminal>>,
        ctx: &SessionCtx,
        sub: &crate::session_watch::Subscription,
        sent_from: Option<aterm_core::grid::Cursor>,
        tick: std::time::Duration,
        deadline: std::time::Instant,
    ) -> Result<(aterm_core::grid::Cursor, bool), &'static str> {
        let mut waited = false;
        loop {
            let look = self.look(&term_lock(term), ctx, sent_from);
            match look {
                Look::Held(cursor) => return Ok((cursor, waited)),
                Look::Stop(why) => return Err(why),
                Look::Wait => {
                    let now = std::time::Instant::now();
                    if now >= deadline {
                        return Err("deadline");
                    }
                    waited = true;
                    let _ = sub.wait(tick.min(deadline - now));
                }
            }
        }
    }
}

/// Whether the program has yet to read keys already written to it: bytes in the slave's input
/// queue or still in the sink's spill ([`aterm_session::sink::SinkWriter::input_backlog`]; a
/// spill whose lock was busy counts as unread). A digit waits for none, since the caret check
/// alone is satisfied by an older key's echo while later keys sit unread. A tty that cannot
/// say (`None`) keeps the caret check alone.
fn keys_unread(ctx: &SessionCtx) -> bool {
    ctx.sink
        .input_backlog()
        .is_some_and(|b| b.queued != 0 || b.spilled != Some(0))
}

/// Whether a typed turn's key is an unshifted ASCII digit, the selector of every numbered box
/// (`1. Yes`, `2. No`), which [`TypingHold::held`] holds back (`!` is SHIFT+`1`, not one).
fn is_digit_key(ev: &InputEvent) -> bool {
    matches!(
        ev,
        InputEvent::Key {
            key: aterm_types::keyboard::Key::Character(c),
            mods,
            ..
        } if c.is_ascii_digit() && mods.is_empty()
    )
}

/// The reply for a typed turn stopped part-way ([`TypingHold::held`], or its deadline): nothing
/// is pressed after it, the Enter included, and `typed=<k> pasted=<p>` tells the driver part of
/// its text sits where it was typed. Lines framing with zero rows, like every skip.
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

/// How a [`Park`] ended.
enum Parked<T> {
    /// The look answered.
    Done(T),
    /// The window closed first.
    Deadline,
    /// The session ended.
    Exited,
    /// The caller hung up. Only an [`OnHangup::Abort`] park ends this way.
    HungUp,
}

/// Whether a [`Park`] ends early when the caller hangs up.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OnHangup {
    /// Nothing has been typed yet (a `turn`'s yield, `await momentum`), or the
    /// submit is verified (a `turn`'s final settle): abandoning the wait leaves
    /// no half-applied input, so the lane goes back now. A `turn` that typed
    /// is still recorded in the turn ledger, with `status=hangup`.
    Abort,
    /// Text is in the target's composer and its submit is not yet verified (a
    /// `turn`'s echo settle and submit verification): the turn finishes, or
    /// the text would sit there unsubmitted and the next turn's would be
    /// appended to it.
    Finish,
}

/// What a [`Park`]'s window does while the session's reader is parked for an
/// update (`SessionCtx::update_parked`), when nothing consumes the program's
/// output and its silence is the host's.
#[derive(Clone, Copy)]
enum Stretch<'a> {
    /// The park does not read the flag: what it waits on is not the
    /// program's output (`await momentum`'s ribbon, `await agent`'s verdict).
    None,
    /// The idle clocks are held while parked ([`fire_idle_unless_parked`])
    /// and the flag is re-read every [`PARKED_POLL`], but the client's
    /// deadline stands (`await`, `ready`).
    Hold(&'a crate::spawn::UpdatePark),
    /// [`Self::Hold`], and the window does not close while parked: the time
    /// spent parked is added to `until`, never past `bound`, because an absent
    /// echo then says nothing about the press or the program (a `turn`'s
    /// phases, stretched never past the turn's own deadline).
    Window {
        parked: &'a crate::spawn::UpdatePark,
        bound: std::time::Instant,
    },
}

impl Stretch<'_> {
    /// Whether the reader is parked now, as this stretch reads it.
    fn parked(self) -> bool {
        match self {
            Stretch::None => false,
            Stretch::Hold(parked) | Stretch::Window { parked, .. } => parked.is_parked(),
        }
    }
}

/// The park every blocking session verb shares: `ready`, `await`, `await
/// agent`, `await momentum` and each of a `turn`'s phases. [`Park::run`] parks
/// on the session's watchers until the look answers, the window closes, the
/// session exits or (for [`OnHangup::Abort`]) the caller hangs up. A park
/// holds the watchers' list through its subscription, never the session's
/// context.
struct Park<'a> {
    sub: &'a crate::session_watch::Subscription,
    exited: &'a dyn Fn() -> bool,
    on_hangup: OnHangup,
    stretch: Stretch<'a>,
    /// How the park asks after its caller: the thread's served connection
    /// ([`Lane::SERVED`]); a test's own slices.
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

    /// Run the park until `until`, as stretched. Each look is `look(now,
    /// parked, closing)`: it takes its own lock (the terminal's or the
    /// timeline's), reads, disarms under that guard when it answers or
    /// `closing` is set, and returns its answer and the next instant the
    /// kernel names. `disarm` runs on the paths that end without a look (an
    /// exit, a hang-up).
    ///
    /// A look runs at entry, on a ring of the session's watchers (output, a
    /// timeline record, the update park's own flip), at the kernel's instant,
    /// every [`PARKED_POLL`] while parked, and at the window's close. A slice
    /// of a served lane that ends with no ring asks only whether the caller
    /// hung up and whether the update park flipped ([`crate::park`]): an exit
    /// is a timeline record, so it rings.
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
            // A caller that hung up ends an abortable wait: no socket caller
            // is left to answer, and the lane goes back now rather than at
            // the deadline.
            if (lane.hung_up)() {
                disarm();
                return Parked::HungUp;
            }
            let now = Instant::now();
            let parked = self.stretch.parked();
            // Parked, only the stretch bound ends the window.
            let closes = match self.stretch {
                Stretch::Window { bound, .. } => {
                    match (parked, parked_since) {
                        (true, None) => parked_since = Some(now),
                        (false, Some(since)) => {
                            // The window resumes with what it had left when
                            // the park began.
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
            // However the park ends, the loop head asks again: a ring, the
            // instant, a hang-up or a flip of the update park nothing rang.
            let flipped = || self.stretch.parked() != parked;
            let _ = crate::park::park(|bound| self.sub.wait(bound), wake, lane, &flipped);
        }
    }
}

/// The look of a park on one armed watcher `id`: under one terminal guard,
/// fire the idle clocks that are due (held while the reader is parked), poll
/// `id`, and disarm it there when it latched or the window is closing. Answers
/// the content seq it latched at.
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

/// What a `turn` press's verification park saw ([`press_look`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PressLook {
    /// The watcher latched: content advanced past the press.
    Latched,
    /// The window closed with the shell's started-command counter past its
    /// value at the press.
    Started,
}

/// The look of a `turn` press's verification park on watcher `id`:
/// [`watcher_look`]'s, and in block mode (`base`, the started-command counter
/// at the press) the verdict as the window closes, read under the guard that
/// disarms `id`: a counter past `base` is [`PressLook::Started`]. An OSC
/// 133/633 C moves no grid content, so no latch wakes the park for one; the
/// window's close is the read that sees it, so a command that prints nothing
/// in the window still verifies its press.
fn press_look(
    term: &Arc<Mutex<Terminal>>,
    id: aterm_core::terminal::WatchId,
    base: Option<u64>,
) -> impl FnMut(std::time::Instant, bool, bool) -> (Option<PressLook>, Option<std::time::Instant>) + '_
{
    move |now, parked, closing| {
        let mut t = term_lock(term);
        fire_idle_unless_parked(&mut t, now, parked);
        let latched = t.watch_poll(id).is_some();
        if latched || closing {
            t.watch_disarm(id);
        }
        let started = closing && base.is_some_and(|base| t.started_command_seq() > base);
        let answer = if started {
            Some(PressLook::Started)
        } else if latched {
            Some(PressLook::Latched)
        } else {
            None
        };
        (answer, t.watch_next_deadline())
    }
}

/// The exit-or-hang-up disarm of a park on watcher `id`.
fn disarm_watch(
    term: &Arc<Mutex<Terminal>>,
    id: aterm_core::terminal::WatchId,
) -> impl FnOnce() + '_ {
    move || term_lock(term).watch_disarm(id)
}

/// How a wait for the ribbon to exhale ended. `Below` and `Deadline` carry
/// the last reading so the reply can say where the ribbon stood.
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

/// Classify a fresh read before another timer is armed. A satisfied reading
/// keeps precedence; an unmet reading cannot re-park beyond the backstop even
/// if the previous crossing latched immediately (or the GUI reply was delayed).
/// The bounded guard contract is `momentum_wait_read_model`.
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

/// The yield: park until the target's typing momentum has decayed to `floor`, analytically:
/// one reading, one crossing solved from the release law (`TypingMomentum::reading_low_crossing`),
/// one kernel timer ([`WatcherSpec::MomentumBelow`]), no polling. When the timer fires the
/// metric is read again: at or under the floor the wait is over; above it the person re-lit the
/// ribbon, and the next crossing is solved from that reading. The person's typing is never
/// touched. Bounded by `until` and session exit like every park.
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
        // Preserve the shared park's exit priority for an unmet reading.
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
            // The crossing passed: confirm on a fresh reading (the loop head),
            // which is also where a re-lit ribbon gets its next crossing.
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

/// `await momentum <floor> [timeout <ms>|timeout=<ms>]` — block until the typing momentum of
/// the window hosting this session (`trail status momentum=`) has decayed to `floor` (0..=1),
/// then `OK momentum <v>`; `OK timeout` past the deadline (default 30 s), `ERR exited` if the
/// session dies. An agent waits on it for the person's ribbon to exhale before typing into a
/// shared session ([`park_until_momentum_below`]).
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

/// What one `submit=guarded:<re>` press decided.
enum GuardedSubmit {
    /// The guard matched and Enter was written in full.
    Pressed,
    /// The cursor's row did not match the guard: nothing was written.
    Missed,
    /// The write itself failed — the verb's reply, already worded.
    Failed(String),
}

/// One guarded submit press: Enter, only while the CURSOR's row matches
/// `guard`, through [`super::control_input::input_if_fenced_observed`] (check and write
/// under one terminal lock hold, the immediate non-parking write). The cursor
/// row, not any row: Claude Code draws every earlier prompt in its transcript
/// with the composer's `❯` at column 0, so an any-row `^❯` still matched with
/// an approval box up and its Enter chose the focused `❯ 1. Yes` (measured live
/// on a private headless aterm). A composer holds the cursor; the box does not.
/// A sink that takes zero bytes right now (the typed text's own paste still
/// draining ahead of it) is retried every few milliseconds until `window` — the
/// press has not happened yet, so a retry cannot double it; the guard is asked
/// again each time.
///
/// Ordering is argued, not enforced: the text went through the App input seam
/// (`PasteFraming::AtDrain`) and this Enter goes straight to the sink. It follows the text
/// because the caller presses only after the echo settled, and the immediate write refuses
/// (`BusyZero`, retried) while a spilled frame is queued. The gap is the echo cap: a target
/// never echo-quiet within it is pressed anyway, and could get this Enter before the text.
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
            decision @ GuardedInput::Pressed(_) => {
                return GuardedSubmit::Failed(super::control_input::guarded_input_reply(decision));
            }
        }
    }
}

/// `turn`'s usage line.
pub(crate) const TURN_USAGE: &str = "ERR usage: turn [idle=<ms>] [timeout=<ms>] [submit=<key|none|guarded:<re>>] [settle=match:<re>|gone:<re>|submitted] [submit_window=<ms>] [presses=<n>] [submit_verify=<auto|seq|block>] [trim=<0|1>] [typed=<0|1>] [cadence=<ms>] [cap=<n>] [yield=<floor>] [if-gen=<epoch>.<seq>] [if-rows=<first>-<last>:<hex16>] [cause=@<off>|<seq>] <text>\n";

/// `turn`'s options as parsed ([`turn_options`]): every leading `k=v`, and the
/// message after them, verbatim.
pub(crate) struct TurnOptions<'a> {
    idle_ms: u64,
    /// The whole turn's budget (`timeout=`, else 240 s), which a drive's
    /// gate holds to its connection's (`control::drive_turn`).
    pub(crate) timeout_ms: u64,
    submit: String,
    submit_guard: Option<String>,
    submit_window_ms: u64,
    presses: u32,
    submit_verify: Option<bool>,
    settle_match: Option<(bool, String)>,
    /// `settle=submitted`: no settle — the turn answers at its verified
    /// submit (`status=submitted`).
    settle_submitted: bool,
    trim: bool,
    typed: bool,
    cadence_ms: u64,
    typed_cap: usize,
    yield_floor: Option<f32>,
    if_gen: Option<crate::control::ScreenGen>,
    if_rows: Option<aterm_types::control_verbs::RowsFence>,
    /// A `cause=` among the options, unchecked ([`turn_preflight`] checks it).
    pub(crate) cause: Option<crate::session_log::CauseRef>,
    text: &'a str,
}

/// PARSE `turn`'s options (pure: no lock, no session): the leading `k=v`
/// options, the message after them verbatim, and every refusal of their shape
/// — a bad value, a repeated `cause=`, a turn with no text that still submits.
/// Shared by [`cmd_turn`] and [`turn_preflight`], so the dispatch's
/// check before the exactly-once claim and the turn itself read one grammar.
///
/// # Errors
///
/// The reply: [`TURN_USAGE`], or `cause=`'s own usage.
pub(crate) fn turn_options(rest: &str) -> Result<TurnOptions<'_>, String> {
    const USAGE: &str = TURN_USAGE;
    // ── grammar: leading k=v options, the remainder verbatim is the message ──
    let mut idle_ms = 1_500u64;
    let mut timeout_ms = 240_000u64;
    let mut submit = "enter".to_string();
    // `submit=guarded:<re>`: Enter, pressed only while the cursor's row matches; `None` is an
    // ordinary press.
    let mut submit_guard: Option<String> = None;
    // `submit_window`: how long one press has to verify before a re-press; `presses`: the most
    // presses in all.
    let mut submit_window_ms = 2_000u64;
    let mut presses = 3u32;
    // `submit_verify`: `Some(true)` is `block`, `Some(false)` `seq`, `None` auto.
    let mut submit_verify: Option<bool> = None;
    // `settle=match:<re>` or `settle=gone:<re>` (the `gone` flag); `None` is the idle settle.
    let mut settle_match: Option<(bool, String)> = None;
    // `settle=submitted`: no phase-3 settle — the turn answers at its verified
    // submit (`status=submitted`).
    let mut settle_submitted = false;
    // `trim=1` drops the settled screen's trailing blank rows from the reply (the
    // `text trim` rule); off by default so a script that counts rows sees the grid.
    let mut trim = false;
    // The even hand: `typed=1` with its `cadence=` and `cap=`, and `yield=<floor>`.
    let mut typed = false;
    let mut cadence_ms = TYPED_CADENCE_MS;
    let mut typed_cap = TYPED_CAP;
    let mut yield_floor: Option<f32> = None;
    // `if-gen=<epoch>.<seq>`: type only while the screen is still the generation the caller
    // judged, checked after the yield, just before the paste.
    let mut if_gen: Option<crate::control::ScreenGen> = None;
    // `if-rows=<first>-<last>:<hex16>`: the narrow fence, type only while those rows still
    // hash to the value; checked with `if-gen=` under the same lock hold.
    let mut if_rows: Option<aterm_types::control_verbs::RowsFence> = None;
    // `cause=@<off>|<seq>` (phase 3A, ruling R6): in any position among the
    // options, once (with a leading one too, the dispatch refuses the pair),
    // checked against what this session holds before any byte is typed.
    let mut cause_opt: Option<crate::session_log::CauseRef> = None;
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
            _ => break, // not an option: the message itself starts with `word=…`
        }
        text = tail;
    }
    let timeout_ms = timeout_ms.min(600_000);
    // An empty turn would press Enter with nothing typed (that is `key enter`), so it is a
    // usage error unless it submits nothing either (`submit=none`, a settle read).
    if text.trim().is_empty() && submit != "none" {
        return Err(USAGE.to_string());
    }
    // `settle=submitted` answers at the submit: a turn that submits nothing has
    // none to answer at.
    if settle_submitted && submit == "none" {
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

/// The patterns a turn's options name (`settle=match:|gone:<re>`,
/// `submit=guarded:<re>`), compiled (untrusted regexes, bounded by
/// `row_matcher`): `ERR badregex` before anything is typed.
type TurnMatchers = (
    Option<(bool, Arc<dyn aterm_core::terminal::RowMatch>)>,
    Option<Arc<dyn aterm_core::terminal::RowMatch>>,
);

fn turn_matchers(o: &TurnOptions<'_>) -> Result<TurnMatchers, String> {
    // Compile a `settle=match:<re>`/`settle=gone:<re>` pattern up front (untrusted
    // regex, bounded by `row_matcher`) OUTSIDE the terminal lock and BEFORE the
    // lease/paste, so a bad pattern fails fast without ever typing into the target
    // or holding the lease.
    let settle_matcher = match &o.settle_match {
        Some((gone, pat)) => match aterm_observe::row_matcher(pat) {
            Ok(m) => Some((*gone, m)),
            Err(_) => return Err("ERR badregex\n".to_string()),
        },
        None => None,
    };
    // The `submit=guarded:<re>` guard, compiled on the same terms and for the
    // same reason: a bad pattern must fail before anything is typed.
    let submit_matcher = super::control_input::compile_guard(o.submit_guard.as_deref())?;
    Ok((settle_matcher, submit_matcher))
}

/// Every refusal a `turn` can make of its own words, made before the
/// exactly-once claim (`pty_idem::guarded`): the options' grammar, a bad
/// pattern, and the `cause=` among them. Only an attempt that reached the PTY
/// seam may consume an `id=` sequence: a refused turn whose key was claimed
/// would answer its corrected retry `ERR in-doubt`, though nothing was typed.
/// Returns the `cause=` among the options, unchecked: the dispatch checks it
/// (authority, then what the session holds) beside a leading one.
///
/// # Errors
///
/// The reply the turn itself would have answered.
pub(crate) fn turn_preflight(rest: &str) -> Result<Option<crate::session_log::CauseRef>, String> {
    let o = turn_options(rest)?;
    turn_matchers(&o)?;
    Ok(o.cause)
}

/// `turn [idle=<ms>] [timeout=<ms>] [submit=<key|none|guarded:<re>>]
/// [settle=match:<re>|gone:<re>|submitted] [submit_window=<ms>] [presses=<n>] [trim=<0|1>]
/// [typed=<0|1>] [cadence=<ms>] [cap=<n>] [yield=<floor>] <text>` — one complete human
/// turn against the target session: type `<text>`, submit it with a real keypress,
/// block until the app's response settles, and return the settled screen —
/// `OK <rows> turn submitted=<0|1> status=<settled|timeout|hangup|submitted> seq=<n> id=<n>
/// dur_ms=<n> hash=<hex16>[ trimmed=<k>]` then `<rows>` text rows (the same rows
/// `text` prints). `hash` is the FNV-1a of the settled screen (the same value
/// `history` reports for this `id`), so a driver can diff two turns' outputs inline
/// without a second call. `trim=1` drops the settled screen's trailing all-blank
/// rows exactly as `text trim` does — `<rows>` is then the count sent and
/// `trimmed=<k>` closes the verdict — while `hash` stays over the untrimmed screen:
/// it names the screen, not the payload, so a trimmed and an untrimmed read of the
/// same settled screen hash alike and `history` agrees with both.
///
/// Why a composite verb: driving one turn out of `paste` + `key enter` +
/// `await idle` from a client is racy at the submit seam — a TUI line editor
/// that is still ingesting a large paste burst (agent CLIs bracket-paste big
/// input into a "[Pasted text]" chip) swallows an Enter that arrives during
/// ingestion, and the client cannot see when ingestion ended. Server-side, the
/// Observation Kernel can: this verb parks on the same model-checked watcher
/// primitives `await` uses and only presses submit after the echo settled, then
/// verifies the press landed within `submit_window` (default 2000ms) and re-presses
/// up to `presses` times (default 3) before giving up. `presses=1` disables re-press
/// for a slow/high-latency link where a second Enter could auto-confirm a following
/// prompt. How the press is verified is `submit_verify=`: `block` keys on the OSC-133
/// command-start (a command block enters Executing) — ambient-repaint-immune, so a
/// periodically-repainting TUI cannot false-verify a swallowed Enter; `seq` keys on a
/// bare `content_seq` advance. The default is auto: `block` when the target is at a
/// shell prompt (a submit will start a command — the sound choice); the composer for
/// a session read as Claude Code whose framed composer shows the typed text — its
/// echo waited for as the screen moves, within the echo's cap
/// ([`composer_key_once_shown`]) — the text leaving the composer (its dim
/// placeholder no text), or a box or menu taking its place by the window's close
/// ([`composer_after_press`]), which no repaint elsewhere gives (its animated
/// logo, a ticking row); else `seq` (a full-screen TUI or a session with no shell
/// integration has no press-attributable signal). So a shell drive and a Claude Code
/// drive are sound by default. Auto additionally degrades
/// honestly: when the window expires with content having moved after the press but
/// no command block started, the prompt-shaped block auto keyed on has proven
/// stale (a desynced 133 stream — e.g. a distro integration whose C mark never
/// lands), so auto takes the seq verdict (`submitted=1`) instead of re-pressing —
/// a press whose echo moved the screen was consumed, and another Enter would land
/// in the running command. Re-press is reserved for a press after which nothing
/// moved (the true swallowed case) — or, verified by the composer, after which the
/// text still sits there. Explicit `submit_verify=block` never degrades, and
/// neither does the composer: a repaint is exactly what it must not take for a
/// landed Enter.
/// The whole exchange is one request line — no client-side timing enters the
/// protocol.
///
/// The gesture vocabulary is app-agnostic on purpose: nothing here knows what
/// program is driven. Claude Code, codex, a shell, emacs (`submit=none` types
/// without submitting; any `key`-verb name may be the submit key) — anything in
/// a PTY holds a conversation through the exact surface a human uses. For an
/// ordinary interactive `turn`, a human typing into the target mid-turn simply
/// extends the same content stream: the settle watcher keeps resetting, and the
/// returned screen contains whatever both parties produced.
///
/// Settle (phase 3) defaults to global idle — no content change for `idle_ms` —
/// which assumes the app stops painting when done. A periodically-repainting TUI
/// (a status-bar clock, a spinner, `watch -n1`) never goes idle, so an idle-settle
/// turn against it burns the full timeout; pass `settle=match:<re>` to key settle
/// on a screen pattern instead (the same regex/`RowRange` machinery as
/// `await match`), `settle=gone:<re>` to key it on that pattern leaving the
/// screen (`await gone` — a busy footer such as Claude Code's `esc to interrupt`
/// disappearing), or lower `idle=` below the repaint cadence.
///
/// `settle=gone:` is two waits, not one, because `await gone` is level-triggered:
/// arming it on a screen the busy footer has not painted yet would latch at arm
/// and report `status=settled` over the pre-response screen — and the verified
/// submit can be satisfied by the input line clearing one frame before the footer
/// lands. So phase 3 first waits (bounded by `submit_window`, the same window the
/// submit had to move the screen) for `<re>` to appear after the verified submit,
/// and only then for it to leave. A pattern that never appears inside that window
/// degrades to the default idle settle: a wrong pattern, or a turn too quick to
/// show its footer, behaves like a plain `turn` instead of settling instantly on
/// a screen nothing was read from. A caller who needs certainty follows the turn
/// with `await gone <re>` while the footer is up.
///
/// `settle=submitted` waits for no settle at all: the turn answers the moment its
/// submit is verified, `status=submitted` over the screen as it stands then. For a
/// program whose screen never goes quiet and has no pattern to key on — Claude Code
/// 2.1.301 animates its header logo's colours every few hundred milliseconds at an
/// idle prompt, so an idle settle burns the whole timeout — and for a caller with its
/// own wait to follow, as a manager's follow-up into a worker has (`await task`).
/// Everything the submit does happens as ever (a done worker's task opens again
/// here). An unverified submit answers `status=timeout` as any turn does; with
/// `submit=none` there is no submit to answer on, and the pair is the usage line.
/// The ledger records the turn as `status=pressed`: its Enter landed and no settle
/// was awaited after it. Into Claude Code auto verifies by the composer (above),
/// which its animation cannot satisfy; at another program's prompt by `seq`, which
/// a repaint of its own can: `submitted=1` there says the screen moved, not that
/// the Enter landed.
///
/// A turn into an agent — a session whose published verdict is an agent's word,
/// not `-` — closes its verdict with `agent_rev=<n>`, the verdict's rev as the
/// Enter found it: `await agent <words> since=<n>` answers only on a verdict
/// published after it, so a wait armed the moment a `settle=submitted` turn
/// answers cannot latch the `idle` from before the submit, which the status
/// sweep (at most every `AGENT_MIN_INTERVAL`) has not yet republished as busy.
///
/// A worker's turn — into a session with a task record — closes its verdict with
/// `task_rev=<n>`, the record's rev as the Enter found it: `await task
/// @<sid>:<n>` waits past it, so the `working` this turn moves a done record to is
/// no news there and the follow-up's own `done` is, even one marked before the
/// wait was armed; and that done is the one the turn opens again
/// ([`crate::task_record::reopen_at_turn`]), never one marked since.
///
/// Fully event-driven like `ready`/`await` (subscriber park + kernel deadlines,
/// zero polling). Empty `<text>` skips the paste (submit-only: useful to fire a
/// chip already sitting in the target's editor); `submit=none` skips the press
/// (type-only). `ERR exited` if the target dies at any phase.
///
/// `submit=guarded:<re>` submits with Enter, and presses it
/// only while the row holding the cursor still matches `<re>` — checked and
/// written under one terminal lock hold, the `key if=` delivery — so a driver
/// that typed into a composer does not submit into whatever replaced it (an
/// approval box that arrived during the paste settle). The cursor row because
/// that is where a submit lands: a transcript row above the box can match the
/// composer's pattern too (see [`guarded_submit`]). A miss on the first press
/// types the text and submits nothing: `OK 0 turn skipped reason=guard
/// submitted=0 seq=<n> id=<n>`, zero rows, the verb's Lines framing. A miss on
/// a re-press ends the presses and the turn reports the first press by its
/// verification, as ever. A guarded turn's verdict also carries `pressed=<n>`,
/// the Enters actually written: `submitted=0 pressed=1` is an Enter that was
/// written and not verified — not "no Enter", and not a turn to retry blindly.
///
/// The even hand (`RAINBOW-KITTY-V2.md` §28): a plain turn is a paste (one
/// event, no keystroke behind any cell, so it lays no ribbon, earns no
/// stardust and steps no melody). `typed=1` presses `<text>` one `InputEvent::Key` per
/// grapheme through the same route a keyboard's presses take, at `cadence=<ms>`
/// (default [`TYPED_CADENCE_MS`]), from an absolute schedule so the interval
/// never drifts: an agent's typed turn lays real ribbon and earns real
/// stardust, and its only signature is time — a jitter-free cadence no hand
/// produces. The seam is told nothing else ([`typed_key_events`] builds the
/// tuple a keyboard builds; `input.rs`'s indistinguishability invariant is
/// untouched). The typed prefix is capped at [`TYPED_CAP`] graphemes (`cap=<n>`,
/// up to [`TYPED_CAP_MAX`], raises it) and the remainder is one paste, voiced as
/// one up-strum by the paste seam. A typed text is never submitted in part:
/// a head whose schedule would end past the deadline is not begun, one the
/// deadline overtakes stops there, and a fenced one (`if-gen=`/`if-rows=`)
/// stops at a person's key or a screen switch counted between its keys, and an
/// `if-rows=` one at the cursor leaving the frame those rows are
/// ([`TypingHold`]) — each pressing nothing more, Enter included, and
/// answering `OK 0 turn skipped reason=<deadline|person|changed> typed=<k>
/// pasted=<p> submitted=0 seq=<n> id=<n>`, the keys already typed left where
/// they landed.
/// `yield=<floor>` parks before typing until the target's typing momentum has
/// exhaled to `floor` ([`park_until_momentum_below`]: analytic, one timer per
/// reading, no polling); the human's own typing is never blocked — the agent
/// yields, the human never waits. A yield that outlives the deadline answers
/// `ERR yield timeout momentum=<v>` having typed nothing. The verdict gains
/// `typed=<keys> pasted=<graphemes>` for a typed turn and `yielded_ms=<n>`
/// for a yielded one; a plain turn's reply is byte-identical to before.
///
/// A hold set while the turn runs (`hold`, the person's Take Over) is asked
/// again before each key, paste and Enter: one that stands before any byte
/// landed answers the dispatch's own `ERR halted`, and one after stops the
/// turn there, pressing nothing more, Enter included — `OK 0 turn skipped
/// reason=halted submitted=0 seq=<n> id=<n>`, a typed turn's with `typed=<k>
/// pasted=<p>`.
pub(crate) fn cmd_turn(
    term: &Arc<Mutex<Terminal>>,
    store: &Store,
    session: u64,
    rest: &str,
    ctx: &SessionCtx,
    io: &TurnIo<'_>,
) -> String {
    use std::time::{Duration, Instant};

    use aterm_core::terminal::{BlockState, WatcherSpec};

    const USAGE: &str = TURN_USAGE;
    /// Echo-settle window: the paste burst has been ingested and painted.
    const ECHO_SETTLE: Duration = Duration::from_millis(150);
    /// Echo phase cap — a busy app (spinner mid-turn) may never go echo-quiet;
    /// after this we press anyway rather than stall the whole turn.
    const ECHO_CAP: Duration = Duration::from_secs(5);

    // ── grammar: leading k=v options, the remainder verbatim is the message ──
    // ([`turn_options`], the grammar the dispatch's pre-claim check reads too).
    let o = match turn_options(rest) {
        Ok(o) => o,
        Err(refusal) => return refusal,
    };
    // `cause=`: a leading one or one among the options, which the dispatch
    // took and checked before the exactly-once claim (`io.cause`); one only
    // among the options of a caller that checked nothing is checked here,
    // before any byte is typed.
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

    // ── turn lease: one driver per session. Acquire-or-busy here is authoritative (the
    // dispatch's check is the fast fail); the drop guard releases it on every exit.
    let (turn_id, handed_back) = {
        let mut lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
        let now = crate::metrics::now_us();
        // A live lease of either kind refuses a new turn, unless it is this connection's own
        // drive lease: the holder types its turn under it and gets it back after (the live
        // upgrade's relaunch line, under the restart's hold). An expired drive lease is free.
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
        // The ledger owes this id a record from here (or its abandonment, by the guard): a
        // handoff exporting the ledger meanwhile does not vouch for it
        // (`TurnLedger::carry_floor`). Minted under the ledger's lock, the one
        // `handoff_carry::export` takes, so an export either sees the id open or ran before it;
        // a turn begun after the export records only in this process.
        let id = {
            let mut turns = ctx.turns.lock().unwrap_or_else(|p| p.into_inner());
            let id = mint_turn_id();
            turns.begin(id);
            id
        };
        // The lease names its issuer — the dispatch site's resolution of the
        // connection's scope ([`TurnIo::driver`]) — so `status hand=` and the
        // tab's hover credit the hand that is actually on the keyboard.
        *lease = Some(crate::Lease::Turn {
            id,
            driver: io.driver.clone(),
            // The driver may type from here to its last Enter
            // ([`crate::Lease::driver_may_type`]): the repaint nudge resizes
            // nothing under it until [`end_turn_input`].
            typing: true,
        });
        (id, own)
    };
    // Who typed it, for the ledger: the caller, or the window harness when
    // the drive lease this connection types under is the harness's hand.
    let by = typed_by(&io.by, handed_back.as_ref());
    struct LeaseGuard<'a> {
        /// `ctx.turn_lease`, a named field so the lock-order census resolves this Drop-path
        /// acquisition to the same `turn_lease` identity as the acquire above.
        turn_lease: &'a std::sync::Mutex<Option<crate::Lease>>,
        /// `ctx.turns`, NAMED for the census like `turn_lease`: the ledger
        /// this turn's id was opened in.
        turns: &'a std::sync::Mutex<crate::turn_ledger::TurnLedger>,
        /// This turn's id: the guard clears the slot only while it still holds this turn's
        /// lease, so a fresh turn after a `lease release force` preemption is not stomped.
        id: u64,
        /// The session's fabric id, for the presence wake the release posts.
        sid: &'a aterm_session::SessionId,
        /// The driving session, if a local one: its own band's `▸ @<sid>`
        /// reads this lease too, so it is woken with the driven session.
        driver: Option<&'a aterm_session::SessionId>,
        /// The drive lease this turn's own connection held when it began:
        /// handed back as the turn ends.
        handed_back: Option<crate::Lease>,
    }
    impl Drop for LeaseGuard<'_> {
        fn drop(&mut self) {
            // A turn that ends without its record owes none any more (a no-op once
            // `TurnLedger::record` took it). Taken and released before the lease, never under it.
            self.turns
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .abandon(self.id);
            let mut lease = self.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
            if matches!(lease.as_ref(), Some(crate::Lease::Turn { id: held, .. }) if *held == self.id)
            {
                *lease = self.handed_back.take();
                drop(lease);
                // The turn settled (or timed out, or was refused): the rim lifts.
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
    // The turn's lease is held: both sessions' presence facts are re-read
    // (`status hand=` names the driver; a lease is a record, never a need,
    // so nothing is drawn for it).
    crate::presence::post_lease_changed(&ctx.self_id);
    if let Some(driver) = &io.driver {
        crate::presence::post_lease_changed(driver);
    }

    // Attribution before delivery: the turn this exchange opens carries `by=` (the
    // connection's, `TurnIo::actor`) and `ledger=<id>`; a turn at rest closes. Noted when
    // the exchange's input is handed to the seam — its first key or paste,
    // after the yield and the `if-gen=`/`if-rows=` check, and again at each
    // Enter, its last byte — so the tracker's 5 s attribution window runs
    // from the exchange's own input: a person's keys during the yield cannot
    // take the turn, and a typed text longer than the window still opens it
    // as this exchange's. The timeline's leaf lock is taken there with no other
    // held (the ledger and lease locks above are released, and no terminal
    // guard lives across a delivery): it is held under none.
    let slog_by = io.actor;
    let note_ctl_input = || {
        let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        let at = timeline.slog().now();
        let _ = timeline.slog_mut().ctl_turn(slog_by, turn_id, cause, at);
        // The log's owed wake rings the session's watchers in this hold (the
        // resting turn may have closed). The separate Input mark comes only
        // from an accepted nonempty receipt.
        let _ = timeline.slog_take_wake();
    };

    let now0 = Instant::now();
    let started_ms = crate::turn_ledger::now_ms();
    let deadline = now0 + Duration::from_millis(timeout_ms);
    // The alt-screen archive's mark as the turn starts, before the yield and the first byte, so
    // `offscreen since=<arch>` reads everything a fullscreen app scrolls off its top from here,
    // this turn's own echo included. Recorded in the ledger and printed by `history`.
    let arch = crate::turn_ledger::ArchMark::of(&term_lock(term));

    // Exit detection as in `await` ([`exit_probe`]).
    let exited = exit_probe(store, session);

    // One subscriber for the whole turn: output/exit notifies wake every phase.
    let sub = crate::session_watch::Subscription::single(&ctx.watchers);

    // Park until watcher `id` latches, `until` passes, or the session exits —
    // [`Park`], the one wait every phase shares. Disarms on every path.
    // `wait` gives the lane back when the caller hangs up; `wait_committed` is
    // for the phases between the first typed byte and a verified submit,
    // which a hangup must not cut short (see [`OnHangup::Finish`]).
    let exited_now = || exited(store);
    // Both hold their clocks while the session's reader is parked for an
    // update, stretched never past the turn's own deadline.
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
    // Arm a watcher; None (budget full) fails the whole verb honestly.
    let arm = |spec: WatcherSpec| -> Option<aterm_core::terminal::WatchId> {
        term_lock(term).watch(spec, Instant::now())
    };

    // ── phase 0: yield until the person's ribbon has exhaled to the floor, before any input.
    // Nothing is typed on a timeout.
    let mut yielded_ms: Option<u64> = None;
    if let Some(floor) = yield_floor {
        let yield0 = Instant::now();
        match park_until_momentum_below(term, &exited_now, &sub, io.momentum, floor, deadline) {
            MomentumWait::Below(_) => yielded_ms = Some(yield0.elapsed().as_millis() as u64),
            MomentumWait::Deadline(v) => return format!("ERR yield timeout momentum={v:.2}\n"),
            MomentumWait::Exited => return "ERR exited\n".to_string(),
            // Nothing is typed before the yield ends: no turn to record.
            MomentumWait::HungUp => return crate::control::HUNG_UP_REPLY.to_string(),
            MomentumWait::Unreadable(e) => return format!("ERR yield: {e}\n"),
        }
        // The dispatch asked the unread-input gate when this turn arrived; the yield can
        // outlast the moment the program is published as stalled, so ask again before the
        // first byte (a slow read is only a warning, and the dispatch's is the one reported).
        if let Some(refusal) = crate::input_stall::refusal(ctx, "turn", "", false) {
            return refusal;
        }
    }

    // ── the precondition: `if-gen=`. The caller judged a screen and asked to type into it;
    // any output since (an echoed keystroke, a box, a repaint) moved the generation, and
    // nothing is typed. Checked after the yield (a person typing during it moves it too), under
    // the terminal lock, just before the paste goes to the seam: only the seam's hand-off stays
    // open. `if-rows=` is the same check over the named rows only, so a spinner or timer
    // elsewhere does not block every notice. Both, and the [`TypingHold`] a typed turn is held
    // to, are read under one lock hold. ──
    let mut hold: Option<TypingHold> = None;
    if if_gen.is_some() || if_rows.is_some() {
        // The rows are the session's published text, looked at before the lock.
        let mut look = ctx.frame.look_frame();
        let terminal = term_lock(term);
        let moved = if_gen
            .is_some_and(|generation| crate::control::screen_gen(&terminal) != generation)
            || if_rows.is_some_and(|fence| {
                look.text(&terminal).rows_hash(fence.first, fence.last) != Some(fence.hash)
            });
        if moved {
            let seq = terminal.content_seq();
            return format!(
                "OK 0 turn skipped reason=changed submitted=0 seq={seq} id={turn_id}\n"
            );
        }
        hold = typed.then(|| TypingHold::of(&terminal, ctx, if_rows));
    }

    // The holds, asked again at every write. The dispatch asked them once,
    // as the turn arrived ([`crate::fabric::halt_refusal`]); a turn then
    // types for as long as its budget lets it, so a hold set since — the
    // person's Take Over above all, which takes the tab from a remote node
    // mid-turn — is asked again before each key, the remainder's paste, the
    // paste and every Enter, under the same rule (a remote node's own hold
    // never refuses its own drive). Before any byte landed (`landed` is
    // `None`) the turn answers the dispatch's own `ERR halted`; after, it
    // stops there, `reason=halted`, pressing nothing more — the Enter above
    // all, which would submit the part typed, or the person's keys with it.
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

    // ── phase 1: type. Paste semantics by default; the seam strips control
    // bytes. `typed=1`: one key per grapheme on an absolute schedule (`t0 +
    // i·cadence`, so the interval never drifts) up to the cap, then the
    // remainder as one paste. ──
    let mut typed_keys = 0usize;
    let mut pasted_graphemes = 0usize;
    let cadence = Duration::from_millis(cadence_ms);
    // From the first byte that lands to the verified submit the turn is committed: a caller
    // that hangs up then stops neither its remaining keys nor its Enter at the main-thread
    // gate, as it ends none of the parks between them ([`OnHangup::Finish`]).
    let mut committed: Option<crate::control::CommittedInput> = None;
    // Where the echo's cap ends: the composer verify waits no later for a
    // Claude Code composer to show the text (phase 2).
    let mut echo_until = None;
    if !text.is_empty() {
        if typed {
            use aterm_grapheme::GraphemeClusters;
            let (head, remainder) = split_typed_cap(text, typed_cap);
            let keys = typed_key_events(head);
            let mut type0 = Instant::now();
            // All or nothing against the deadline: a head whose schedule ends past it is not
            // begun.
            let last_due =
                type0 + cadence * u32::try_from(keys.len().saturating_sub(1)).unwrap_or(u32::MAX);
            if !keys.is_empty() && last_due >= deadline {
                return typed_cut(term, "deadline", 0, 0, turn_id);
            }
            // The cursor the previous key went out from: a digit waits until
            // the caret has moved off it ([`TypingHold::held`]).
            let mut sent_from = None;
            for (i, ev) in keys.into_iter().enumerate() {
                let due = type0 + cadence * i as u32;
                let now = Instant::now();
                if due > now {
                    std::thread::sleep(due - now);
                }
                // The deadline bounds the typing too, and a text cut short there is never
                // submitted: its Enter would send the part as the whole.
                if Instant::now() >= deadline {
                    return typed_cut(term, "deadline", typed_keys, 0, turn_id);
                }
                if exited(store) {
                    return "ERR exited\n".to_string();
                }
                let mut waited = false;
                if let Some(h) = &hold {
                    let digit = is_digit_key(&ev);
                    match h.held(
                        term,
                        ctx,
                        &sub,
                        sent_from.filter(|_| digit),
                        cadence,
                        deadline,
                    ) {
                        Ok((cursor, held)) => {
                            waited = held;
                            sent_from = Some(cursor);
                        }
                        Err(why) => return typed_cut(term, why, typed_keys, 0, turn_id),
                    }
                }
                if let Some(cut) = halted(committed.is_some().then_some((typed_keys, 0))) {
                    return cut;
                }
                if typed_keys == 0 {
                    note_ctl_input();
                }
                if !(io.key)(ev) {
                    return "ERR key delivery failed\n".to_string();
                }
                // A key that waited anchors the schedule on itself, as it went
                // out: the next is one cadence after it, never a burst catching
                // up on the time spent (a torn frame has been held ~280 ms, 140
                // keys at `cadence=2`), nor on a stall between the hold's
                // verdict and the key.
                if waited {
                    type0 += Instant::now().saturating_duration_since(due);
                }
                committed.get_or_insert_with(crate::control::CommittedInput::enter);
                typed_keys += 1;
            }
            if !remainder.is_empty() {
                if let Some(Err(why)) = hold
                    .as_ref()
                    .map(|h| h.held(term, ctx, &sub, None, cadence, deadline))
                {
                    return typed_cut(term, why, typed_keys, 0, turn_id);
                }
                if let Some(cut) = halted(committed.is_some().then_some((typed_keys, 0))) {
                    return cut;
                }
                pasted_graphemes = remainder.graphemes().count();
                if typed_keys == 0 {
                    note_ctl_input();
                }
                if !(io.paste)(remainder) {
                    return "ERR paste delivery failed\n".to_string();
                }
                committed.get_or_insert_with(crate::control::CommittedInput::enter);
            }
        } else {
            if let Some(refusal) = halted(None) {
                return refusal;
            }
            note_ctl_input();
            if !(io.paste)(text) {
                return "ERR paste delivery failed\n".to_string();
            }
            committed = Some(crate::control::CommittedInput::enter());
        }
        // Echo settle: the editor ingested and painted the burst, capped so an app already
        // animating cannot stall the turn.
        let Some(id) = arm(WatcherSpec::IdleFor { dur: ECHO_SETTLE }) else {
            return format!("ERR {WAITS_FULL}\n");
        };
        let until = deadline.min(Instant::now() + ECHO_CAP);
        echo_until = Some(until);
        match wait_committed(id, until) {
            Parked::Exited => return "ERR exited\n".to_string(),
            // A committed park never ends on a hangup (`OnHangup::Finish`):
            // the turn goes on to its submit either way.
            Parked::Done(_) | Parked::Deadline | Parked::HungUp => {}
        }
        // The last gap of a held typing: a person's key during the echo
        // settle would be submitted with the text by the Enter below, and a
        // box drawn there would take it.
        if let Some(Err(why)) = hold
            .as_ref()
            .map(|h| h.held(term, ctx, &sub, None, cadence, deadline))
        {
            return typed_cut(term, why, typed_keys, pasted_graphemes, turn_id);
        }
    }

    // ── phase 2: verified submit. `submitted` means a press verifiably landed; with
    // `submit=none` there is no press, so it stays 0 while the settle still runs.
    let mut submitted = false;
    // Enters the guarded submit actually wrote (`pressed=` on its verdict).
    let mut guarded_presses = 0u32;
    // A worker's task record as the Enter finds it: the rev its verdict's
    // `task_rev=` names — the edge an `await task @<sid>:<rev>` waits past
    // for this turn's own done — and the one entry into its state this turn
    // may open again (`task_record::reopen_at_turn`: that done, re-marked or
    // not since).
    let task_found = task_record_of(ctx).map(|task| (task.rev, task.entered));
    // The agent verdict as the Enter finds it, where the session is an agent's
    // (its word not `-`): the rev its verdict's `agent_rev=` names, the edge
    // an `await agent <words> since=<rev>` waits past — so the `idle` from
    // before this submit, which the status sweep may not have republished yet
    // when a `settle=submitted` turn answers, is no answer to it.
    let agent_found = {
        let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        let agent = tl.agent();
        (agent.word != "-").then_some(agent.rev)
    };
    if submit != "none" {
        // Explicit `submit_verify=` wins; auto picks `block` at a shell prompt, else `seq`, and
        // may degrade below: a prompt-shaped block can be stale under a desynced 133 stream (an
        // integration whose 133;C never lands, such as stock Ubuntu bash sourcing vte.sh
        // twice), where block verification never succeeds though every press lands.
        let auto_mode = submit_verify.is_none();
        let submit_verify_block = submit_verify.unwrap_or_else(|| {
            matches!(
                term_lock(term).all_blocks().last().map(|b| b.state),
                Some(BlockState::PromptOnly | BlockState::EnteringCommand)
            )
        });
        // Block mode verifies a press by the shell's started-command counter
        // (`started_command_seq`) rising past its pre-press value. Each OSC
        // 133/633 C the A→B→C gate accepts bumps it, and no clear, RIS or
        // block-ring eviction takes one back, so a command that started, ran,
        // finished and was archived out of a full ring — or wiped by `clear`'s
        // ED 3 — in the press's own paint still reads as started. An ambient
        // repaint never touches it. (The C, not a new block id: a new id
        // appears only at the NEXT prompt — too late to verify the submit.)
        //
        // The composer verify: where auto keys on `seq` for an identified
        // Claude Code session whose framed composer shows the typed text, the
        // submit is verified by that text leaving the composer instead — a
        // signal no repaint gives: Claude Code 2.1.301 animates its logo at an
        // idle prompt, so a content advance is no proof the Enter landed
        // ([`composer_after_press`]). An echo still to come is waited for as
        // the screen moves, up to the echo's cap; one not shown by then (a
        // composer Claude Code scrolled, a `!` or `#` it took as a mode) keeps
        // `seq`. The read and the seq the next wait arms past are taken under
        // one lock, so an echo drawn between the two is never slept through.
        let composer_key = if auto_mode && !submit_verify_block && read_as_claude(ctx) {
            let look = || {
                let t = term_lock(term);
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
        for press_no in 0..presses {
            // A re-press is for an Enter the program read and swallowed; one still unread would
            // only queue another behind it (`input_stall::repress_would_queue`).
            if press_no > 0 && crate::input_stall::repress_would_queue(&ctx.sink) {
                break;
            }
            // Never a re-press into a parked session: its echo cannot be seen until the reader
            // runs again, so a window without one says nothing about the press — and a second
            // Enter can confirm whatever the program shows next (a permission box's default).
            // The window only closes parked at the turn's own deadline; the press is judged as
            // unverified.
            if press_no > 0 && ctx.update_parked.is_parked() {
                break;
            }
            // A hold set since the text went in: no Enter. A re-press ends
            // the presses, the first judged by its verification, as a
            // guard's miss on one does.
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
            // One hold reads the base and arms BEFORE the press, so the press's
            // own content change latches it (`after` = pre-press seq); a
            // post-press arm would race past it.
            let (base_started, mut id) = {
                let mut t = term_lock(term);
                let base = t.started_command_seq();
                let after = t.content_seq();
                match t.watch(WatcherSpec::SeqAdvanced { after }, Instant::now()) {
                    Some(id) => (base, id),
                    None => return format!("ERR {WAITS_FULL}\n"),
                }
            };
            // The Enter is the exchange's last byte: the turn it starts is
            // attributed from here (the session log, above).
            note_ctl_input();
            if let Some(guard) = submit_matcher.as_deref() {
                // The guarded submit: Enter only while `<re>` still matches,
                // decided and written under one terminal lock hold, on the
                // control thread against this target's own `(term, sink)` —
                // the `key if=` arm's delivery, whichever route typed the text.
                match guarded_submit(term, ctx, guard, window, io.mark) {
                    GuardedSubmit::Pressed => guarded_presses += 1,
                    GuardedSubmit::Missed => {
                        term_lock(term).watch_disarm(id);
                        if press_no == 0 {
                            // Nothing was submitted: the text sits in the
                            // composer, and the driver is told so in the
                            // verb's own Lines framing.
                            let seq = term_lock(term).content_seq();
                            return format!(
                                "OK 0 turn skipped reason=guard submitted=0 seq={seq} \
                                 id={turn_id}\n"
                            );
                        }
                        // A re-press whose guard no longer matches: the screen moved on, so
                        // the first press is judged by the verification below.
                        break;
                    }
                    GuardedSubmit::Failed(reply) => {
                        term_lock(term).watch_disarm(id);
                        return reply;
                    }
                }
            } else if !(io.press)(&submit) {
                term_lock(term).watch_disarm(id);
                return USAGE.to_string();
            }
            // `seq`: the first content advance verifies. `block`: only a new command start does,
            // re-armed past ambient repaints until the window closes. `advanced` separates a
            // swallowed press (nothing moved) from one consumed without starting a command.
            let mut advanced = false;
            // The composer verify's last read: the composer gone from the
            // screen (a box or a menu took its place) rather than still
            // holding the text.
            let mut composer_gone = false;
            loop {
                match Park::new(&sub, &exited_now, OnHangup::Finish, stretch).run(
                    window,
                    press_look(term, id, submit_verify_block.then_some(base_started)),
                    disarm_watch(term, id),
                ) {
                    Parked::Exited => return "ERR exited\n".to_string(),
                    // Block mode read the counter as the window closed: an
                    // OSC 133/633 C changes no grid content, so a C apart
                    // from the Enter's echo, from a command that then prints
                    // nothing in the window (`sleep 3`, `ssh host`), woke no
                    // latch, and the close is where it is seen.
                    Parked::Done(PressLook::Started) => {
                        submitted = true;
                        break;
                    }
                    Parked::Done(PressLook::Latched) => {
                        advanced = true;
                        // One hold judges and re-arms, so content landing
                        // after this read latches the new watcher. In block
                        // mode the counter is the verdict; a content latch
                        // only wakes this loop to read it before the window
                        // closes, and a C that moves no content wakes nothing
                        // and is read at the window's close (below). The
                        // composer verify's read is taken in the same hold:
                        // the composer clearing after it is the next wait's
                        // latch.
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
                        // block mode, ambient repaint only — or the composer
                        // still holding the text: keep waiting in-window.
                        if Instant::now() >= window {
                            break;
                        }
                        let after = t.content_seq();
                        match t.watch(WatcherSpec::SeqAdvanced { after }, Instant::now()) {
                            Some(nid) => id = nid,
                            None => return format!("ERR {WAITS_FULL}\n"),
                        }
                    }
                    // The window closed: with no C, the look read the counter
                    // as it closed. A committed park never ends on a hangup
                    // (`OnHangup::Finish`); were it to, the press is judged
                    // as one whose window closed, never abandoned unrecorded.
                    // The composer verify reads the screen once more as the
                    // window closes: its last word is the screen then, not
                    // the last read a content advance woke.
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
            // AUTO degrade: the window expired with content moving after the press
            // but no command starting — and this stream has never had a C
            // accepted (`started_command_seq() == 0`; a clear or an evicted
            // block does not reset it). A stream that has never once fired
            // 133;C has proven the block signal unavailable (dead or desynced
            // integration), so fall back to the verdict AUTO would have chosen
            // without it — the seq advance. The press provably reached the app;
            // re-pressing here would type a REAL extra Enter into a target that
            // already consumed the first (and a driver told submitted=0 would
            // re-type the whole turn — the double-type this closes). A stream
            // that HAS started commands before keeps the strict re-press:
            // there, no-command-plus-ambient-movement is exactly what a
            // swallowed Enter beside a spinner looks like, and claiming
            // submitted=1 on it would break this fn's "a press VERIFIABLY
            // landed" contract. Explicit `submit_verify=block` stays strict
            // unconditionally: soundness was asked for by name.
            if !submitted && advanced && auto_mode && base_started == 0 && composer_key.is_none() {
                submitted = true;
            }
            // The composer verify's window closed on a screen whose composer
            // the Enter took away (a menu or a box drawn over it, which no
            // repaint of the logo does): the press landed, and another Enter
            // would go to whatever took its place.
            if !submitted && composer_gone {
                submitted = true;
            }
            if submitted || Instant::now() >= deadline {
                break; // verified, or the overall deadline: report honestly
            }
        }
    }

    // The input is over, submitted or not: from here a gone caller's hop is
    // refused again, as the settle below gives its lane back.
    drop(committed);

    // The submit VERIFIABLY landed: the lease's turn moved, so the needs
    // border's facts are re-read (`Wake::LeaseChanged`). Posted once, here,
    // after the verdict — never on the press.
    // A turn that lands in a FINISHED worker opens its task again at once,
    // unless the window harness typed it (`task_record::reopen_at_turn`).
    if submitted {
        crate::presence::post_lease_changed(&ctx.self_id);
        crate::task_record::reopen_at_turn(
            ctx,
            session,
            &by,
            task_found.map(|(_, entered)| entered),
        );
    }

    // The turn has typed its last key: the repaint nudge may resize the
    // session now — HERE, inside the turn, where the settle below waits out
    // the program's repaint — never between the caller's read and the paste,
    // nor between the paste and the Enter. A turn typed under its own connection's drive lease keeps the
    // hold: its holder types on after it. A turn whose submit did not land
    // skips its settle below and releases the lease at once: ending its input
    // first would post a wake that could raise the row only for the release's
    // wake to fold it a moment later — the N → N-1 → N
    // re-grid a program on the alternate screen can miss. It keeps the hold
    // to its release, and the row, if anything else wants it, comes then.
    if lease_guard.handed_back.is_none() && (submitted || submit == "none") {
        end_turn_input(&ctx.turn_lease, turn_id, &ctx.self_id);
    }

    // ── phase 3: the turn settles — no content change for `idle_ms`. ──
    // An unverified submit skips the settle wait (waiting `idle_ms` for a turn
    // that never started would just burn the deadline) and reports honestly.
    // A caller that hangs up here gets its lane back at once (`wait` is
    // [`OnHangup::Abort`]), but its turn was typed and submitted, so it is
    // still recorded below, as `status=hangup`: the ledger is what `history`,
    // the events digest and the harness's report reader read back. The verdict says
    // `status=hangup` too.
    let mut status = "timeout";
    if settle_submitted && submitted {
        // `settle=submitted`: the verified submit is the answer.
        status = "submitted";
    } else if submitted || submit == "none" {
        'settle: {
            let armed = match &settle_matcher {
                Some((true, m)) => {
                    // `gone` is level-triggered: armed before the footer paints (the verified
                    // submit can be the input line clearing a frame early) it would latch over
                    // the pre-response screen. So 3a waits for the pattern to appear and 3b for
                    // it to leave; never seen within `submit_window`, the idle settle.
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

    // ── reply: the settled screen, framed like `text` with turn metadata on the
    // status line (the client streams `OK <count> …` + count rows). ──
    let mut look = ctx.frame.look_frame();
    let (seq, screen) = {
        let t = term_lock(term);
        // One construction, shared with `status`'s stamp: the session's published
        // text ([`crate::frame_memo::ScreenText`]), built here only when no reader
        // built this screen's already.
        (t.content_seq(), look.text(&t))
    };
    // Published now, for the reads of this settled screen that follow.
    drop(look);
    let rows = screen.row_count();
    // Record this exchange in the session turn ledger — read back by `turns`,
    // streamed by the `subscribe … events` digest. Drop-oldest + bounded; the
    // ledger lock is disjoint from the terminal lock (released just above).
    // Computed once and shared by the ledger record and the verdict line below, so
    // the inline `dur_ms=`/`hash=` an AI diffs by (the catalog + INTROSPECTION.md
    // promise them) are the same values `history` later reports for this id.
    let dur_ms = now0.elapsed().as_millis() as u64;
    let screen_hash = screen.fnv();
    // `record` keeps the ids rising in landing order: a turn a `lease release
    // force` preempted, finishing after the one that took its slot, is
    // re-numbered, and its verdict prints the id it was recorded under.
    let turn_id = {
        let mut ledger = ctx.turns.lock().unwrap_or_else(|p| p.into_inner());
        let record = crate::turn_ledger::TurnRecord {
            id: turn_id,
            started_ms,
            dur_ms,
            submitted,
            // A turn answered at its submit awaited no settle: the ledger's word
            // for an Enter that landed with nothing awaited after it.
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
        // The record wakes the session's watchers: an `events` subscriber scans
        // it at once.
        ledger.record(record, mint_turn_id)
    };
    // `trim=1` drops the settled screen's trailing blank rows and closes the verdict
    // with `trimmed=<k>` — the same rule and framing as `text trim`, through the one
    // shared framer. `screen_hash` above was taken over the untrimmed screen on
    // purpose: it is the settled screen's identity — the value `history` reports for
    // this id and the one a driver diffs across turns — not a checksum of the bytes
    // sent, so the same settled screen hashes the same whether or not the caller
    // asked for its blank tail.
    let mut verdict = format!(
        "turn submitted={} status={status} seq={seq} id={turn_id} dur_ms={dur_ms} hash={screen_hash:016x}",
        u8::from(submitted)
    );
    // The even hand's fields ride only the turns that asked for them.
    if typed {
        use std::fmt::Write as _;
        let _ = write!(verdict, " typed={typed_keys} pasted={pasted_graphemes}");
    }
    if let Some(ms) = yielded_ms {
        use std::fmt::Write as _;
        let _ = write!(verdict, " yielded_ms={ms}");
    }
    // A guarded turn says how many Enters it wrote, so `submitted=0` after a
    // written-but-unverified press cannot read as "nothing was pressed".
    if submit_matcher.is_some() {
        use std::fmt::Write as _;
        let _ = write!(verdict, " pressed={guarded_presses}");
    }
    // A worker's turn names the task rev its Enter found: `await task
    // @<sid>:<rev>` then answers on this turn's own done, even one that
    // landed before the wait was armed. A session with no task, nothing.
    if let Some((rev, _)) = task_found {
        use std::fmt::Write as _;
        let _ = write!(verdict, " task_rev={rev}");
    }
    // A turn into an agent names the verdict rev its Enter found: `await agent
    // <words> since=<rev>` then answers on a verdict published after the
    // submit, never the one before it. A session no agent is in, nothing.
    if let Some(rev) = agent_found {
        use std::fmt::Write as _;
        let _ = write!(verdict, " agent_rev={rev}");
    }
    super::control_query::frame_rows_reply(screen.whole(), rows, &verdict, trim, 0)
}

/// The words in Claude Code's composer, every whitespace character dropped
/// (a draft that wraps is cut at the composer's width, and each of its rows
/// is read trimmed): the caret row between the composer's two rules down to
/// its bottom rule ([`aterm_phase::phase::composer_rules`],
/// [`aterm_phase::phase::composer_draft`]). Its dim placeholder is no words —
/// a suggestion (`Try "…"`), `Press up to edit queued messages` — which keeps
/// the cursor at column 2 of the caret row, where typing pushes it right
/// ([`aterm_phase::phase::is_placeholder`]): a key of a few letters is no
/// part of it once the Enter took the line. `None` where no framed composer
/// is on the screen — a box or a menu over it, or another program.
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

/// Whether the session is an identified Claude Code: the agent verdict's
/// reader ([`crate::session_timeline::AgentPublication::reader`]), which the
/// composer verify needs.
fn read_as_claude(ctx: &SessionCtx) -> bool {
    match ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .agent()
        .reader
    {
        Some(aterm_phase::Program::Claude) => true,
        // The verify reads Claude Code's framed composer alone; every other
        // agent's Enter stands on AUTO's `seq`, as before the verify.
        Some(
            aterm_phase::Program::Codex
            | aterm_phase::Program::Agy
            | aterm_phase::Program::Muse
            | aterm_phase::Program::Gemini
            | aterm_phase::Program::Generic,
        )
        | None => false,
    }
}

/// What the composer verify reads of an identified Claude Code's screen as
/// the Enter is about to be pressed ([`composer_verify_key`]).
#[derive(Clone, Debug, PartialEq, Eq)]
enum ComposerKey {
    /// Its framed composer shows the typed text: the KEY, that text with
    /// every whitespace character dropped, verifies the Enter.
    Shows(String),
    /// Its framed composer does not show the text yet — the echo late — and
    /// no `[Pasted text …]` chip stands for it: read again as the screen
    /// moves, within the echo's cap.
    NotYet,
    /// No composer verify — AUTO's `seq` stands: no framed composer on the
    /// screen, an empty text, or a paste Claude Code folded into a chip.
    No,
}

/// The composer verify's key in `t`, the screen of a session read as Claude
/// Code ([`read_as_claude`]), for the typed `text` ([`ComposerKey`]).
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

/// The composer verify's key, its echo waited for: `look()` reads the
/// screen of a session read as Claude Code once — what
/// [`composer_verify_key`] makes of it, and the content seq it stood at,
/// under one lock — and `wait(after)` parks until the screen moves past
/// `after` (`Ok(true)`) or the echo's cap ends (`Ok(false)`); `Err` is the
/// reply that ends the turn (the session exited, no watcher free). A
/// composer that does not show the text yet ([`ComposerKey::NotYet`]) is read
/// again each time the screen moves, and one that shows it gives the key;
/// nothing — AUTO's `seq` — where there is no composer to verify by, or the
/// cap ends first.
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

/// What a screen after the Enter says of the typed text ([`composer_after_press`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ComposerAfterPress {
    /// A framed composer that no longer holds it: Claude Code took the line
    /// (the composer emptied, its placeholder back, the line queued above it).
    Left,
    /// No framed composer on the screen: a box or a menu took its place (a
    /// slash command's), or a frame half drawn — judged at the window's
    /// close, by the last read.
    Gone,
    /// The composer still holds it: the Enter has not landed (a repaint
    /// elsewhere, Claude Code's animated logo), or it made a newline (an open
    /// bracket).
    Holds,
    /// A frame the program has not finished drawing (an open DEC 2026
    /// bracket, [`Terminal::sync_frame_unfinished`]): torn, so not judged.
    Unfinished,
}

/// The composer verify's read of `t` after the Enter, for the key
/// [`composer_verify_key`] took.
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

/// `history [<n>] [since=<id>]` -> the session's turn ledger, newest-last, framed
/// `OK <count>\n` + one line per record (streaming like `text`). `<n>` keeps the
/// last n (default: all retained); `since=<id>` keeps records with id strictly
/// greater than `<id>` (poll the ledger forward). Each record: `turn <id>
/// submitted=<0|1> status=<..> started_ms=<..> dur_ms=<..> seq=<..> hash=<hex16>
/// by=<harness|ctl|bridge|<sid>|-> arch=<origin>:<last> text=<pct-encoded>` — the
/// durable memory of what a driver typed and what settled, keyed by the same id
/// the `turn` reply and events digest use. `by=` names who typed it
/// ([`crate::turn_ledger::TurnBy`]: this instance's window harness, an
/// owner-token client, the fabric bridge, an edge-scoped caller's own sid, or
/// `-` when no one can be named); `status=pressed` is an Enter written with
/// nothing awaited after it: a text the window harness typed with `send` and
/// submitted with `key enter` ([`ledger_typed_input`]), or a `turn
/// settle=submitted`. `arch=` is the alt-screen archive's mark when
/// the turn started
/// ([`crate::turn_ledger::ArchMark`]): hand it to `offscreen since=` to read what a
/// fullscreen app scrolled away since. It sits BEFORE `text=` because `text=` is
/// the free-text tail every reader cuts a row at (aterm-link's hook splits on
/// `" text="`), so a field after it would be read as part of the message — as
/// does ` carried=1`, printed on a record a self-update handoff carried from the
/// previous process ([`crate::turn_ledger::TurnRecord::carried`]: its
/// `started_ms` and `seq` are that process's). A ledger an update could not
/// carry whole adds ` unheld_below=<id>` to the header (after the count)
/// when that floor is above the `since=` anchor, until `LEDGER_CAP` turns at
/// or past it have landed (the lost ones would have aged out by then):
/// records of turn ids below it may be gone rather than absent.
pub(crate) fn cmd_history(ctx: &SessionCtx, rest: &str) -> String {
    let page = match RingPage::parse(rest, "history") {
        Ok(page) => page,
        Err(usage) => return usage,
    };
    // A ledger a handoff could not carry whole says so:
    // records under ids below its floor may be gone, not absent, so an
    // empty or short reply is not "no turn landed". Only when the floor is
    // above the anchor — a `since=` past it lost nothing — and after the
    // count, where every reader of the header already allows fields. Only
    // until the turns it lost would have aged out of a ledger carried whole
    // (`TurnLedger::live_floor`): from then on it is stale noise a
    // supervisor's every `drive ledger` would repeat.
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

/// The `history` reply over the records [`cmd_history`] copied, with the
/// header's `unheld_below=` floor when it has one. Runs after the ledger lock
/// is released.
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

/// The `[<n>] [since=<id>]` window `history`, `timeline` and `exits` share.
/// `n` keeps the newest n rows (0: all); `since` keeps rows with id strictly
/// greater. Each verb takes its ring's lock by name, copies the page with
/// [`RingPage::copy`] or [`RingPage::copy_where`] (record text is an
/// `Arc<str>`, so a copy is a refcount bump), and formats after releasing it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RingPage {
    n: usize,
    since: Option<u64>,
}

impl RingPage {
    /// Tokens in any order, a repeated one's last value winning; anything
    /// else is `ERR usage: <verb> [<n>] [since=<id>]`.
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

    /// The newest `n` rows of a suffix the ring already seeked to `since`
    /// (all when `n` is 0), cloned in ring order. Its length is known, so
    /// the older rows are skipped without a visit.
    fn copy<'a, T: Clone + 'a>(&self, suffix: impl ExactSizeIterator<Item = &'a T>) -> Vec<T> {
        let skip = match self.n {
            0 => 0,
            n => suffix.len().saturating_sub(n),
        };
        suffix.skip(skip).cloned().collect()
    }

    /// The newest `n` rows satisfying `keep` (all when `n` is 0), cloned in
    /// ring order: walked from the newest end, so the walk stops at the n-th.
    fn copy_where<'a, T: Clone + 'a>(
        &self,
        rows: impl DoubleEndedIterator<Item = &'a T>,
        keep: impl Fn(&T) -> bool,
    ) -> Vec<T> {
        let take = match self.n {
            0 => usize::MAX,
            n => n,
        };
        let mut out: Vec<T> = rows.rev().filter(|r| keep(r)).take(take).cloned().collect();
        out.reverse();
        out
    }
}

/// `meta` — the session-metadata verb. Three forms:
///
/// * bare `meta` (Read): one status line joining the engine identity (live OSC
///   title, reported cwd, lifecycle state) with the user identity (`meta set`
///   fields) — `OK title=<pct> user_title=<pct|-> description=<pct|-> icon=<pct|->
///   role=<pct|-> attention=<pct|-> cwd=<pct|-> state=<s>`, then the additive
///   tail `attention_owner= attention_owners= attention_kinds= supervisor=
///   questions= agent_cwd= trusted_cwd= autopilot= land_at= autopilot_by=
///   autopilot_since=` (`land_at=`: the instant a timed Cruise lands, unix
///   seconds, `-` while none; `autopilot_by=`/`autopilot_since=`: who set the
///   pair and when, [`autopilot_by_fields`]). Every free-text value is pct-encoded so the reply is always
///   one line; `-` marks an unset optional. Readers find a field by key.
///   `autopilot=` is printed on every bare `meta`, set or `-`: the supervisor's
///   loop reads the session's word only off a reply that names the field
///   (`aterm_agent::supervise`'s `AutopilotWord`), so a host that kept the word
///   and printed it only when set would read as an older build's.
/// * `meta set <title|description|icon|role|attention|questions|autopilot> <text...>`
///   (write-escalated by the dispatch gate): stamp the operator's identity on
///   the session. Byte caps (after trim): title ≤ 120, description ≤ 1024,
///   icon ≤ 64, role ≤ 64, attention ≤ 256 — over-cap is a hard ERR, never a
///   silent truncation (the caller must know its label was refused). C0/C1,
///   line separators, bidi controls, and spoof-relevant invisible format
///   characters are rejected rather than silently stored. The user title
///   outranks the OSC title in tab labels; a role is a label the menus and
///   the pill name the session by first, and no value elects anything; an
///   `attention` entry that asks is the typed needs-human escalation the
///   menu-bar status item badges and lists, and a `kind=wait` entry a
///   record it does not badge (`crate::needs::of` reads which). `questions` is a
///   closed set, not text: `ask|recommended|long-term|short-term` (case
///   folded, stored lowercase) — how the in-window supervisor treats this
///   session's Claude Code question dialog, over `[harness] answer_questions`
///   and `question_priority` (`ask` hands it to a person, `recommended`
///   answers it with its recommended option, `long-term` / `short-term` by
///   the worker's own choice under that policy); any other value is `ERR
///   usage: meta set questions …`
///   ([`questions_usage`]). Writing it is Owner-only
///   ([`meta_questions_denied`], answered before this handler). `autopilot` is
///   the other closed set: `cruise|land|off` (the owner's words `forever`,
///   `infinity`, `wrap-up`, `wrapup` and `wrap` admitted, case folded, stored
///   as the mode's word) — how far the window's harness carries this session's
///   work, over `[harness] autopilot` (`cruise` keeps it moving, `land` wraps
///   it up and stops, `off` keeps the harness's hands off: the host stops the
///   session's worker); any other value is `ERR usage: meta set autopilot …`
///   ([`AUTOPILOT_USAGE`]). Writing it is Owner-only too
///   ([`meta_questions_denied`]), and a write that moves it rings the host
///   ([`crate::session_timeline::apply_meta_value`]).
/// * `meta unset <field>` — clear a field (labels fall back down the chain;
///   `meta unset questions` hands the session back to the global setting, and
///   `meta unset autopilot` to `[harness] autopilot`).
///
/// Returns `(reply, changed)`: `changed` is `true` only when a stored value
/// actually moved — for `attention`, any entry moving, one under a newer
/// entry included — and the dispatch arm keys its side-effects (tab-strip
/// repaint wake, the need's republish) on it, so a no-op re-set stays
/// silent. The `meta-change` timeline record happens here, and only when the
/// effective value moves ([`crate::session_timeline::apply_meta_value`]):
/// `timeline` and the `events` digest see exactly one event per change a
/// reader of the field can see, and none for an entry moving under a newer
/// one. The record wakes the session's watchers.
///
/// A test's line: [`cmd_meta_by`] as a `ctl` client's.
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

/// [`cmd_meta`], each write's session log mark by `by` (the wire's
/// [`log_actor`]).
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
            // The value is the raw remainder after the field token (single-space grammar like
            // `turn`): token iteration would collapse interior whitespace. The wire grammar
            // stops here; below is the shared policy ladder a GUI caller enters directly.
            let value = rest
                .split_once("set")
                .and_then(|(_, r)| r.trim_start().split_once(char::is_whitespace))
                .map_or("", |(_, v)| v);
            // Keyed attention: `meta set attention owner=<k> [kind=<ask|wait>]
            // <text>` writes only owner `<k>`'s entry, of that kind. The owner
            // leads, like every option on this wire, so a bare text that merely
            // contains `owner=` stays text; `kind=` comes only after it.
            // A timed Cruise (§9.3.5): `cruise until=<when>` writes the word
            // and its instant as one; any other autopilot value is a hand
            // write of the word, which drops a timer, the same word included.
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
                // A kind names a keyed entry's: the bare entry is a person's
                // ask, so `kind=` there is a usage error, never stored text.
                if value.trim_start().starts_with("kind=") {
                    return (attention_set_usage(), false);
                }
            }
            match write_session_meta(ctx, typed, MetaEdit::Set(value), by) {
                Ok(changed) => ("OK\n".to_string(), changed),
                // On the wire `meta set title ""` is a usage error, never a clear (that is
                // `meta unset`). A bare `meta set questions` names its own closed set.
                Err(MetaWriteError::NotAQuestionsPolicy) => {
                    (format!("ERR {}\n", questions_usage()), false)
                }
                Err(MetaWriteError::Empty) if typed == MetaField::Questions => {
                    (format!("ERR {}\n", questions_usage()), false)
                }
                // …and so does a bare `meta set autopilot`.
                Err(MetaWriteError::NotAnAutopilot) => (format!("ERR {AUTOPILOT_USAGE}\n"), false),
                Err(MetaWriteError::Empty) if typed == MetaField::Autopilot => {
                    (format!("ERR {AUTOPILOT_USAGE}\n"), false)
                }
                Err(MetaWriteError::Empty) => (meta_set_usage(), false),
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
                return (meta_unknown_field(), false);
            };
            // `meta unset attention owner=<k>` clears only owner `<k>`'s entry;
            // the bare form clears the bare owner's, and a keyed owner's
            // escalation outlives it.
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
            // A clear has no validation ladder to run, so it applies directly.
            let changed = apply_meta_value(ctx, field, None, by);
            ("OK\n".to_string(), changed)
        }
        Some(_) => (
            "ERR usage: meta [set <field> <text...> | unset <field>]\n".to_string(),
            false,
        ),
    }
}

/// `meta set autopilot <word> until=<when>` (§9.3.5), or `None` where the
/// value carries no `until=` (the plain word's write, [`cmd_meta`]'s own).
///
/// * `<when>` is `HH:MM` (local: the earliest instant strictly after now
///   whose local reading it is, today else tomorrow; a time a spring-forward
///   skips on the day it would fall is refused with the reason) or
///   `+<n>h`, `+<n>m`, `+<n>h<n>m` (1 minute to 24 hours of elapsed time):
///   `crate::autopilot_timers::resolve`.
/// * Only Cruise takes a time: `until=` with `land`, `off` or no word is
///   `ERR until= goes with cruise only: usage: …`. The owner's own words for
///   Cruise (`forever`, `infinity`) are Cruise. A malformed `<when>`, or two
///   `until=`, is the usage line ([`AUTOPILOT_USAGE`]).
/// * The word and the instant are stored as ONE write under the meta guard
///   (`session_timeline::apply_autopilot`), and the reply names the instant,
///   `OK land_at=<unix s> local=<date>T<HH:MM><offset>`, the `OK` token
///   leading as on every `meta set` (each in-tree reader tests that token
///   alone). `changed` is whether the stored pair moved; the dispatch arm's
///   `Wake::MetaChanged` indexes the timer in the App.
///
/// Owner-only by the field's gate ([`meta_questions_denied`], answered
/// before this): `until=` rides the `autopilot` field.
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
        // A wire write: the connection's actor, no window surface.
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

/// `ERR usage: meta set <field> <text...>`, naming the closed field list
/// ([`MetaField::NAMES`]).
fn meta_set_usage() -> String {
    format!("ERR usage: meta set <{}> <text...>\n", MetaField::NAMES)
}

/// `ERR unknown meta field (…)`, naming the closed field list
/// ([`MetaField::NAMES`]).
fn meta_unknown_field() -> String {
    format!("ERR unknown meta field ({})\n", MetaField::NAMES)
}

/// A LEADING `owner=<k>` token on an attention value, and the text after it.
/// `None` when the value does not lead with one (the bare form). `Err` carries
/// the refusal for a malformed key — named before anything is stored.
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

/// `ERR usage: meta set attention owner=<k> [kind=ask|wait] <text...>`.
fn attention_set_usage() -> String {
    "ERR usage: meta set attention owner=<k> [kind=ask|wait] <text...>\n".to_string()
}

/// A LEADING `kind=<ask|wait>` token on a keyed attention text (after its
/// `owner=`), and the text after it: `Ok((None, text))` when the text does
/// not lead with one, `Err` (the usage line) for a kind outside the closed
/// set.
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

/// The reply for a keyed attention write, worded like the bare field's.
fn attention_owned_reply(
    result: Result<bool, crate::session_timeline::AttentionWriteError>,
) -> (String, bool) {
    use crate::session_timeline::AttentionWriteError;
    match result {
        Ok(changed) => ("OK\n".to_string(), changed),
        // `NotAQuestionsPolicy` and `NotAnAutopilot` are the closed fields'
        // refusals and never come out of the attention ladder; each reads as
        // the usage line here rather than a panic, should that ever change.
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

/// `meta set supervisor <holder> [ttl=<ms>]` / `meta unset supervisor [holder=<holder>]`: the
/// owner-writable key that makes a running supervisor visible (`status`, `sessions` and `meta`
/// print `supervisor=<holder|->`). `None` when `rest` is not a supervisor write, so the caller
/// falls through to [`cmd_meta`].
///
/// Owner-class only: a supervisor answers the session's prompts, and an edge
/// token (even a write edge) must not be able to claim to be one or hide the
/// real one. Without `ttl=` the claim is bound to `conn`, the connection this
/// request arrived on, and is cleared when it stops serving; with `ttl=<ms>`
/// (1..=600000) it is a lease a one-shot client renews. A different holder's
/// live claim answers `ERR busy supervisor=<holder>`; the same holder renews.
/// A bare unset clears any holder's claim; `holder=<h>` clears it only while
/// `<h>` holds it (`OK` either way). Returns `(reply, changed)` like
/// [`cmd_meta`].
///
/// The proven self-claim ([`crate::self_claim`]): `prove` reads the proof of
/// a set's holder from the connection and the session (the kernel's peer pid,
/// the live foreground group, the process's image) — only for a set with no
/// `ttl=`, as a self-claim is connection-bound; a proven `codex-alab@<pid>` claim replaces the
/// window host's and a proven claim whose process is gone
/// ([`crate::session_timeline::claim_rule`]), and the host is told at once
/// ([`crate::harness_host::self_claimed`]). Every other replacement is
/// `ERR busy supervisor=<holder>`, as before.
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
    let ttl_ms = match toks.next() {
        None => None,
        Some(tok) => match tok.strip_prefix("ttl=").map(str::parse::<u64>) {
            Some(Ok(ms)) if (1..=crate::session_timeline::SUPERVISOR_TTL_MAX_MS).contains(&ms) => {
                Some(ms)
            }
            _ => return Some((USAGE.to_string(), false)),
        },
    };
    if toks.next().is_some() {
        return Some((USAGE.to_string(), false));
    }
    let (bound, expires_us) = match ttl_ms {
        Some(ms) => (None, Some(now.saturating_add(ms.saturating_mul(1000)))),
        None => (conn, None),
    };
    // A self-claim is connection-bound: a `ttl=` lease outlives the
    // connection (and an `execve` into another image), so it is never proven.
    let proven = if ttl_ms.is_some() {
        None
    } else {
        prove(holder)
    };
    let proof = proven.clone();
    match crate::session_timeline::claim_supervisor_as(
        ctx, holder, bound, expires_us, proven, now, by,
    ) {
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
            Some(("OK\n".to_string(), claimed.changed))
        }
        Err(other) => Some((
            format!("ERR busy supervisor={}\n", pct_encode(&other)),
            false,
        )),
    }
}

/// `meta set harness_act <act>` — an agent that supervises itself records an
/// act it took on the session (the native harness contract §5, law 5: who is
/// driving a session is seen per session). `None` for every other form, so
/// the caller falls through.
///
/// Only the holder of the session's live proven self-claim may: the request
/// arrives on the connection the claim is bound to, or the kernel proves it
/// again (`prove`: the peer pid of this direct connection is the claim's pid
/// and leads the foreground group). Anyone else is `ERR denied`, an unproven
/// claim's holder included. `<act>` is one owner token (1..64 bytes of
/// `[A-Za-z0-9._:@/-]`), else `ERR usage: meta set harness_act <act>`. The
/// act is one `harness-act` row on the session's timeline (`act=<act>
/// holder=<holder>`) and `status last_act=<act>`; it counts as a change, so
/// a proven claim's record copy is read again with it
/// ([`crate::self_claim::meta_changed`]). In the same timeline hold, the
/// session log's `act` mark by `by`, the connection's actor ([`log_actor`]),
/// for every act (`word=unknown` for one the log has no word for, since the
/// review of 6F-7): its wake is taken in that
/// hold, where it and the row ring the session's watchers. A landing's
/// `landing_mark` (or a point left to a person's,
/// `harness_task::holds_until_another_turn`), sampled before admission,
/// commits in that same hold:
/// another request cannot record a later landing and then have an earlier
/// request overwrite its mark after the commit.
pub(crate) fn cmd_meta_harness_act(
    ctx: &SessionCtx,
    landing_mark: Option<crate::session_timeline::LandingMark>,
    conn: Option<u64>,
    by: aterm_slog::Actor,
    prove: &dyn Fn(&str) -> Option<crate::self_claim::Proven>,
    rest: &str,
) -> Option<(String, bool)> {
    const USAGE: &str = "ERR usage: meta set harness_act <act>\n";
    let mut toks = rest.split_whitespace();
    let sub = toks.next()?;
    if !matches!(sub, "set" | "unset") || toks.next() != Some("harness_act") {
        return None;
    }
    if sub == "unset" {
        return Some((USAGE.to_string(), false));
    }
    let (Some(act), None) = (toks.next(), toks.next()) else {
        return Some((USAGE.to_string(), false));
    };
    if !crate::session_timeline::valid_owner_token(act) {
        return Some((USAGE.to_string(), false));
    }
    let holder = {
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
    // The proof is read outside the meta lock (it reads the kernel).
    let allowed = holder
        .filter(|(holder, proven, bound, _)| *bound || prove(holder).as_ref() == Some(proven));
    let Some((holder, proven, _, revision)) = allowed else {
        return Some(("ERR denied\n".to_string(), false));
    };
    // The kernel look runs off-lock. A release, renewal or replacement while
    // it ran invalidates admission, even if the next claim spells the same
    // holder. Hold meta through the timeline commit, in the usual lock order.
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    if meta.supervisor_revision != revision
        || meta
            .live_claim(crate::metrics::now_us())
            .and_then(|claim| claim.proven.as_ref())
            != Some(&proven)
    {
        return Some(("ERR denied\n".to_string(), false));
    }
    {
        let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_harness_act(act, &holder, by);
        if harness_act_face(rest).is_some_and(crate::harness_task::holds_until_another_turn)
            && let Some(mark) = landing_mark
        {
            timeline.mark_landing(mark);
        }
        if let Some(face) = crate::harness_task::self_claim_act_face(act) {
            timeline.queue_self_task(face);
        }
        // The `harness-act` row rang the session's watchers; the log's owed
        // wake is taken in the same hold.
        let _ = timeline.slog_take_wake();
    }
    Some(("OK\n".to_string(), true))
}

/// The face a recorded `meta set harness_act <act>` tells
/// ([`crate::harness_task::self_claim_act_face`]), for the meta arm's
/// change wake: `landed` for codex-alab's `landed@v1`, `theirs` for its
/// `left-to-person@v1` (a person's own exchange left to them), `open` for
/// any other act save its re-exec (`upgrade@v1`, no face); `None` for that
/// one and for every other `meta` line. Read only where the line was recorded (the arm's
/// `on_change`): [`cmd_meta_harness_act`] answered `OK`.
pub(crate) fn harness_act_face(rest: &str) -> Option<aterm_agent::harness::acts::TaskFace> {
    let mut toks = rest.split_whitespace();
    if toks.next() != Some("set") || toks.next() != Some("harness_act") {
        return None;
    }
    let act = toks.next()?;
    if toks.next().is_some() {
        return None;
    }
    crate::harness_task::self_claim_act_face(act)
}

/// `meta set questions|autopilot …` / `meta unset questions|autopilot` from a
/// connection that is not Owner-class: `ERR denied`. `None` for every other
/// form, so the caller falls through to [`cmd_meta`], which validates and
/// stores it.
///
/// The question policy decides whether the in-window supervisor answers
/// Claude Code's question box FOR the person — the same authority class as
/// the supervisor claim ([`cmd_meta_supervisor`]), and for the same reason:
/// an edge token — even a write edge granted on the session — must not be
/// able to switch the session's questions from `ask` to answered-without-you.
/// The Autopilot word is the same authority over the whole session: it decides
/// whether the harness answers and types at all (`off` to `cruise` puts the
/// harness's hands back on a session its owner took them off). Reading either
/// (`meta`) stays open to every scope that reads metadata.
pub(crate) fn meta_questions_denied(scope: Scope, rest: &str) -> Option<(String, bool)> {
    let mut toks = rest.split_whitespace();
    let sub = toks.next()?;
    if !matches!(sub, "set" | "unset") || !matches!(toks.next(), Some("questions" | "autopilot")) {
        return None;
    }
    (!scope.is_owner_class()).then(|| ("ERR denied\n".to_string(), false))
}

/// The bare-`meta` status line. Locks are sequential leaves (meta, then term, then the store
/// read), never nested, so this cannot deadlock against the render path (meta before term
/// there too) or the registry.
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
            // The `cwd` verb's native conversion: the two verbs share one boundary so they
            // agree.
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
    // The foreground program's own working directory, read from the OS (its timeline lock is a
    // leaf after the store's): an agent's folder-trust dialog asks for exactly this folder,
    // and a shell with no OSC 7 reports no `cwd=`.
    let agent_cwd = {
        let pgid = ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .agent()
            .program_pgid;
        u32::try_from(pgid)
            .ok()
            .filter(|p| *p > 0)
            .and_then(aterm_sysprobe::process_cwd)
            .map(|p| p.to_string_lossy().into_owned())
    };
    // A stored value of exactly "-" must not read back as the unset sentinel: pct_encode leaves
    // '-' verbatim, so it is escaped here ("%2D"), which any percent-decoder recovers.
    let opt = |v: Option<&str>| match v {
        None => "-".to_string(),
        Some("-") => "%2D".to_string(),
        Some(v) => pct_encode(v),
    };
    let (autopilot_by, autopilot_since) = autopilot_by_fields(&meta);
    // After `state=`, additive: which owner's entry `attention=` shows (the bare
    // owner prints `%2D` by the rule above), how many owners hold one, each
    // entry's kind (`kind=`), the live supervisor, the session's question policy
    // word, the foreground program's own working directory, the one folder a
    // worker's starter trusted for it (`spawn agent=`), the session's Autopilot
    // word (always named, `-` while unset: the supervisor's loop reads the word
    // only off a reply that names it), the instant a timed Cruise lands
    // (`land_at=`, unix seconds, `-` while unset), and who set the pair and when
    // (`autopilot_by=`, `autopilot_since=`; [`autopilot_by_fields`]). Beside
    // `supervisor=`, `supervisor_proven=<0|1>`: whether the live claim is a
    // proven self-claim ([`crate::self_claim`]).
    format!(
        "OK title={} user_title={} description={} icon={} role={} attention={} cwd={} state={state} \
         attention_owner={} attention_owners={} attention_kinds={} supervisor={} supervisor_proven={} \
         questions={} agent_cwd={} trusted_cwd={} autopilot={} land_at={} autopilot_by={} \
         autopilot_since={}\n",
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
        u8::from(meta.supervisor_proven(crate::metrics::now_us())),
        opt(meta.questions.as_deref()),
        opt(agent_cwd.as_deref()),
        opt(meta.trusted_cwd.as_deref()),
        opt(meta.autopilot.as_deref()),
        crate::autopilot::land_at_field(meta.land_at),
        autopilot_by,
        autopilot_since,
    )
}

/// `meta`'s `autopilot_by=` and `autopilot_since=` (design §11.7.2): who set
/// the Autopilot pair — `human:chip`, `human:border`, `human:menu`,
/// `human:settings`, `human:every`, `ctl`, `harness`, `bridge`, `aterm` (the
/// timer's fire), a session's sid (its edge connection), `-` for none
/// attributable ([`crate::session_timeline::AutopilotSetBy::word`]) — and
/// when, unix seconds. A word carried from before a restart by a leaf that
/// named no setter is `restored` and `-`; a word never set, `-` and `-`. A
/// setter stays beside an unset word (`meta unset autopilot` over the wire
/// reads `ctl`): it says who handed the session back to the default. Each
/// word is from a closed alphabet with no space or `=`, so none is encoded.
pub(crate) fn autopilot_by_fields(meta: &crate::session_timeline::SessionMeta) -> (String, String) {
    match (meta.autopilot_set_by, meta.autopilot.is_some()) {
        (Some(set_by), _) => (set_by.word(), set_by.since_ms.div_euclid(1000).to_string()),
        (None, true) => ("restored".to_string(), "-".to_string()),
        (None, false) => ("-".to_string(), "-".to_string()),
    }
}

/// `attention_kinds=`: each entry's kind by owner, in write order
/// (`AttentionOwners::kinds_field`); a meta seeded as a value and never keyed
/// holds the bare owner's implicit ask.
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

/// The owner of the entry `attention=` shows. A meta whose attention was
/// seeded as a value (a restore) and never keyed has one implicit entry, the
/// bare owner's.
fn attention_owner(meta: &crate::session_timeline::SessionMeta) -> Option<&str> {
    match meta.attention_owners.effective_owner() {
        Some(owner) => Some(owner),
        None => meta
            .attention
            .as_ref()
            .map(|_| crate::session_timeline::BARE_ATTENTION_OWNER),
    }
}

/// How many owners hold an attention entry (the implicit bare one counted).
fn attention_owners(meta: &crate::session_timeline::SessionMeta) -> usize {
    match meta.attention_owners.len() {
        0 => usize::from(meta.attention.is_some()),
        n => n,
    }
}

/// `timeline [<n>] [since=<id>]` -> the session's event timeline, oldest first, framed
/// `OK <count>\n` + one `event <id> t=<ms> kind=<k> <payload>` line per record, with
/// `history`'s window grammar over a drop-oldest ring. Payload values were pct-encoded at
/// record time, so every event is one line.
pub(crate) fn cmd_timeline(ctx: &SessionCtx, rest: &str) -> String {
    let page = match RingPage::parse(rest, "timeline") {
        Ok(page) => page,
        Err(usage) => return usage,
    };
    let events = {
        let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        page.copy(tl.since(page.since))
    };
    timeline_reply(&events)
}

/// The `timeline` reply over the events [`cmd_timeline`] copied. Runs after
/// the timeline lock is released.
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

/// `log [<node>] [lvl=] [since=<seq> ep=<epoch>] [n=] [kinds=] [class=turn]
/// [tier=content] [--json]`: the target session's session log
/// ([`crate::session_log`]), a pure reader of the ctx's timeline lock like
/// `timeline`, cross-session correct. `OK <n> v=2 as_of=<seq> ep=<e>
/// oldest=<seq>[ lost=<n>][ more=<k>]`, then a `gap why= lost= resync_to=`
/// line when the reader's cursor fell behind the ring or names another epoch,
/// then one record per line ([`crate::session_log::render_log_copy`]). The
/// default reply stops at the host tier; `tier=content` adds the content tier
/// (the OSC title), under the same `ReadScreen` authority `text` and
/// `timeline` already answer that content with. Only the records shown are
/// copied under the timeline's leaf lock; the reply is encoded after it is
/// released, so a long read never holds the lock the status sweep takes.
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

/// `log --json` (the dispatcher's `--json`/`json` route, the flag already
/// taken off `rest`): [`cmd_log`] with one JSON object per body line, the
/// cursor's gap included (`{"op":"gap","why":…,"lost":…,"resync_to":…}`).
pub(crate) fn cmd_log_json(ctx: &SessionCtx, rest: &str) -> String {
    cmd_log(ctx, &format!("{rest} --json"))
}

/// `log` of a session that has closed in this process ([`cmd_log`]'s reply,
/// `json` spelled either way, over the closed-session index's copy of its
/// log, `session_log::ClosedLog::source`): no lock is held.
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

/// `brief` of a session that has closed in this process ([`cmd_brief`]'s
/// reply over the closed-session index's copy of its log, as of the wall
/// clock now); `brief all` is the instance's, as from a live session.
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

/// [`cmd_brief_closed`] of a parsed `query` (the journal's reader places its
/// cursor first), its header handing out `hand` in place of the log's head
/// when given (a journal reply's, `aterm_slog::journal::View::reply_cursor`).
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

/// `brief [<node>] [budget=<tokens>] [since=<seq> ep=<epoch>]`: the target
/// session's token-budgeted rollup for an AI reader (design §7.4), from its
/// session log; `brief all [budget=]` is the instance's, every session's in
/// turn within one budget (Owner-only, escalated by `control::argument_escalation`,
/// like `sessions`). The store guard is dropped before any timeline is locked,
/// and each timeline's lock is held only to copy what its brief reads.
///
/// `jobs` is the stuck engine's line for this session (`jobs 2 jobs · 1
/// stuck · …`, [`crate::sys_host::brief_jobs_line`]), the last line before
/// the trailer's `more=`: the log's brief is fitted to the budget less that
/// line's tokens, so the whole stays within it, and a budget only the
/// one-line form fits goes without it.
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

/// `brief all [budget=]`: every live session's brief in turn within one
/// budget. The store guard is dropped before any timeline is locked, and each
/// timeline's lock is held only to copy what its brief reads.
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

/// `cast` -> `OK <nbytes>\n` then the session's full asciicast v2 recording as
/// the body (design A.5.1 / B.7). The body is the JSON header line followed by
/// one `[t, "o", …]`/`[t, "r", …]` event per recorded burst — exactly what
/// `asciinema play -`/`agg` consume. `<nbytes>` is the byte length of the body
/// that follows (UTF-8), matching the read-verb framing so the existing client
/// can read the body without guessing where it ends. Output-only and bounded
/// (drop-oldest) by the recorder; this verb only serializes the snapshot, never
/// the renderer, so it is cheap and lock-disjoint from the PTY write path. The
/// recorder lock is held only to lift the snapshot (Arc bumps, the `cast frames`
/// order); the codec's writer (`aterm_control::asciicast::write_into`) does the
/// O(bytes) escaping after it is released, so a large recording never stalls the
/// PTY reader, which records into it.
pub(crate) fn cmd_cast(cast: &Mutex<crate::cast::CastRecorder>) -> String {
    // The widest `OK <n>\n` a body length can need: the reply is written after
    // this slot and the real head is put over it once `n` is known, so the body
    // moves left by the slot's unused bytes inside one buffer and is never copied
    // into a second.
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

/// `cast frames [count=N]` -> expand the compressed `cast` recording into `count`
/// evenly-spaced keyframe SCREENS (default 8) — the recording read as a "video"
/// FLIPBOOK an AI can watch: raw `cast` is the compact, sendable asciicast; this
/// is its readable expansion. Framed like `text` (`OK <n>\n` + n lines): each
/// frame is a `--- frame <k>/<N> @ <ms>ms ---` header then that instant's screen
/// rows. Frame instants are rebased to the first retained event, so a truncated
/// recording spreads frames across the surviving span (no blank leading frames).
/// A recording with no output yields `OK 0`. The events are snapshotted OUT of the
/// recorder lock (chunk refcounts) and folded ONCE forward with the lock released,
/// so this stays cheap and never stalls the PTY reader, which records into it.
/// `policy` is the session's resize seam
/// ([`crate::app_render::pty_resize_policy`]), asked by the caller before the
/// lock: the fold replays every resize under it, as `cast drift`'s folds do.
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
    // Lift the events out of the lock (refcounts), THEN fold off-lock: the
    // O(events) VTE fold must never run under `ctx.cast`, which the PTY reader
    // takes per output burst (a long fold under it would stall the reader's
    // ingest).
    let snapshot = {
        let rec = ctx.cast.lock().unwrap_or_else(|p| p.into_inner());
        rec.snapshot()
    };
    let frames = snapshot.fold_frames(count, policy);
    let total = frames.len();
    let mut body = String::new();
    // DISCLOSE head-eviction, mirroring the `cast` header's `aterm_truncated`: if
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
    // …and for a recording that starts at a seamless update (an adopted
    // session): the screen it restored is not in the recording, so its frames
    // fold from a blank engine.
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

/// How long one cut of `cast drift` waits for the recording to hold the screen it
/// read ([`crate::cast::CastRecorder::snapshot_at`]). The PTY reader records a
/// burst right after the holds that drew it and a resize pass records its resize
/// right after its hold, so the wait is normally microseconds and none at all on a
/// quiet session; this caps one stalled in between (a cross-session resize
/// records after its rewrap).
const CAST_DRIFT_CATCH_UP: std::time::Duration = std::time::Duration::from_millis(60);

/// Consistent-cut attempts before `cast drift` proceeds with `cut=racy`.
const CAST_DRIFT_CUT_ATTEMPTS: usize = 3;

/// At most this many `cast drift` analyses fold recordings at once in this
/// instance. Each is up to `2 + MAX_RUNS` folds of a recording (a variant
/// folds its events after the anchor when the faithful replay forks there) on a
/// wait lane, and `cast drift` is Read-class, so any scoped peer can ask: the
/// cap keeps a burst of askers from taking the cores of the window a person
/// is using (measured before it: eight concurrent requests held the instance
/// at 700% CPU).
const CAST_DRIFT_CONCURRENCY: usize = 2;

/// How long a request waits for a fold slot before it is refused `ERR busy`.
const CAST_DRIFT_SLOT_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// The instance's `cast drift` fold slots ([`CAST_DRIFT_CONCURRENCY`]).
static CAST_DRIFT_SLOTS: DriftSlots = DriftSlots {
    busy: Mutex::new(0),
    freed: std::sync::Condvar::new(),
};

/// A counting semaphore for [`CAST_DRIFT_SLOTS`]; a slot is released when its
/// guard drops (a panicking analysis included).
struct DriftSlots {
    busy: Mutex<usize>,
    freed: std::sync::Condvar,
}

/// One held fold slot.
struct DriftSlot(&'static DriftSlots);

impl DriftSlots {
    /// Wait up to `wait` for a slot; `None` when every slot stayed taken.
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

/// `cast drift [max_runs=<k>] [rows=<n>] [seed=auto|alt|primary]` -> the
/// counterfactual replay audit of this session's asciicast against its live
/// screen (`aterm_control::cast_drift`, which owns the method and the wire):
/// `OK <n> verdict=<clean|desync|unfaithful|unknown|empty> fidelity=… culprit=…
/// … cut=<quiescent|racy> folds=<f> fold_events=<e> ms=<ms>` then `n`
/// `run`/`row`/`cursor` lines, parse by key. It names the resize run an app
/// never repainted after — the alt-screen net-zero flap that shifts every row
/// up one — and the rows that are wrong, with what the app believes they say.
///
/// The cut. Under the term lock read the rows, cursor and alt flag, and the
/// [`crate::cast::CastCut`] of that screen (the PTY reader's holds so far and the
/// engine's resize ordinal), release; then, under the recorder's lock, wait up to
/// [`CAST_DRIFT_CATCH_UP`] for the recording to hold every burst and resize that
/// cut names, and snapshot it. A recording that holds exactly those is the screen's
/// (`cut=quiescent`), with no wait when it already does; one that holds something
/// later, or did not catch up, is cut again, and after
/// [`CAST_DRIFT_CUT_ATTEMPTS`] the verb answers `cut=racy` with the replays held
/// against each other (`against=replay fidelity=-`: an app that never stops
/// drawing cannot make every answer `unknown`). The two locks are taken one
/// after the other, never nested (the `cast frames` order), and the folds run
/// with both released: a replay never holds the engine or the recorder. At
/// most [`CAST_DRIFT_CONCURRENCY`] analyses fold at once in the instance; a
/// request that waits [`CAST_DRIFT_SLOT_WAIT`] for a slot is answered `ERR
/// busy`. `policy` is the session's console-host posture: its resize seam
/// ([`crate::app_render::pty_resize_policy`]) and, under OpenConsole, the host's
/// alternate-screen resync (`crate::alt_screen_resync`), asked by the caller
/// before any lock. Served on a wait lane (`is_wait_request`), never the main
/// thread.
pub(crate) fn cmd_cast_drift(
    term: &Arc<Mutex<Terminal>>,
    frame: &crate::frame_memo::FrameSlot,
    cast: &Mutex<crate::cast::CastRecorder>,
    policy: impl Into<aterm_control::cast_drift::Posture>,
    rest: &str,
) -> String {
    cast_drift_with(term, frame, cast, policy, rest, CAST_DRIFT_CATCH_UP, &|| {})
}

/// [`cmd_cast_drift`] with the catch-up bound and a step between reading the
/// screen and snapshotting the recording injected, so a test can move the session
/// inside the cut.
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
        // The rows are the session's published text, looked at before the lock
        // and copied out after it.
        let mut look = frame.look_frame();
        let (text, cursor, alt, cut) = {
            let t = term_lock(term);
            let c = t.cursor();
            (
                look.text(&t),
                (c.row, c.col),
                t.is_alternate_screen(),
                crate::cast::CastCut::take(&t, &watermark),
            )
        };
        drop(look);
        let live = LiveScreen {
            rows: text.rows().map(str::to_string).collect(),
            cursor: Some(cursor),
            alt: Some(alt),
        };
        between();
        let (snapshot, quiescent) = crate::cast::CastRecorder::snapshot_at(cast, &cut, catch_up);
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

/// `temporal [tick] [trim]` -> `OK <nbytes>[ trimmed=<k>]\n` then the session's
/// screen RECONSTRUCTED at logical `tick` (default: the latest recorded instant) —
/// with `trim`, minus its trailing all-blank rows (the `text trim` rule; the byte
/// count on the header is the trimmed body's) — the read half of the
/// hydratable temporal spine (design Addendum B / B.9), the endpoint the
/// `TemporalRecorder` producer feeds. Replay seeds from the nearest retained
/// keyframe `<= tick` and folds the recorded `RawIn`/`Resize` events forward
/// through a fresh HEADLESS engine, then serializes its rows exactly as `text`
/// does (same [`super::visible_row`] path), so the body is faithful past-instant
/// screen text. Pure observer of the TARGET's own recorder (no renderer, no live
/// term lock), so it is correct cross-session like `cast`. `<tick>` is
/// MICROSECONDS since the session's recorder epoch; `temporal status` -> `OK
/// enabled=<bool> latest_tick=<n> reachable_from=<n|-> keyframes=<n> live_events=<n>
/// dropped_events=<n> lost_resizes=<n> dropped_input=<n> dropped_replies=<n>
/// pending_resizes=<n>[ carried=0 from_build=<n|->]` reports the reachable window and
/// whether recording is on, so a caller can pick a valid tick without guessing. Every
/// tick at or past `reachable_from=` replays, the spine's end included; `-` when the
/// end does not (the newest keyframe's chain was evicted, or a break is open), so a
/// dead window never reads as a live one. `dropped_events=` counts the
/// events the spine's ring evicted (no replay reaches before the oldest live one).
/// `lost_resizes=` counts resizes the live engine applied that the spine will never
/// hold (its resize journal overran before a reader read it — a replay across them
/// lacks their row moves). `dropped_input=` counts input slices the reader could not
/// queue: each opens a BREAK, which no replay crosses, until the reader queues a
/// keyframe of the live engine at its next hold that can (while one is open, a replay
/// past the spine's last event is unreachable). `dropped_replies=` counts reply
/// instants not queued, which no replay reads. `pending_resizes=` counts resizes it
/// applied that no reader has handed the spine YET (they wait for the reader's next
/// hold, the program's next output; [`crate::temporal::SpineWatermark::pending`]).
/// A replay at the spine's end (no tick, or one at or past `latest_tick`) lacks the
/// pending ones, so its header then says so too: `OK <nbytes>[ trimmed=<k>][
/// pending_resizes=<n>]`, the key only when `n > 0`. The `carried=0 from_build=`
/// tail is only for an adopted session, whose spine starts at the seamless update
/// that adopted it (`session_timeline::HandoffGap`). Two DISTINCT failures:
/// `ERR temporal: this session is not recording …\n` when recording
/// was never enabled for it (the default — a
/// config fix that reaches NEW tabs: a session keeps its spawn-time wiring),
/// versus `ERR temporal unreachable\n` when the base keyframe (or a needed input
/// blob) has aged out of the bounded retention window, or the tick lies past a
/// break (honest partial reach, never a wrong reconstruction). `<nbytes>` is the UTF-8 body length, matching the
/// read-verb framing so the existing client reads the body without guessing.
pub(crate) fn cmd_temporal(ctx: &SessionCtx, term: &Mutex<Terminal>, rest: &str) -> String {
    const USAGE: &str = "ERR usage: temporal [status | <tick>] [trim]  (tick = µs since \
                         session start; `temporal status` reports the reachable range)\n";
    // `trim` is the last token of either form; `temporal status trim` has no rows to trim, so
    // it is a usage error rather than a token that quietly vanishes.
    let (rest, trim) = super::control_query::split_trim_tail(rest);
    // The resizes the target's engine applied that no reader has handed its spine yet. The
    // watermark is read under the target's term lock (the reader publishes it there), after the
    // recorder lock is released, so the two never nest; and before any replay, so a resize
    // handed over meanwhile is counted, not missed.
    let pending = || {
        let spine = ctx
            .temporal
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .spine_watermark();
        spine.pending(&term_lock(term))
    };
    // `temporal status`: the reachable window and whether recording is on, so a caller can pick
    // a valid tick and tell an off recorder from an aged-out one.
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
             dropped_replies={} pending_resizes={}{}\n",
            rec.latest_tick().0,
            rec.keyframe_count(),
            rec.live_events(),
            rec.dropped_events(),
            rec.lost_resizes(),
            rec.dropped_input(),
            rec.dropped_replies(),
            if enabled { pending } else { 0 },
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
    // An enabled recorder always seeds a t0 keyframe at spawn, so `total_events() == 0` means
    // recording was never turned on (the default): a config fix, distinct from the retention
    // bound `ERR temporal unreachable` names.
    let pending = pending();
    // The replay is lifted out under the recorder lock (refcounts) and folded
    // after it is released, so the writer thread never waits on it.
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
    let term = plan.run();
    // A replay at the spine's end lacks the pending resizes: said on the header,
    // after the byte count the client reads (the key only when there are any, so
    // the common header is unchanged).
    let pending = if at_end && pending > 0 {
        format!(" pending_resizes={pending}")
    } else {
        String::new()
    };
    let rows = term.rows() as usize;
    // A private replay engine: the one construction, with no slot to share.
    let mut body = super::screen_text(&term);
    if trim {
        // Byte-framed: the header's count is the trimmed body's length, which the client's
        // `byte_count` (first token only) reads unchanged; `trimmed=<k>` follows as on the
        // row-framed verbs.
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

/// Why a wait could not arm: the session's watchers are all armed (the
/// observation kernel's per-session cap, 256). Every `await`/`turn`/`yield`
/// form says it in these words.
const WAITS_FULL: &str = "too many waits open on this session; retry when one ends";

/// `grant`'s answer to an op word [`Op::parse`] does not know: the words it does.
const GRANT_UNKNOWN_OP: &str =
    "ERR unknown op (read-screen|write-input|signal|config-write|clipboard-write|derive-loop)\n";

/// `grant <src-id> <op>` -> mint an edge (src -> this session, op) and return its
/// bearer token hex. Owner-only (also enforced by the gate's catch-all Deny).
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
    // Audit off the table lock: the logger sink has its own mutex and must never nest inside
    // `ctx.edges`.
    session_edge_audit::emit(EdgeAction::Grant, "wire", &src, &ctx.self_id, op.as_str());
    format!("OK {}\n", tok.to_hex())
}

/// The refusal of an edge source that is one of the session log's actor
/// words (`human ctl harness bridge aterm vendor -`):
/// `grant` and `connect` take a source as given, and no session is named
/// after an actor, so a grant under one could only be a try at being read
/// as that actor. The log never would (it attributes an edge only to a
/// minted sid, [`log_actor`]); the refusal says so where it was asked.
fn reserved_src(src: &str) -> Option<String> {
    aterm_slog::is_reserved_actor_word(src).then(|| {
        format!(
            "ERR reserved: src={src} is a session log actor (human ctl harness bridge aterm \
             vendor -), never a session\n"
        )
    })
}

/// `revoke <edge-hex>` -> remove one edge by its bearer token. `revoke
/// src=<sid>` -> sweep EVERY edge from that source (any op) and reply
/// `OK <removed>` — the wire dissolution primitive (design §1.4#4/§6): token
/// hexes are deliberately non-enumerable, so the sweep is the only way to
/// dissolve edges whose hex is lost. Owner-only; both forms fail closed with
/// `ERR no such edge` when nothing matches. Each successful act emits one
/// `session_edge` audit event (§1.4#5).
pub(crate) fn cmd_revoke(ctx: &SessionCtx, scope: Scope, rest: &str) -> String {
    if !scope.is_owner_class() {
        return "ERR denied\n".to_string();
    }
    revoke_in(&ctx.edges, &ctx.self_id, rest)
}

/// The `revoke` verb: [`cmd_revoke`] on the connection's own session, and a
/// `revoke <edge-hex>` it finds no row for goes on to the read grants the
/// closed-session index keeps (`session_log::ClosedLog`): a grant a session
/// closed with is revoked by its token as a live one is, from any owner
/// connection (the session it was on has no shell left to ask). A token is
/// one row in one table, so the first table holding it is the one. Each
/// table is taken out under the store's read lock and locked after it. A
/// closed session's grants by source go with `disconnect dst=<it> src=` and
/// with the source's own close.
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

/// `revoke`'s body against `edges`, the table of session `dst` (a live
/// session's, or the read grants a closed one keeps in the closed-session
/// index): `revoke <edge-hex>` removes that row, `revoke src=<sid>` every row
/// `<sid>` holds. Mutation is under the table lock, and the audit is emitted
/// after it: the logger must never nest inside an authority table lock.
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
        // ONE event for the whole sweep: `op=*` (every op the source held).
        session_edge_audit::emit(EdgeAction::RevokeSrc, "wire", &src, dst, "*");
        return format!("OK {removed}\n");
    }
    let Some(tok) = EdgeToken::from_hex(rest) else {
        return "ERR bad token\n".to_string();
    };
    // Read the row's (src, dst, op) identity BEFORE removal — the audit
    // event describes the edge, and after `revoke` the row is gone.
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

/// Parse the `dst=<sid> src=<sid> [kind=pull|push|both]` grammar `connect` and `disconnect`
/// share. `kind` is `None` when absent: `connect` defaults to `both`, `disconnect` to the whole
/// connection. An unknown token, empty value or missing endpoint is `Err`: an authority-minting
/// argument gets no guessed default.
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

/// `connect dst=<sid> src=<sid> [kind=pull|push|both]` against the process-wide
/// connection store. See [`cmd_connect_in`].
pub(crate) fn cmd_connect(store: &Store, scope: Scope, rest: &str) -> String {
    cmd_connect_in(&crate::connections::connections(), store, scope, rest)
}

/// The `connect` verb: set the connection `src → dst` to exactly `kind` (default `both`)
/// through [`crate::connections`], one atomic mint+revoke audited on `session_edge` with origin
/// `wire`. Owner-only.
///
/// `dst` must be a live registered session (its table and nonce are the mint target); `src`
/// may be any sid, as with `grant` (an unknown src never presents the token), but a self-loop
/// is refused. The reply names only the live ops, `OK read-screen=<hex> write-input=<hex>
/// signal=<hex>`; a no-op (the connection already is `kind`) replies the current set the same
/// way, from the record whose tokens stay live.
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
    // Resolve the destination, lifting its ctx out before any table lock. A dead session's
    // table would mint rows its nonce already fails closed: refuse instead.
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

/// The `disconnect` verb: dissolve the connection `src → dst`, only the `kind=` half when given.
/// Owner-only. Recorded connections dissolve by held token; wire-granted rows with no record
/// fall back to the op-filtered source sweep ([`crate::connections::disconnect_kind_in`]).
/// Reply `OK <revoked>`; an unknown pair fails closed with `ERR no such connection`. Unlike
/// `connect`, an exited but registered dst is a legal target: dissolution needs no liveness.
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
        // A self-loop is never a connection; refusing here also keeps the
        // fallback sweep from eating raw self-granted rows by accident.
        return "ERR self-loop\n".to_string();
    }
    // A live dst's table, or the read grants a closed one keeps in the
    // closed-session index (dissolution needs no liveness, §1.4#4): taken
    // out under the store's read lock, locked after it.
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

/// `flows [--json]` -> the instance's aggregated session-connection graph: every live edge row
/// across every registered session's table, `OK <n>\n` + one `<src> <dst> <op>` line per row,
/// sorted by `(src, dst, op)`. Owner-only: like `sessions`, it discloses the whole fabric.
/// `--json` groups per directed pair: `{"flows":[{"src":..,"dst":..,"ops":[..]}]}`.
/// Collection follows the [`crate::connections`] snapshot discipline (no store lock held
/// across a table lock).
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
        // Grouped per (src, dst): `all_edges` sorts, so pairs are consecutive. The op tokens
        // are the closed `Op::as_str` vocabulary (no escaping); sids go through
        // `json_str_field` like `edges --json`.
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

/// Resolve the `raise <sid>` argument against the registry: the headless-testable half of
/// [`cmd_raise`] (`pub(super)` for that test). Exactly one sid token; an unknown sid fails
/// closed.
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
/// the wire twin of the status item's Go / map Enter (design §6; no shipping
/// verb can do this: `tab` drives only the front window, `focus` is a PTY
/// focus event). Owner-only. Main-thread hop like `spawn` (window/tab state is
/// `App`-owned): the event loop runs the status item's Go switch+focus shape
/// (`App::focus_session_window`) for the resolved session and reports whether
/// any window hosts it.
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

/// `exits [<n>] [since=<id>]` -> the instance's exit ledger, oldest-first, framed
/// `OK <count>\n` + one `exit <id> t=<ms> sid=<sid> local=<n> reason=<…>
/// exit_code=<n|-> by=<sid|human|ctl|bridge|->` line per session that left the registry —
/// the `history`/`timeline` grammar (`<n>` keeps the last n; `since=<id>` keeps
/// rows with id strictly greater) over the roster journal, whose seq is the
/// row id. Owner-only, like `sessions`: it names every sid the instance ever
/// hosted. This is the tale the dead could not tell: a driver whose peer
/// answered `ERR no such session` asks here when it went (`t=`, the timeline's
/// clock), why (`reason=`), with what status (`exit_code=`, `-` when the child
/// was hung up by a close, died by signal, was not ours to reap, or had not yet
/// exited at either of the ledger's two non-blocking looks — see
/// `App::shell_exit_code`) and by whom (`by=`, the closing connection's sid,
/// `human`, `ctl` for an owner-token client, `bridge` for the fabric bridge,
/// or `-` — see [`caller_actor`]).
/// Bounded like the journal it reads (drop-oldest): a row older than the
/// retained window is gone, and `OK 0` means no exit is retained, not that
/// none happened.
pub(crate) fn cmd_exits(store: &Store, rest: &str) -> String {
    let page = match RingPage::parse(rest, "exits") {
        Ok(page) => page,
        Err(usage) => return usage,
    };
    // Copied under the read guard, which blocks only a register or a
    // deregister, and formatted after it is dropped.
    let rows = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        page.copy_where(g.roster_since(page.since.unwrap_or(0)), |r| {
            r.change == crate::session_store::RosterChange::Exited
        })
    };
    exits_reply(&rows)
}

/// The `exits` reply over the `Exited` rows [`cmd_exits`] copied, oldest
/// first. One line per row, fields at the END additive and pct-encoded where
/// free-form (`by=` may carry a sid).
fn exits_reply(rows: &[crate::session_store::RosterRecord]) -> String {
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
            r.sid,
            r.local_id,
            r.reason.as_str(),
            pct_encode(r.actor.as_wire()),
        );
    }
    out
}

/// The exit ledger's `by=` for a control-socket `close`: the caller's own sid when the
/// connection carries one. An edge's token was granted to a session
/// ([`aterm_session::EdgeTable::src_of`], read from the resolved target's table), and that
/// session is the caller. An owner-token connection carries no session, so it writes the kind
/// of client, [`ExitActor::Ctl`] (`by=ctl`); the fabric bridge writes [`ExitActor::Bridge`].
/// Never the front tab: `spawn` makes the new session the front tab, so an `@<new> close`
/// would name the closed session as its own closer.
pub(crate) fn caller_actor(scope: Scope, target: &SessionCtx) -> ExitActor {
    caller_actor_of(scope, Some(target))
}

/// [`caller_actor`] for a request that may have no resolved session (the App lane's `tab` and
/// `invoke`). The owner and the bridge, the only scopes that lane admits, are named by kind
/// either way; an edge caller is found only in a resolved session's table, so with none it
/// names nobody (`-`).
pub(crate) fn caller_actor_of(scope: Scope, target: Option<&SessionCtx>) -> ExitActor {
    match scope {
        Scope::Owner => ExitActor::Ctl,
        // The bridge is a connection, not a session: no sid to write, but named apart from
        // `ctl` because a fleet order and a local client are different callers to a ledger
        // reader. Unix-only like the scope (an inherited `socketpair`).
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
        // A fleet peer is another machine's reader, no session here and no
        // local caller; the fleet gate admits no verb that writes a ledger.
        #[cfg(any(unix, test))]
        Scope::Fleet { .. } => ExitActor::Unknown,
    }
}

/// The turn ledger's `by=` for a `turn` from this connection ([`TurnIo::by`]),
/// from the scope and the driver the dispatch already resolved from it (an
/// edge's source in the target's table, `crate::control::turn_driver`): the
/// caller the exits ledger would name ([`caller_actor`]), so the two ledgers
/// attribute a connection in the same words — `ctl` for an owner-token
/// client, `bridge` for the fabric bridge, an edge-scoped caller's own sid,
/// `-` for an edge whose grant is not in the target's table. Whether the
/// caller is the window harness is the turn's to say ([`typed_by`]).
pub(crate) fn turn_by(scope: Scope, driver: Option<&SessionId>) -> TurnBy {
    match scope {
        Scope::Owner => TurnBy::Ctl,
        #[cfg(any(unix, test))]
        Scope::Bridge => TurnBy::Bridge,
        Scope::Edge(_) => {
            driver.map_or(TurnBy::Unknown, |sid| TurnBy::Sid(sid.as_str().to_string()))
        }
        // A fleet peer names no caller here, and the fleet gate admits no
        // verb that types a turn ([`caller_actor`]'s arm says the same).
        #[cfg(any(unix, test))]
        Scope::Fleet { .. } => TurnBy::Unknown,
    }
}

/// Who typed a turn whose caller is `caller` ([`TurnIo::by`]) and that types
/// under `under`, the drive lease its own connection holds as it begins (the
/// one it hands back as it ends): an owner-token turn under THIS process's
/// window harness's hand ([`crate::harness_host::holder`], the lease the
/// harness's upgrade and relaunch lanes take a tab by for every line they
/// type — the live upgrade's notice and release, a relaunched agent's
/// continuation) is the harness's. This is
/// the rule by which the presence band reads aterm's own hand (an Owner-token
/// turn whose lease hands back to that hand), so the band and the ledger agree
/// on which turns are aterm's. Only an owner-token caller is
/// refined: an edge-scoped connection is its own sid whatever its lease is
/// named, and the bridge is the bridge — a holder name is never authority.
pub(crate) fn typed_by(caller: &TurnBy, under: Option<&crate::Lease>) -> TurnBy {
    let harness_hand = matches!(under, Some(crate::Lease::Drive { holder, .. })
        if *holder == crate::harness_host::holder());
    match caller {
        TurnBy::Ctl if harness_hand => TurnBy::Harness,
        other => other.clone(),
    }
}

/// Record a text the window harness types with `send` and submits with `key enter`
/// into the ledger ([`crate::turn_ledger::STATUS_PRESSED`]): wherever the
/// host fences `send`, the turn-end loop's continuation, done check and
/// answers go out so (`aterm-agent`'s `Session::type_fenced`), never as a
/// `turn`, and they are the harness's turns all the same. Called with every
/// input verb's reply (`verb`, its arguments past the leading options, the
/// reply), it keeps the books by two rules:
///
/// * a `send` this process's harness wrote ([`typed_by`]: an owner-token
///   caller whose connection holds the harness's hand) is kept as the text it
///   typed ([`crate::turn_ledger::TurnLedger::typed`]), and the `key enter`
///   that connection writes next records it: `by=harness`, `status=pressed`,
///   `submitted=1`, `hash=` the screen as the Enter left it, by the hash
///   `cmd_turn` takes;
/// * any other write that WROTE (an `OK` that is no skip) — another key, a
///   turn, a paste, anyone else's — ends the text unrecorded, as does the
///   hand's release ([`cmd_lease`]): only the Enter right behind the text
///   submits it. A write that wrote nothing changes nothing (the hand refuses
///   every other connection's).
///
/// Anyone else's `send` and `key` record nothing: a raw driver's line is no
/// turn here. `feed-bin` and `paste-bin` are served before this seam; the
/// harness's Enter is fenced on a read that shows its text in the composer,
/// so neither can pass for it.
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
    // The harness's own connection, while it holds the hand.
    let harness = super::serving_connection().filter(|&conn| {
        let lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
        let own = lease
            .as_ref()
            .filter(|l| l.held_over(crate::metrics::now_us(), Some(conn)));
        typed_by(&turn_by(scope, None), own) == TurnBy::Harness
    });
    // What a record reads off the terminal is read before the ledger's lock,
    // never under it.
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
            // The screen's hash off the session's published text, as a
            // `turn`'s record takes it, looked at before the lock.
            let mut look = ctx.frame.look_frame();
            let (seq, screen_hash) = {
                let t = term_lock(term);
                (t.content_seq(), look.text(&t).fnv())
            };
            drop(look);
            // The record wakes the session's watchers: the `events` digest
            // scans it now, as after a `turn`.
            let mut turns = turns();
            if let Some(typed) = turns.take_typed().filter(|typed| typed.conn == conn) {
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
                turns.record(record, mint_turn_id);
            }
        }
        _ => {
            turns().take_typed();
        }
    }
}

/// The session log's `by=` for a request on `scope` against `target` (design
/// §2.5): who the connection is, never a parameter.
///
/// - An Owner connection is `harness` when it presented the harness's
///   in-memory token ([`crate::control::Principal::Harness`]), else `ctl`
///   (every other in-session client: no finer label would be honest).
/// - The bridge's connection is `bridge`.
/// - An edge is the session its token was granted from, read from the
///   target's own table (the one that authorized the request) — and only
///   when that name is a sid this build mints
///   ([`aterm_session::is_minted_sid`]): `grant` takes its source as given,
///   so any other name (`harness`, `human`, …) is nobody, `-`. No path turns
///   a chosen word into a trusted actor.
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
        // A fleet peer is another machine's reader, no session here and no
        // local caller; the fleet gate admits no verb that opens a turn.
        #[cfg(any(unix, test))]
        Scope::Fleet { .. } => Actor::Nobody,
    }
}

/// `whoami` -> this session's fabric id and nonce and the connection's effective scope against
/// the session active now. An edge's op is re-derived from the presented token via `authorize`
/// (the gate's per-request authority), so it never over-reports power the token lost when `@.`
/// swung to another session (`edge unauthorized`).
pub(crate) fn cmd_whoami(ctx: &SessionCtx, scope: Scope) -> String {
    let s = match scope {
        Scope::Owner => "owner".to_string(),
        // The bridge learns it is the bridge, which a hand-started `aterm-link serve --sock`
        // (an Owner-token client) cannot see any other way; it checks this before claiming
        // delivery.
        #[cfg(any(unix, test))]
        Scope::Bridge => "bridge".to_string(),
        Scope::Edge(presented) => {
            let table = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
            match table.authorize(&presented, &ctx.self_id, &ctx.nonce) {
                Some(op) => format!("edge {}", op.as_str()),
                None => "edge unauthorized".to_string(),
            }
        }
        // Unreachable through the fleet gate (`whoami` is no fleet verb), and
        // named for what it is should that ever change.
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
    /// The count a handoff manifest carries leaves room for the turns the parent still serves
    /// before its `_exit`, so the successor never mints an id the parent already replied with;
    /// only a parent that outruns the reserve stands its Commit down.
    #[test]
    fn the_handoff_turn_count_leaves_room_for_turns_in_the_overlap() {
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

    /// A count held at the top of what a manifest carries (`raise_turn_ids` at `MAX_TURN_ID`)
    /// mints its next id past it, and every later ceiling is the top again
    /// (`turn_ids_for_handoff`'s clamp): that must not stand every Commit down. Pure
    /// arithmetic: the process-wide count is never moved.
    #[test]
    fn a_count_at_the_manifest_limit_never_wedges_the_commit() {
        let max = crate::handoff_carry::MAX_TURN_ID;
        // The state after `raise_turn_ids(MAX_TURN_ID)` and one `turn`.
        let minted = max + 1;
        let ceiling = minted.saturating_add(TURN_ID_RESERVE).min(max);
        assert_eq!(ceiling, max, "the ceiling is held to the top");
        assert!(!super::past_ceiling(minted, ceiling));
        assert!(!super::past_ceiling(minted + TURN_ID_RESERVE, ceiling));
        // Below the top the reserve still holds every Commit to it.
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
        use crate::session_timeline::TIMELINE_CAP;
        use crate::turn_ledger::{ArchMark, LEDGER_CAP, TurnRecord};

        let h = crate::session_store::test_handle(73);
        assert_eq!(cmd_history(&h.ctx, "1 since=0"), "OK 0\n");
        assert_eq!(cmd_timeline(&h.ctx, "1 since=0"), "OK 0\n");

        // Overflow both rings, and give turns gaps in their process-global ids.
        // Queries below the retained floor must still return the retained tail.
        for i in 1..=(LEDGER_CAP.max(TIMELINE_CAP) + 7) as u64 {
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
            h.ctx
                .timeline
                .lock()
                .unwrap()
                .record("title-change", format!("title=tab%20{i}"));
        }

        type HistoryQuery = fn(&SessionCtx, &str) -> String;
        // The timeline reads an anchor above every id it handed out as another
        // count's (a keeper's recovery restarts it): every retained row.
        let verbs: [(HistoryQuery, usize, bool); 2] = [
            (cmd_history, LEDGER_CAP, false),
            (cmd_timeline, TIMELINE_CAP, true),
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
                    // Independent wire oracle: filter the complete reply, then
                    // select its tail. No ring iterator/skip is used here.
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

    /// The ring verbs format off their locks: `history`, `timeline` and
    /// `exits` copy their page under the ring's lock (the turn ledger's, the
    /// timeline's, the store's read guard) and format after releasing it, so
    /// a probe at the start of each formatter
    /// ([`crate::session_log::ENCODE_PROBE`]) finds all three free.
    /// Negative control: each formatter run inside its ring's held guard
    /// finds exactly that lock held, so the probe sees every lock it checks,
    /// and the reply is the same.
    #[test]
    fn ring_reads_format_after_their_lock_is_released() {
        use crate::session_log::ENCODE_PROBE;
        use crate::session_store::{ExitActor, ExitReason, RosterChange, new_store, test_handle};
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
            .record("title-change", "title=t".to_string());

        // [turn ledger, timeline, store] held, per formatter run.
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

        // Negative control: formatting under each guard is seen.
        let all = RingPage { n: 0, since: None };
        let under = [
            {
                let ledger = h.ctx.turns.lock().unwrap();
                history_reply(&all.copy(ledger.since(None)), None)
            },
            {
                let tl = h.ctx.timeline.lock().unwrap();
                timeline_reply(&all.copy(tl.since(None)))
            },
            {
                let g = store.read().unwrap();
                exits_reply(
                    &all.copy_where(g.roster_since(0), |r| r.change == RosterChange::Exited),
                )
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

    /// Tier-1: every production read decision has exactly the model's guard.
    /// The retired decision re-armed after an expired, unmet reading.
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

    /// Genuine momentum, terminal watchers and subscriber parking. The read cap
    /// makes a reintroduced immediate-latch loop fail instead of hanging.
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

        // A late GUI reply can carry a crossing already behind the backstop.
        // Checking only the park is insufficient: latch precedes its timeout.
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

    /// A registered handle over a bare engine (sentinel `master = -1`), so these lines are the
    /// real store projection, not a fixture string.
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
            ctx,
            identity: None,
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

    /// The published program / agent columns, `human_ms=`, the typed
    /// `role=`/`attention=`/`user_title=`, `supervisor=`, `autopilot=`, the
    /// owner's columns, the harness's `land_at=`/`task=`/`pause=`, and the
    /// task record's `task_state=`/`task_rev=`, elided by [`tail`] and pinned
    /// on their own by [`the_roster_carries_the_published_program_and_agent_verdict`].
    const AGENT_COLUMNS: [&str; 21] = [
        "program=",
        "agent=",
        "agent_detail=",
        "agent_rev=",
        "agent_since_ms=",
        "agent_gen=",
        "agent_fp=",
        "human_ms=",
        "role=",
        "attention=",
        "user_title=",
        "supervisor=",
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

    /// The tail every line carries, without the sid and `nonce=` (minted per run; the nonce is
    /// pinned by [`the_roster_carries_each_sessions_public_launch_nonce`]) and without the
    /// columns in [`AGENT_COLUMNS`].
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

    /// `await agent` takes the verdict words and, for lane A's walls, `wall`
    /// (any wall) and `wall:<kind>`; a typo or a malformed kind is refused
    /// rather than parked until the timeout.
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
        // Against the words the server really publishes.
        let overloaded = aterm_phase::WallKind::Overloaded.wire();
        let session = aterm_phase::WallKind::UsageSession.wire();
        assert!(words[1].matches(overloaded));
        assert!(words[2].matches(session));
        assert!(!words[2].matches(overloaded));
        // Negative control: `wall` is not a prefix match on everything.
        assert!(!words[1].matches("prompt") && !words[1].matches("wallpaper"));
        // `limited` is refused like any unknown word: `wall` or `wall:<kind>` names it.
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

    /// The typed `role=`, `attention=` and `user_title=` a fleet glance
    /// classifies by ride every row, so the menu-bar fleet scan reads a sibling
    /// in one hop.
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
                " role=reviewer attention=stuck%20on%20CI user_title=my%20build supervisor="
            ),
            "{line}"
        );
        assert!(line.contains(" meta=1 "), "{line}");
    }

    /// One `sessions` read tells a manager which of its workers has news:
    /// `task_state=<state> task_rev=<n>` closes every row, `-`/`0` for a
    /// session with no task, after `task=`, the task's face under Land (`-`
    /// with no live claim).
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

    /// Three registered workers, each marked `working` (rev 1).
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

    /// The header and the `state=` of each row of an `await task` reply.
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

    /// `await task` is EDGE-TRIGGERED on each target's rev and combines with
    /// `any`/`all`: news past a target's since answers (`any`) at once; the
    /// same news does not answer a wait re-armed with the rev it was read at,
    /// nor one armed without a since (the rev at arm); `since=0` takes news
    /// from before the wait; `all` waits for the last worker; and with no
    /// news the deadline answers every row under `status=timeout`. The short
    /// deadlines are the expected timeouts, never a latency budget.
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
                "@{} rev=1 state=working agent=- attention=- result_bytes=0\n",
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
            any.contains(" rev=2 state=done agent=- attention=- result_bytes=64\n"),
            "{any}"
        );
        // Negative controls: `all` still waits on two; re-armed with the rev
        // each was read at, and armed with none, the same news is no news.
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
        // `since=0`: news from before the wait counts.
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

    /// A parked `await task` is woken by the mark itself (its `task-change`
    /// row rings the session's watchers), and the supervisor's attention on a
    /// working worker is news;
    /// a session that closed while waited on answers `state=closed`. The
    /// 60 s deadlines are hang detectors: each wait must end on its event.
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
        // The harness escalates on a working worker: its rev moves and the
        // raised flag is news (the row's attention= is the session's own).
        write_attention_owned(
            &workers[1].ctx,
            crate::harness_host::ATTENTION_OWNER,
            MetaEdit::Set("a login"),
            aterm_slog::Actor::Harness,
        )
        .unwrap();
        let reply = one(&workers[1], 1);
        assert!(
            reply.ends_with(" rev=2 state=working agent=- attention=a%20login result_bytes=0\n"),
            "{reply}"
        );
        // A worker whose session ended answers `closed`, whatever its record.
        store
            .write()
            .unwrap()
            .set_state(workers[2].local_id, SessionState::Exited);
        assert_eq!(task_reply(&one(&workers[2], 1)), ("OK 1", vec!["closed"]));
    }

    /// `await task`'s single form takes the session's own record through
    /// `cmd_await`, and refuses a target list after a selector.
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
        // `task` is the first word: a client reads the reply's framing off
        // the second word of the request (`framing_of`), so any other order
        // would answer rows to a reader expecting one line.
        assert_eq!(await_("timeout=20 task"), AWAIT_TASK_USAGE);
        assert_eq!(await_("timeout 20 task since=1"), AWAIT_TASK_USAGE);
    }

    /// Latched within a wait: a target that showed news keeps it, and the
    /// row it showed, after its record moves on to no news — so news that
    /// came and went between two wakes still counts toward `all` — while a
    /// target without news is read again on every wake. A wait armed again
    /// reads every record as it stands.
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
            })
            .collect();
        let edges = [1; 3];
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
        // Moved back to work (rev 3, no news) while another finishes.
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
        // Negative control: a wait armed again reads the record as it stands.
        let mut again: Vec<Option<String>> = vec![None; 3];
        latch_task_news(&store, &targets, &edges, &exits, &mut again);
        assert!(again[0].is_none() && again[1].is_some(), "{again:?}");
    }

    /// `path=` is measured where it can be: a carried adoption mark reads `path=frozen
    /// path_evidence=carried` until a child of the shell is read; a live reading then wins and
    /// says so, `copy=` names the agent's build, and `upgrade=` the tab's live upgrade. Negative
    /// control: a reading for a group no longer in front changes nothing.
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
        // A live reading lowers the mark once, for the sweep to apply; a
        // frozen one never raises it (one job's `PATH=` override reads so).
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

    /// The five published columns ride every row LAST, after `path=`, read
    /// from the session timeline the status sweep writes (`-` until it has).
    /// `autopilot=` beside `supervisor=` is `-` until the session runs an agent
    /// the harness supervises, then its effective Autopilot.
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
            line.contains(" supervisor=- autopilot=- "),
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
        // `human_ms=`, the typed metadata, `supervisor=` and `autopilot=` —
        // `off` for a supervisable agent with no host to supervise it — then
        // the owner's three columns, the harness's `land_at=`/`task=`/
        // `pause=` (no timer, no face, no pause told) and the task record's
        // two close the row: no person has typed…
        assert!(
            line.ends_with(
                " human_ms=- role=- attention=- user_title=- supervisor=- autopilot=off \
                 path_evidence=- copy=- upgrade=- land_at=- task=- pause=- task_state=- task_rev=0"
            ),
            "{line}"
        );
        // …and once one has, the row says how long ago.
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
                " supervisor=- autopilot=off path_evidence=- copy=- upgrade=- land_at=- task=- \
                 pause=- task_state=- task_rev=0"
            ),
            "{line}"
        );
        // Under a host that supervises, the agent tab's EFFECTIVE Autopilot:
        // the table's default, then the session's own word.
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

    /// With rows present, each line names its window, active-tab and front bits; a session the
    /// rows do not mention is detached (`window=none`), not unknown.
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
            "0 - alive tab-0 meta=0 window=0 active=1 wfocus=0 detail=- identity=- path=live"
        );
        assert_eq!(
            tail(lines[2]),
            "1 - alive tab-1 meta=0 window=3 active=0 wfocus=1 detail=- identity=- path=live"
        );
        assert_eq!(
            tail(lines[3]),
            "2 - alive tab-2 meta=0 window=none active=0 wfocus=0 detail=- identity=- path=live"
        );
    }

    /// The title is the strip's, read live ([`roster_title`]): rung by rung, and the
    /// contended-lock fallback.
    #[test]
    fn sessions_lines_print_the_title_the_strip_settles_on() {
        // No title, no cwd: the store's title.
        let bare = Arc::new(Mutex::new(Terminal::new(24, 80)));
        // ConPTY's program path with the cwd known: the cwd rung, never the
        // path. A POSIX cwd so the native form is the same on every host.
        let fresh = Arc::new(Mutex::new(Terminal::new(24, 80)));
        {
            let mut t = fresh.lock().unwrap();
            t.set_title("C:\\Program Files\\PowerShell\\7\\pwsh.exe");
            t.process(b"\x1b]7;file://localhost/srv/demo-cwd\x07");
        }
        // The prompt's own title: printed as it is, identity and all.
        let settled = Arc::new(Mutex::new(Terminal::new(24, 80)));
        settled
            .lock()
            .unwrap()
            .set_title("user@m17-tower: ~\\aterm");
        // `cmd.exe`'s `<path> - <command>`: the command.
        let running = Arc::new(Mutex::new(Terminal::new(24, 80)));
        running
            .lock()
            .unwrap()
            .set_title("C:\\Windows\\system32\\cmd.exe - ping build-01");
        // The operator's title outranks everything the engine says.
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

        // A contended engine lock falls back to the store's title, with a program path dropped
        // there too and "aterm" standing in for an empty one, so the token never vanishes.
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

    /// `family` prints `sessions`' title column ([`roster_title`]): a session
    /// whose OSC title is still ConPTY's program path reads under its cwd in
    /// both roster verbs, and under the path in neither.
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

    /// A failed hop costs the window columns (`-`), never a line: the count and
    /// every store column survive, and `detail=` — the engine's, not the window
    /// layer's — is still answered.
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
            "0 - alive tab-0 meta=0 window=- active=- wfocus=- detail=- identity=- path=live"
        );
        assert_eq!(
            tail(lines[2]),
            "1 - alive tab-1 meta=0 window=- active=- wfocus=- detail=sleep identity=- path=live"
        );
    }

    /// The roster carries each session's launch nonce, the presence row's `epoch=` for the
    /// fabric bridge: `whoami` reports only the connection's own session, so a bridge cannot
    /// read any session's nonce there. The nonce is public anti-spoof state
    /// (`aterm_session::LaunchNonce`), republished in every presence row, and this listing is
    /// Owner-only.
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
        // Thirty-two hex digits, right after `meta=` so `aterm-ctl`'s `roster_tail` (which
        // reads back to that anchor) keeps working.
        assert_eq!(expect.len(), 32, "{expect}");
        let fields: Vec<&str> = row.split_whitespace().collect();
        let meta_at = fields.iter().position(|f| f.starts_with("meta=")).unwrap();
        let nonce_at = fields.iter().position(|f| f.starts_with("nonce=")).unwrap();
        assert_eq!(nonce_at, meta_at + 1, "{row}");
    }

    /// The bridge projection must agree with the public roster on all three
    /// identity fields, while staying answerable behind locks the public
    /// roster reads. No event-loop proxy enters `cmd_sessions_bridge`, so it
    /// cannot ask the main thread for placement either.
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
        // The three locks stay held until this test returns, so an answer shows the roster
        // neither takes nor waits on them (`bridge_roster` reads the store alone); the minute is
        // a hang detector.
        let bridge = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("identity-only roster must not wait on terminal, meta or timeline");
        let mut lines = bridge.lines();
        assert_eq!(lines.next(), Some("OK 2"));
        assert_eq!(lines.map(str::to_string).collect::<Vec<_>>(), expected);
        assert!(!bridge.contains("window=") && !bridge.contains("detail="));
        worker.join().unwrap();
    }

    /// `detail=` is the sanitized executing command (no arguments), `-` when
    /// the block is not executing — and `-` when the engine lock is contended,
    /// because the roster must never wait on a PTY reader for a column.
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
            out.contains(" window=0 active=1 wfocus=1 detail=targo%20test identity=- path=live "),
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
            out.contains(" detail=- identity=- path=live "),
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
            out.contains(" detail=- identity=- path=live "),
            "complete reads `-`: {out}"
        );
    }

    /// `identity=` names the agent identity each session was spawned under, `-` for the human's
    /// own. It sits right after `detail=`, so aterm-ctl's `roster_tail` (which reads the tail by
    /// key back to `meta=`) is unmoved; `path=` and the agent columns follow it.
    #[test]
    fn sessions_lines_carry_the_identity_after_detail_as_a_name_or_a_dash() {
        let t0 = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let t1 = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let mut worker = handle(0, &t0);
        worker.identity = Some(Arc::from("worker"));
        let snapshot = vec![worker, handle(1, &t1)];
        let rows = vec![row(0, 0, true, true), row(1, 0, false, true)];
        let out = sessions_lines(
            &snapshot,
            Ok(&rows),
            crate::autopilot::HarnessPosture::default(),
            |_| false,
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "OK 2");
        assert_eq!(
            tail(lines[1]),
            "0 - alive tab-0 meta=0 window=0 active=1 wfocus=1 detail=- identity=worker path=live"
        );
        assert_eq!(
            tail(lines[2]),
            "1 - alive tab-1 meta=0 window=0 active=0 wfocus=1 detail=- identity=- path=live"
        );
        for line in &lines[1..] {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let detail_at = fields
                .iter()
                .position(|f| f.starts_with("detail="))
                .expect("detail= column");
            assert!(
                fields[detail_at + 1].starts_with("identity="),
                "identity= sits right after detail=: {line}"
            );
            assert!(
                fields[detail_at + 2].starts_with("path="),
                "path= sits right after identity=: {line}"
            );
            assert_eq!(
                fields.len(),
                detail_at + 3 + AGENT_COLUMNS.len(),
                "only the published agent columns follow path=: {line}"
            );
            for (i, key) in AGENT_COLUMNS.iter().enumerate() {
                assert!(fields[detail_at + 3 + i].starts_with(key), "{line}");
            }
        }
        // The failed-hop arm carries it too: the label is a handle field, not
        // a main-thread answer.
        let out = sessions_lines(
            &snapshot,
            Err("no event loop"),
            crate::autopilot::HarnessPosture::default(),
            |_| false,
        );
        assert!(
            out.lines()
                .nth(1)
                .unwrap()
                .contains(" detail=- identity=worker path=live program=-"),
            "{out}"
        );
    }

    /// `path=` is the registry's frozen-path mark: `frozen` for the shell the store marked at
    /// adoption, `live` for every other, on both arms (a registry fact, not a main-thread
    /// answer). Also through the verb over a real store, so the roster names the store's mark.
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
            "0 - alive tab-0 meta=0 window=0 active=1 wfocus=1 detail=- identity=- path=frozen"
        );
        assert_eq!(
            tail(lines[2]),
            "1 - alive tab-1 meta=0 window=0 active=0 wfocus=1 detail=- identity=- path=live"
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
                .contains(" detail=- identity=- path=frozen program=-"),
            "the failed-hop arm carries the mark too: {out}"
        );
        // Through the verb: the store's own mark, read under the snapshot's guard.
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
            tail(lines[1]).ends_with(" detail=- identity=- path=frozen"),
            "the marked session reads frozen: {out}"
        );
        assert!(
            tail(lines[2]).ends_with(" detail=- identity=- path=live"),
            "an unmarked session reads live: {out}"
        );
    }

    /// A minimal `SessionCtx` for the `temporal` verb: only the recorder is
    /// exercised, the rest is the same neutral shape [`handle`] builds.
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
            update_parked: Default::default(),
            watchers: observed.watchers,
            frame: Default::default(),
            witness: Default::default(),
            fenced_writes: Default::default(),
        }
    }

    /// `temporal trim` drops the trailing blank rows, and its header's byte count is the trimmed
    /// body's own length (the client reads the body by that count); `temporal status trim` is a
    /// usage error.
    #[test]
    fn temporal_trim_byte_frames_the_trimmed_body_and_status_trim_is_usage() {
        let ctx = temporal_ctx();
        let rows: usize = 8;
        let live = Mutex::new(aterm_core::terminal::Terminal::new(rows as u16, 20));
        // A ground blank grid is the replay base; the two lines below fill only
        // the first two of eight rows, so six trailing rows stay blank. (The
        // engine and the recorder are locked one after the other, never nested.)
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

        // Bare: the whole grid, one line per row (row-count framed).
        let bare = cmd_temporal(&ctx, &live, "");
        assert!(bare.starts_with("OK "), "bare temporal: {bare:?}");

        // Trim: `OK <nbytes> trimmed=<k>` then the trimmed body.
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

    /// `temporal status` names where the replayable range starts: the tick of
    /// the oldest keyframe whose chain is whole, and `-` when nothing replays,
    /// so a recording whose window is gone never reads like a live one (both
    /// say `enabled=true`).
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

        // Events and no keyframe (a seed no checkpoint could carry): no window.
        let dead = temporal_ctx();
        dead.temporal.lock().unwrap().record_raw_in(b"a");
        let status = cmd_temporal(&dead, &live, "status");
        assert!(status.starts_with("OK enabled=true "), "{status}");
        assert_eq!(reachable_from(&status).as_deref(), Some("-"));
        assert_eq!(cmd_temporal(&dead, &live, ""), "ERR temporal unreachable\n");
    }

    /// `temporal` lifts its replay out under the recorder lock and folds it
    /// after releasing it: a probe from inside the fold takes the lock.
    /// Negative control: the same probe while the lock is held finds it taken.
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

    /// `temporal status` counts what the reader could not queue: input
    /// (`dropped_input=`, each a break) and reply instants (`dropped_replies=`).
    /// While the break is open the spine's end does not replay, so
    /// `reachable_from=-` and a bare `temporal` is unreachable. The keyframe
    /// the next hold queues closes it, and the end replays the live screen.
    #[test]
    fn temporal_status_discloses_dropped_input_and_an_open_break() {
        use crate::temporal::{SpineMark, SpinePre, TemporalTx};
        fn hold(live: &Mutex<Terminal>, mark: &mut SpineMark, tx: &TemporalTx, bytes: &[u8]) {
            let mut t = term_lock(live);
            let pre = SpinePre::take(&t);
            t.process(bytes);
            mark.record(tx, pre, &t, bytes.into());
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

    /// A flap that lands after the program's last output is PENDING: no reader
    /// hold has handed it to the spine yet, so a replay at the spine's end is
    /// the screen before it. `temporal status` and the replay's header say how
    /// many (`pending_resizes=2`) instead of passing that screen off as the
    /// live one; the program's next output hands both halves over, and the
    /// replay then IS the live screen, with nothing pending. The flap is a
    /// width flap (20 -> 19 -> 20 columns cuts the last one), which the
    /// engine's resize undo does not serve, so the two screens differ.
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
        // One reader hold, then the writer thread's folds.
        let mut slice = |bytes: &[u8]| {
            {
                let mut t = term_lock(&live);
                let pre = SpinePre::take(&t);
                t.process(bytes);
                mark.record(&tx, pre, &t, bytes.into());
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
            cmd_temporal(&ctx, &live, "status").ends_with(" pending_resizes=0\n"),
            "nothing pending before the flap"
        );

        {
            let mut t = term_lock(&live);
            t.resize(ROWS, COLS - 1);
            t.resize(ROWS, COLS);
        }
        let status = cmd_temporal(&ctx, &live, "status");
        assert!(status.ends_with(" pending_resizes=2\n"), "{status}");
        let (head, body) = replayed(&cmd_temporal(&ctx, &live, ""));
        assert!(head.ends_with(" pending_resizes=2"), "{head}");
        assert_ne!(
            body,
            screen(),
            "the spine lacks the flap until the next output"
        );

        slice(b"\x1b[1;1HT1\x1b[K\x1b[4;3H");
        let status = cmd_temporal(&ctx, &live, "status");
        assert!(status.ends_with(" pending_resizes=0\n"), "{status}");
        let (head, body) = replayed(&cmd_temporal(&ctx, &live, ""));
        assert!(!head.contains("pending_resizes"), "{head}");
        assert_eq!(body, screen(), "both halves handed over: the live screen");
    }

    /// A live engine and its asciicast recorder, driven by the SAME operations
    /// the PTY reader and the resize pass perform on a real session, each hold
    /// first and its record after (`crate::cast::rig`): an alt-screen frame with
    /// a non-blank footer, an 8 -> 7 -> 8 flap split by output that draws nothing
    /// (a mode set: it drops the engine's resize undo, so the grow appends and
    /// the rows stay shifted), then a diff frame the app sent without a repaint.
    /// `quiet_flapped_session` is the same flap with nothing between its halves,
    /// which the engine undoes.
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

    /// The verb's wire: `OK <n>` counts exactly the lines after it, the cut is
    /// quiescent when nothing moves, and the flap is named with its rows.
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
        // The grammar's refusal.
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

    /// The same flap with nothing between its halves: the engine's resize undo hands the
    /// demoted row back, and the verb answers clean with no culprit.
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

    /// A session that moves between reading the screen and snapshotting the
    /// recording (the reader draws and records a burst there, every cut) is a
    /// RACY cut: the recording holds more than the screen read. After the
    /// bounded retries the verb still answers, with the replays held against
    /// each other (`against=replay fidelity=-`) — the screen it could not read
    /// still is compared with nothing, and a busy app gets an answer.
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

    /// The verb on a session whose recording holds its screen: the cut is
    /// quiescent, and the recording is never waited on. Negative control: with
    /// a burst on screen that the reader has not recorded yet (and no time
    /// given to catch up), every cut waits, none holds the screen, and the
    /// answer is racy.
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

    /// A cut taken while the screen shows a resize and a burst the recording
    /// does not hold yet waits for them: another thread records both once the
    /// cut is waiting, and the answer is quiescent and names the flap. A
    /// recording that never catches up within the bound is racy (the negative
    /// control, for the resize and the burst alone).
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

    /// The instance folds at most [`CAST_DRIFT_CONCURRENCY`] recordings at
    /// once; a request that cannot get a slot in time is refused by name, and
    /// a released slot is taken again.
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

    /// `exits` on an instance that has lost nothing is `OK 0` — including one
    /// whose journal holds only births (a `Created` row is not an exit).
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

    /// Every reason in `ExitReason::ALL` renders on its own line in the documented shape, with
    /// each actor kind and both `exit_code` cases.
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

    /// `exits <n>` formats only the rows it replies with: the page is cut
    /// from the journal's newest end under the read guard, so an older exit
    /// is neither cloned nor formatted. Over interleaved births and exits,
    /// for every `<n>` and `since=`, the reply equals the full-format-then-drain
    /// oracle, and the formatter's row count ([`crate::work_counts`]) equals
    /// the rows shown. Negative control: with no `<n>` every exit is formatted,
    /// so the count can see rows the page leaves out.
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
                .roster_since(since)
                .filter(|r| r.change == RosterChange::Exited)
                .map(|r| {
                    let exit_code = r
                        .exit_code
                        .map_or_else(|| "-".to_string(), |c| c.to_string());
                    format!(
                        "exit {} t={} sid={} local={} reason={} exit_code={exit_code} by={}",
                        r.seq,
                        r.t_ms,
                        r.sid,
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

    /// The `history`/`timeline` grammar: `<n>` keeps the newest n, `since=<id>`
    /// keeps rows with id strictly greater (the seq the row itself prints, so a
    /// reader pages with the last id it saw), and anything else is `ERR usage`
    /// — never a silently ignored argument.
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
        // Newest one.
        let last = cmd_exits(&store, "1");
        assert!(last.starts_with("OK 1\n"), "{last:?}");
        assert!(last.contains(&format!("exit {} ", ids[2])), "{last:?}");
        // Strictly newer than the second.
        let newer = cmd_exits(&store, &format!("since={}", ids[1]));
        assert!(newer.starts_with("OK 1\n"), "{newer:?}");
        assert!(newer.contains(&format!("exit {} ", ids[2])), "{newer:?}");
        // Caught up.
        assert_eq!(cmd_exits(&store, &format!("since={}", ids[2])), "OK 0\n");
        // Both together.
        let both = cmd_exits(&store, &format!("5 since={}", ids[0]));
        assert!(both.starts_with("OK 2\n"), "{both:?}");
        // A repeated token's last value wins.
        assert_eq!(cmd_exits(&store, "3 1"), last);
        assert_eq!(
            cmd_exits(&store, &format!("since=0 since={}", ids[1])),
            newer
        );
        // Usage.
        for bad in ["bogus", "since=x", "since=", "1 extra", "-1"] {
            assert_eq!(
                cmd_exits(&store, bad),
                "ERR usage: exits [<n>] [since=<id>]\n",
                "{bad:?}"
            );
        }
    }

    /// `by=` names the caller, never the closed session: an edge's caller is the session its
    /// token was granted to (read from the target's table), an owner-token connection is
    /// `ctl`, the bridge is `bridge`, and a token the target's table does not know is `-`.
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
        // With no resolved session (the flagless `tab` on the App lane) the owner and the
        // bridge are still named by kind and an edge names nobody; with one, the answer is
        // `caller_actor`'s.
        assert_eq!(caller_actor_of(Scope::Owner, None), ExitActor::Ctl);
        assert_eq!(caller_actor_of(Scope::Bridge, None), ExitActor::Bridge);
        assert_eq!(caller_actor_of(Scope::Edge(tok), None), ExitActor::Unknown);
        assert_eq!(
            caller_actor_of(Scope::Edge(tok), Some(&target.ctx)),
            ExitActor::Sid(caller_sid.clone())
        );
        // The row an edge-attributed close writes carries the caller's sid, not its own.
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

    /// The turn ledger's caller is the exits ledger's ([`turn_by`], from the
    /// driver the dispatch resolved, `turn_driver`), and the one thing that
    /// refines it is the lease the turn types under ([`typed_by`]): an
    /// owner-token turn under this process's harness hand is the harness's.
    /// Negative controls: another instance's harness (a different pid in the
    /// holder), a manager's lease, a `turn`'s lease, no lease, and every
    /// non-owner caller are left as they are — an edge under the harness's
    /// holder name is still its own sid, because a lease's name is never
    /// authority.
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
            // The exits ledger names the same caller.
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

/// The session log's `by=` (design §2.5): who the
/// connection is, never a name anyone chose.
#[cfg(test)]
mod log_actor_tests {
    use super::{cmd_grant, log_actor};
    use crate::control::{Principal, Scope, ServingPrincipal};
    use crate::session_store::test_handle;
    use aterm_session::{EdgeToken, Op, SessionId};
    use aterm_slog::Actor;

    /// Owner is `ctl`, the harness's token `harness`, the bridge `bridge`, an
    /// edge its minted source sid; an edge whose source is no minted sid —
    /// `harness` among them, granted around `grant`'s refusal — and a token
    /// the target never issued are nobody (`-`).
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

    /// R1: `grant` (and `connect`) refuse a source that is one of the log's
    /// actor words, naming it; a sid is granted as before.
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

/// The counters a driver pages or waits by keep their meaning across a seamless update: the
/// exit ledger's and the timeline's ids continue above the old process's (carried on the
/// manifest), and a content seq from before it is recognised rather than parked on.
#[cfg(test)]
mod update_anchor_tests {
    use super::{cmd_await, cmd_exits, cmd_timeline};
    use crate::session_store::{
        ExitActor, ExitReason, SessionHandoff, SessionStore, new_store, test_handle,
    };

    /// `exits since=<id>` from before an update still means "after that row" on the successor.
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

        // The successor's store, as `main_entry` makes it: the carried count
        // first, then the adopted sessions.
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
        // An older producer's manifest carries none: nothing is raised.
        let old = SessionHandoff::from_toml(
            "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-old\"\nstate = \"alive\"\ntitle = \"zsh\"\n",
        )
        .expect("an old manifest reads");
        assert_eq!(old.roster_seq, None);
    }

    /// An adopted session's timeline continues above the id the record carried, so a pre-update
    /// `timeline since=<id>` sees the `handoff` row and what follows.
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
        // `register` recorded `spawned` as well: 58 rows, and the overlap's
        // reserve above them.
        let reserve = crate::session_timeline::HANDOFF_ID_RESERVE;
        assert_eq!(read.sessions[0].timeline_id, 58 + reserve);

        // The old process goes on recording after the draw and a driver pages past those events
        // (`since=60`): the successor's `handoff` row must still lie above that anchor.
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
        // A process that outruns the reserve says so: the Commit stands down.
        {
            let mut tl = producer_timeline.lock().unwrap();
            for _ in 0..reserve {
                tl.record_raw_meta_change(String::new());
            }
            assert!(tl.past_handoff_ceiling());
        }
    }

    /// A `seq` anchor above the current count (read off the instance an update replaced)
    /// answers at once with the current seq, never `OK timeout`; an anchor at the current
    /// count still waits for a change.
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
}

/// The raw `grant` verb mints a row for every call, an identical `(src, dst, op)` included, and
/// takes any string as its source; the handoff carries each triple once. A triple the successor
/// cannot re-mint (a source that is not one of its sessions, a self-loop) still rides once: its
/// drop in `App::remint_carried_connections` is the `session_edge` audit of its dissolution.
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
            // A controller that grants on every attach.
            assert!(
                cmd_grant(&target.ctx, Scope::Owner, &format!("{src} read-screen"))
                    .starts_with("OK ")
            );
            // A source that is no session of this store.
            assert!(
                cmd_grant(&target.ctx, Scope::Owner, "s-controller read-screen").starts_with("OK ")
            );
        }
        for op in ["signal", "write-input"] {
            assert!(
                cmd_grant(&target.ctx, Scope::Owner, &format!("{src} {op}")).starts_with("OK ")
            );
        }
        // A self-loop, which nothing mints but `grant` does not refuse.
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
            // Each once, for the successor to drop and audit.
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

/// While a session's PTY reader is parked for an update (`SessionCtx::update_parked`), nothing
/// consumes its output, so the silence is not the program's: an idle wait does not latch on
/// it, and a `turn`'s submit window neither closes nor presses Enter again.
#[cfg(test)]
mod update_park_wait_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::{OnHangup, Park, Parked, Stretch, TurnIo, cmd_await, cmd_ready, cmd_turn};
    use crate::session_store::{new_store, test_handle};

    /// Both waits time out with the reader parked; unparked mid-wait, the window runs from the
    /// resume.
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
        // Unparked mid-wait, the idle window runs in full from the resume. Mid-wait is observed,
        // not slept: nothing is printed, so only a parked look moves the await's idle deadline,
        // and a deadline a whole window past the first shows the await held its window open
        // across the park. The resume and the deadline it is judged by are read under one hold
        // of the terminal lock, only while that deadline is still ahead, so `held` is what the
        // last parked look set and lies after the resume.
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
                            // Disarmed: the await answered while parked.
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
        // Negative control: unparked, the same wait latches at its 60 ms window; the timeout
        // is the hang detector.
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

    /// `ready`'s shell-integration answer is the parser's, and a parked reader
    /// leaves the parser behind the program: a shell at its prompt that was
    /// handed `make` while parked has signed the command's start into the
    /// kernel queue, unread. So no look answers `prompt` while the park stands,
    /// and the park stands until the resumed reader has parsed what queued
    /// (`spawn::UpdatePark`; the real reader is driven through it in
    /// `spawn::update_park_catch_up_tests`). Then the parser is the program's
    /// again: here `make` runs, so the answer is the idle settle after it,
    /// never the prompt it left.
    #[test]
    fn ready_answers_no_prompt_from_a_parked_readers_parser() {
        let handle = test_handle(0);
        let store = new_store();
        // A fresh shell-integration prompt: the newest block is PromptOnly.
        handle.term.lock().unwrap().process(b"\x1b]133;A\x07$ ");
        handle.ctx.update_parked.park_for_test();
        assert_eq!(
            cmd_ready(&handle.term, &store, 0, &handle.ctx, "400"),
            "OK timeout\n",
            "a parked reader's parser says nothing about the shell"
        );
        // The reader resumes mid-wait, as a real one does: it parses what
        // queued at the prompt (the shell started `make`), and only then ends
        // the park.
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
        // Negative control: never parked, a prompt answers at once.
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

    /// A `turn` whose Enter lands while the reader is parked presses it once —
    /// the absent echo says nothing then — however many presses it was allowed.
    /// Negative control: unparked, a swallowed press is pressed again.
    ///
    /// The deadline is the subject only parked, where the first press's
    /// window stays open to it. Unparked, the turn ends at its third
    /// unanswered window, so its timeout is a hang detector: a bound that
    /// three 150 ms windows had to fit cut a correct run to two presses
    /// whenever the test thread stalled.
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

    /// The onset of a park is an event: `park_reader` rings the session's
    /// watchers, and between the slices of a served lane the park reads the
    /// flag itself, so even a flip nothing rang is seen within one slice.
    /// Negative control: with no flip, the same park over 100 ms looks twice,
    /// at entry and at its close, however many slices it took.
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
                // No ring: only the park's own read between slices sees it.
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

/// [`Park`]: a slice of a served lane that ends with no wake asks only after
/// the caller, and never re-runs the verb's look.
#[cfg(test)]
mod park_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::{OnHangup, Park, Parked, Stretch};
    use crate::park::Lane;
    use crate::session_watch::{Subscription, Watchers};

    static SLICES: AtomicUsize = AtomicUsize::new(0);

    /// A served lane's 2 ms slices, counted.
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

    /// The loop every park ran before [`Park`]: the whole look after every
    /// wait, a bare slice's included. Returns how many looks it took.
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

    /// A park over 100 ms of 2 ms slices, with nothing ringing and the caller
    /// there, looks exactly twice: at entry and at its close. The count does
    /// not depend on how many slices the window took. Negative control: the
    /// loop before it looked once per slice, plus once at entry.
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

    /// What one turn did: its reply, the keys and pastes it wrote, and its
    /// Enters.
    struct Ran {
        reply: String,
        writes: usize,
        presses: usize,
    }

    /// `line` as one turn, the person's Take Over landing once `take_after`
    /// keys and pastes have been written (`Some(0)`: before the turn), and
    /// `remote` the node that holds the tab and drives it, if any.
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

    /// A hold set while a turn runs stops it there: the person's Take Over
    /// landing between a typed turn's keys writes no key after it and no Enter,
    /// and says what landed; one landing after a plain turn's paste presses no
    /// Enter; one standing before the first byte answers the dispatch's own
    /// `ERR halted`, typing nothing. Negative controls: with no Take Over the
    /// same turns write every key and press their Enter, and a remote node's
    /// own hold never stops its own drive's turn.
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

    /// A Claude Code screen at 24x80: its composer's caret row between two
    /// rules holding `caret_row` (the `❯ ` and what follows it), the cursor
    /// left at `col` (1-based) of that row.
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

    /// The key is the typed text the composer shows, whitespace aside. Not
    /// shown yet is `NotYet` — read again as the screen moves — save where a
    /// `[Pasted text …]` chip stands for it, or no framed composer is on the
    /// screen, or the text is empty: `No`, and auto's `seq` stands. Claude
    /// Code's dim placeholder (the cursor at column 2 of the caret row) is no
    /// text: a key it spells is not shown. Negative control: the same words
    /// with the cursor after them are the typed text.
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
        // The placeholder spells the key; the cursor says it is no text.
        let placeholder = screen("\u{276f} Press up to edit queued messages", 3);
        assert_eq!(
            composer_verify_key(&placeholder, "edit"),
            ComposerKey::NotYet
        );
        // Negative control: the cursor after the words.
        let drafted = screen("\u{276f} Press up to edit queued messages", 35);
        assert_eq!(
            composer_verify_key(&drafted, "edit"),
            ComposerKey::Shows("edit".to_string())
        );
    }

    /// After the Enter the composer has left the key once it no longer holds
    /// it — emptied, or Claude Code's placeholder back, though its words
    /// spell the key (`Press up to edit queued messages` after `edit`) —
    /// holds it while the typed text stands, is gone where a box or a menu
    /// took its place, and is not judged in a frame still being drawn.
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

    /// A late echo is waited for: the composer read `NotYet` is read again
    /// each time the screen moves past the seq it was read at, and its key
    /// taken once shown. The echo's cap ending first, or no composer to
    /// verify by, gives none (auto's `seq`); a reply that ends the turn (the
    /// session exited) is handed back. Negative control: a composer showing
    /// the text at once waits for nothing.
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
        // Negative control: shown at once.
        assert_eq!(
            script(vec![shows()], vec![]),
            (Ok(Some("edit".to_string())), vec![])
        );
    }
}
