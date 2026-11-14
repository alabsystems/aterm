// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Session-bound, credential-free account observations. These observations grant
//! no assignment, renewal or restart authority. Schema 1 says only what the pool holds. Schema
//! 2 also says what `restart account=` may ask of it for the asking session: the row aterm
//! judges that session to run on, the pool's own choice, which rows a move would not be refused
//! on record, and whether the restart would be carried now. It is a judgement, not a proof: the
//! relaunch still proves the row it starts on. Schema 3 is schema 2 with each row's
//! `signed_out_reason`, the settled refusal on record of a signed-out Codex login
//! ([`aterm_agent::harness::codex_app_login::row_signed_out_reason`]): an observation, which
//! grants nothing either. A client asks the schema it reads, so neither earlier schema changes.

use super::*;
use aterm_agent::harness::codex_app_login::{self, RefusalReason};
use aterm_agent::harness::restart_stamp::Wanted;
use aterm_agent::harness::{accounts, accounts_pool, cli::Env, self_claimed};
use aterm_json::{Value, json};
use std::sync::OnceLock;
use std::time::Instant;

const MAX_ROWS: usize = 128;
const MAX_FRAME: usize = 64 * 1024;

/// `snapshot schema=<1|2|3> request=<32 lowercase hex>`: the schema asked for and the request's
/// id. A client asks the schema it can read; no other is answered.
fn request(line: &str) -> Option<(u8, &str)> {
    let mut words = line.split_ascii_whitespace();
    if words.next()? != "snapshot" {
        return None;
    }
    let schema = match words.next()? {
        "schema=1" => 1,
        "schema=2" => 2,
        "schema=3" => 3,
        _ => return None,
    };
    let id = words.next()?.strip_prefix("request=")?;
    (words.next().is_none()
        && id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some((schema, id))
}

pub(super) fn account_id(label: &str) -> String {
    aterm_agent::harness::codex_enrollment::account_id(label)
}

fn masked_email(email: Option<&str>) -> Option<String> {
    let (name, domain) = email?.split_once('@')?;
    let first = name.chars().next()?;
    (!email?.chars().any(char::is_control) && domain.chars().count() <= 128)
        .then(|| format!("{first}***@{domain}"))
}

pub(super) fn claim(ctx: &crate::SessionCtx, master: i32) -> Option<crate::self_claim::Proven> {
    if crate::control::serving_principal() == crate::control::Principal::Harness {
        return None;
    }
    let pid = crate::control::serving_peer_pid()?;
    crate::control::serving_connection()?;
    crate::control::prove_self_claim_for(ctx, &format!("codex-alab@{pid}"), master)
}

/// Why a row of a given condition is signed out, as schema 3 says it ([`rows`]): a refusal's
/// closed word ([`RefusalReason::code`]), or `None`.
type SignedOutReason<'a> = &'a dyn Fn(&accounts::Row, accounts::Condition) -> Option<RefusalReason>;

/// The snapshot's rows; each carries `signed_out_reason` only where `reason` is given (schema
/// 3), so the rows of schemas 1 and 2 are what they were.
fn rows(
    records: &accounts::AccountRecords,
    home: &std::path::Path,
    now: i64,
    eligible: &dyn Fn(&str) -> bool,
    reason: Option<SignedOutReason<'_>>,
) -> Option<Vec<Value>> {
    if records.roster.rows.len() > MAX_ROWS {
        return None;
    }
    let enrolled = accounts::enrolled_in(accounts::Cli::Codex, &records.roster, home);
    records
        .roster
        .rows
        .iter()
        .map(|row| {
            if row.label.chars().count() > 128 || row.label.chars().any(char::is_control) {
                return None;
            }
            let present = enrolled.contains(&row.label);
            let condition = accounts::condition(row, &records.state, present, now);
            let sample = records
                .state
                .accounts
                .get(&row.label)
                .and_then(|a| a.sample.as_ref());
            let five = sample.and_then(|s| s.session(now));
            let week = sample.and_then(|s| s.week(now));
            let mut value = json!({
                "id": account_id(&row.label), "label": row.label,
                "masked_email": masked_email(row.email.as_deref()),
                "condition": condition.as_str(), "enrolled": present,
                "held": row.held_by.is_some(),
                // Cached quota and file presence cannot establish fresh provider
                // authentication or permission to launch under this account: schema 1 says
                // no row is eligible, and schemas 2 and 3 only that a move to it would not be
                // refused on record. The relaunch still proves the row.
                "eligible": eligible(&row.label),
                "quota": {"observed_at": sample.map(|s| s.at),
                    "stale": sample.is_none_or(|s| now.saturating_sub(s.at) > 300),
                    "five_hour_used_percent": five.map(|v| v.0),
                    "five_hour_reset_at": five.map(|v| v.1),
                    "weekly_used_percent": week.map(|v| v.0),
                    "weekly_reset_at": week.map(|v| v.1)}
            });
            if let Some(reason) = reason {
                value.as_object_mut()?.insert(
                    "signed_out_reason".into(),
                    Value::from(reason(row, condition).map(RefusalReason::code)),
                );
            }
            Some(value)
        })
        .collect()
}

/// A snapshot, and what its pool says of moving the asking session ([`serve`] joins the
/// session's own side at reply time).
#[derive(Clone)]
struct Snapshot {
    value: Value,
    /// Whether the pool moves this session at all ([`accounts_pool::Moves::offered`]).
    select: bool,
    /// Whether a move to the pool's own choice would not be refused on record.
    auto: bool,
}

fn snapshot(sid: &str, proof: &crate::self_claim::Proven, schema: u8) -> Option<Snapshot> {
    let env = Env::from_process().ok()?;
    let launch = judged_launch(sid, proof, schema);
    snapshot_in(sid, proof, &env, schema, launch.as_ref())
}

/// The launch a snapshot of `schema` judges for the proven process `proof` of tab `sid`: for
/// schemas 2 and 3, its validated line ([`launch_of`]) with the credential variables that
/// process was started with ([`crate::harness_host::RunningLaunch::of_live`]). Schema 1 judges
/// no launch: it says what it said before schema 2 existed.
fn judged_launch(
    sid: &str,
    proof: &crate::self_claim::Proven,
    schema: u8,
) -> Option<crate::harness_host::RunningLaunch> {
    (schema >= 2)
        .then(|| launch_of(sid, proof))
        .flatten()
        .and_then(|resume| crate::harness_host::RunningLaunch::of_live(resume, proof.pid))
}

/// The launch of the proven process `proof` of tab `sid` as aterm validated it: the tab's
/// self-claim copy, only where it is of that very process. The login it carried is what aterm
/// read from that process as it validated its self-claim, never anything the session says of
/// itself.
fn launch_of(sid: &str, proof: &crate::self_claim::Proven) -> Option<self_claimed::Resume> {
    crate::self_claim::copy_of_tab(sid)
        .filter(|copy| copy.pid == proof.pid && copy.start == proof.start)
        .and_then(|copy| copy.resume.ok())
}

/// The snapshot of `schema` for the proven process `proof` of tab `sid`, whose launch aterm
/// read as `launch` (`None`: no validated self-claim copy of it, or its exec environment could
/// not be read).
fn snapshot_in(
    sid: &str,
    proof: &crate::self_claim::Proven,
    env: &Env,
    schema: u8,
    launch: Option<&crate::harness_host::RunningLaunch>,
) -> Option<Snapshot> {
    let home = env.home.as_deref()?;
    let config = env.config.as_deref()?.parent()?;
    let seat = accounts_pool::Seat {
        config_dir: config,
        state: &env.state,
        home,
        now: env.now,
    };
    if schema == 1 {
        let path = config.join(accounts::Cli::Codex.accounts().roster_file);
        let records = seat
            .store(accounts::Cli::Codex)
            .peek_account_records(&path, env.now)
            .ok()?;
        // A credential path or None==None is not a proved assignment. Until the
        // native slot's identity/generation is available, display unknown.
        let value = json!({"schema": 1, "observed_at": env.now,
            "revision": accounts::pool_authority_revision(&records.raw_roster, &records.logins).ok()?,
            "session": {"sid": sid, "process_pid": proof.pid,
                "process_birth": proof.start, "epoch": host_epoch()?},
            "assignment": {"account_id": Value::Null, "mode": "unknown"},
            "accounts": rows(&records, home, env.now, &|_| false, None)?,
            "capabilities": {"add": false, "select": false, "auto": false}});
        return Some(Snapshot {
            value,
            select: false,
            auto: false,
        });
    }
    // Schema 2, and schema 3 with each row's reason. The row is the one the session's launch
    // environment names in the roster, as its cold relaunch would be judged: a judgement, not
    // a proved identity. A row is named only where this snapshot lists it. Each row's
    // condition, its `eligible` and its reason are read off one reading, the one a move is
    // judged on.
    let records = accounts_pool::move_records(&seat, accounts::Cli::Codex).ok()?;
    let moves = launch.and_then(|launch| {
        accounts_pool::moves_in(
            &seat,
            accounts::Cli::Codex,
            &records,
            &launch.login_env(),
            launch.resume.argv.first().map(std::path::Path::new),
        )
        .ok()
    });
    let id_of = |label: &str| records.roster.row(label).map(|row| account_id(&row.label));
    let eligible = |label: &str| {
        moves
            .as_ref()
            .is_some_and(|moves| moves.refusal(&Wanted::Row(label.to_owned())).is_none())
    };
    // A row with no directory of its own is on the default login, whose refusal is read where
    // the accounts store reads that login's lapse, as the reports read it.
    let default_login = seat.store(accounts::Cli::Codex).default_login_home();
    let signed_out_reason = |row: &accounts::Row, condition| {
        codex_app_login::row_signed_out_reason(row, condition, default_login.as_deref(), env.now)
    };
    let reason = (schema == 3).then_some(&signed_out_reason as SignedOutReason<'_>);
    let value = json!({"schema": schema, "observed_at": env.now,
        "revision": accounts::pool_authority_revision(&records.raw_roster, &records.logins).ok()?,
        "session": {"sid": sid, "process_pid": proof.pid,
            "process_birth": proof.start, "epoch": host_epoch()?},
        "assignment": {"account_id": moves.as_ref().and_then(|moves| id_of(&moves.ran))},
        "pool": {
            "choice_id": moves.as_ref().and_then(|moves| moves.choice.as_deref()).and_then(id_of),
            "pinned": records.state.pinned,
            "rotation": seat.mode_for(accounts::Cli::Codex).as_str()},
        "accounts": rows(&records, home, env.now, &eligible, reason)?,
        // The pool's side only; `serve` joins whether the restart would be carried.
        "capabilities": {"add": false, "select": false, "auto": false}});
    Some(Snapshot {
        value,
        select: moves.as_ref().is_some_and(accounts_pool::Moves::offered),
        auto: moves
            .as_ref()
            .is_some_and(|moves| moves.offered() && moves.refusal(&Wanted::Pool).is_none()),
    })
}

/// Whether `restart account=` would be carried for the proven process `proof` now, as far as
/// its session and host go: the act's own fences pass ([`crate::restart_act::unavailable`]),
/// this connection kept its restart authority ([`untyped_refusal`]), a harness state directory
/// holds its stamp, no restart of it stands there already (whose destination every other ask
/// is refused against, `account-restart-pending`), and the act's account fence finds a shell to
/// relaunch it at and no renewal or exec handoff in flight
/// ([`crate::restart_act::account_fence`]). What the pool says is the snapshot's
/// ([`Snapshot::select`]).
fn move_offered(ctx: &crate::SessionCtx, proof: &crate::self_claim::Proven) -> bool {
    carried(
        ctx,
        proof,
        crate::harness_host::harness_state().as_deref(),
        untyped_refusal(ctx).is_some(),
    )
}

/// [`move_offered`], with the harness state directory `state` and where this connection gave
/// up its restart authority (`denied`).
fn carried(
    ctx: &crate::SessionCtx,
    proof: &crate::self_claim::Proven,
    state: Option<&std::path::Path>,
    denied: bool,
) -> bool {
    let Ok(pgrp) = i32::try_from(proof.pid) else {
        return false;
    };
    let Some(state) = state else {
        return false;
    };
    let now = aterm_time::unix_secs().unwrap_or(0);
    // A standing restart refuses every ask but one for its own destination: no move is offered
    // until it is carried or lapses.
    let standing = aterm_agent::harness::restart_stamp::standing(
        state,
        ctx.self_id.as_str(),
        proof.pid,
        &proof.start,
        now,
    );
    let judged = crate::restart_act::Judged {
        pid: proof.pid,
        start: proof.start.clone(),
        copied: true,
        // The offer reads no rest: the act judges that when it is asked.
        home: None,
        refusal: None,
    };
    standing.is_none()
        && !denied
        && crate::restart_act::unavailable(ctx, pgrp, &proof.start, crate::metrics::now_us())
            .is_none()
        && crate::restart_act::account_fence(
            ctx,
            (proof.pid, &proof.start),
            &Wanted::Pool,
            Some(&judged),
            None,
        )
        .is_none()
}

/// A schema 2 or 3 snapshot's capabilities, true only where both the pool ([`Snapshot`]) and the
/// session ([`move_offered`]) allow the move. `add` stays false: nothing here signs anyone in.
fn grant(snapshot: &mut Snapshot, offered: bool) {
    let (select, auto) = (snapshot.select && offered, snapshot.auto && offered);
    if let Some(capabilities) = snapshot
        .value
        .as_object_mut()
        .and_then(|top| top.get_mut("capabilities"))
        .and_then(Value::as_object_mut)
    {
        capabilities.insert("select".into(), Value::from(select));
        capabilities.insert("auto".into(), Value::from(auto));
    }
}

struct Job {
    proof: crate::self_claim::Proven,
    started: Instant,
    result: Mutex<Option<Option<Snapshot>>>,
}

/// One worker per session, request id and schema: a schema 2 request never changes what a
/// schema 1 request of the same id is answered.
type Jobs = BTreeMap<(String, String, u8), Arc<Job>>;
fn jobs() -> &'static Mutex<Jobs> {
    static JOBS: OnceLock<Mutex<Jobs>> = OnceLock::new();
    JOBS.get_or_init(Mutex::default)
}

fn same_binding(
    before: &crate::self_claim::Proven,
    after: Option<&crate::self_claim::Proven>,
    alive: bool,
) -> bool {
    alive && after == Some(before)
}

fn prune(table: &mut Jobs) {
    table.retain(|_, job| {
        job.started.elapsed() < Duration::from_secs(120)
            || job
                .result
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_none()
    });
}

pub(crate) fn serve(ctx: &Arc<crate::SessionCtx>, master: i32, line: &str) -> String {
    let Some((schema, id)) = request(line) else {
        return "ERR codex-accounts invalid request\n".into();
    };
    let Some(before) = claim(ctx, master) else {
        return "ERR codex-accounts denied\n".into();
    };
    let key = (ctx.self_id.as_str().to_owned(), id.to_owned(), schema);
    let job = {
        let mut table = jobs().lock().unwrap_or_else(|p| p.into_inner());
        // A blocked worker retains its slot until terminal; expired requests
        // cannot multiply the number of background filesystem readers.
        prune(&mut table);
        if let Some(job) = table.get(&key) {
            if job.proof != before {
                return "ERR codex-accounts stale session\n".into();
            }
            Arc::clone(job)
        } else {
            if table.len() >= 32 {
                return "ERR codex-accounts busy\n".into();
            }
            let job = Arc::new(Job {
                proof: before.clone(),
                started: Instant::now(),
                result: Mutex::new(None),
            });
            let worker = Arc::clone(&job);
            let sid = key.0.clone();
            if std::thread::Builder::new()
                .name("codex-pool-snapshot".into())
                .spawn(move || {
                    let value = snapshot(&sid, &worker.proof, schema);
                    *worker.result.lock().unwrap_or_else(|p| p.into_inner()) = Some(value);
                })
                .is_err()
            {
                return "ERR codex-accounts unavailable\n".into();
            }
            table.insert(key, Arc::clone(&job));
            job
        }
    };
    let result = job.result.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let Some(result) = result else {
        return format!("OK codex-accounts state=waiting request={id}\n");
    };
    let Some(snapshot) = result else {
        return "ERR codex-accounts unavailable\n".into();
    };
    // A process replacement or changed PTY foreground invalidates even a read.
    if !same_binding(&before, claim(ctx, master).as_ref(), before.alive()) {
        return "ERR codex-accounts stale session\n".into();
    }
    // Judged here, on the dispatch thread at reply time, where the serving connection and the
    // session's hands are known.
    reply(id, schema, snapshot, || move_offered(ctx, &before))
}

/// The reply that carries `snapshot` for request `id`. Only a schema 2 or 3 snapshot asks
/// whether the move would be carried (`offered`) and is granted a capability; schema 1 asks
/// nothing of the session and says none.
fn reply(id: &str, schema: u8, mut snapshot: Snapshot, offered: impl FnOnce() -> bool) -> String {
    if schema >= 2 {
        grant(&mut snapshot, offered());
    }
    let Some(object) = snapshot.value.as_object_mut() else {
        return "ERR codex-accounts unavailable\n".into();
    };
    object.insert("request".into(), Value::String(id.into()));
    match aterm_json::to_string(&snapshot.value) {
        Ok(body) if body.len() + "OK codex-accounts \n".len() <= MAX_FRAME => {
            format!("OK codex-accounts {body}\n")
        }
        _ => "ERR codex-accounts snapshot too large\n".into(),
    }
}

#[cfg(test)]
#[path = "codex_accounts_control_tests.rs"]
mod tests;
