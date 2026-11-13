// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Session-bound, credential-free account observations. These observations grant
//! no assignment, renewal or restart authority.

use super::*;
use aterm_agent::harness::{accounts, accounts_pool, cli::Env};
use aterm_json::{Value, json};
use std::sync::OnceLock;
use std::time::Instant;

const MAX_ROWS: usize = 128;
const MAX_FRAME: usize = 64 * 1024;

fn request(line: &str) -> Option<&str> {
    let mut words = line.split_ascii_whitespace();
    if words.next()? != "snapshot" || words.next()? != "schema=1" {
        return None;
    }
    let id = words.next()?.strip_prefix("request=")?;
    (words.next().is_none()
        && id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some(id)
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

fn rows(
    records: &accounts::AccountRecords,
    home: &std::path::Path,
    now: i64,
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
            Some(json!({
                "id": account_id(&row.label), "label": row.label,
                "masked_email": masked_email(row.email.as_deref()),
                "condition": condition.as_str(), "enrolled": present,
                "held": row.held_by.is_some(),
                // Cached quota and file presence cannot establish fresh provider
                // authentication or permission to launch under this account.
                "eligible": false,
                "quota": {"observed_at": sample.map(|s| s.at),
                    "stale": sample.is_none_or(|s| now.saturating_sub(s.at) > 300),
                    "five_hour_used_percent": five.map(|v| v.0),
                    "five_hour_reset_at": five.map(|v| v.1),
                    "weekly_used_percent": week.map(|v| v.0),
                    "weekly_reset_at": week.map(|v| v.1)}
            }))
        })
        .collect()
}

fn snapshot(sid: &str, proof: &crate::self_claim::Proven) -> Option<Value> {
    let env = Env::from_process().ok()?;
    snapshot_in(sid, proof, &env)
}

fn snapshot_in(sid: &str, proof: &crate::self_claim::Proven, env: &Env) -> Option<Value> {
    let home = env.home.as_deref()?;
    let config = env.config.as_deref()?.parent()?;
    let seat = accounts_pool::Seat {
        config_dir: config,
        state: &env.state,
        home,
        now: env.now,
    };
    let path = config.join(accounts::Cli::Codex.accounts().roster_file);
    let records = seat
        .store(accounts::Cli::Codex)
        .peek_account_records(&path, env.now)
        .ok()?;
    // A credential path or None==None is not a proved assignment. Until the
    // native slot's identity/generation is available, display unknown.
    Some(json!({"schema": 1, "observed_at": env.now,
        "revision": accounts::pool_authority_revision(&records.raw_roster, &records.logins).ok()?,
        "session": {"sid": sid, "process_pid": proof.pid,
            "process_birth": proof.start, "epoch": host_epoch()?},
        "assignment": {"account_id": Value::Null, "mode": "unknown"},
        "accounts": rows(&records, home, env.now)?,
        "capabilities": {"add": false, "select": false, "auto": false}}))
}

struct Job {
    proof: crate::self_claim::Proven,
    started: Instant,
    result: Mutex<Option<Option<Value>>>,
}

type Jobs = BTreeMap<(String, String), Arc<Job>>;
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
    let Some(id) = request(line) else {
        return "ERR codex-accounts invalid request\n".into();
    };
    let Some(before) = claim(ctx, master) else {
        return "ERR codex-accounts denied\n".into();
    };
    let key = (ctx.self_id.as_str().to_owned(), id.to_owned());
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
                    let value = snapshot(&sid, &worker.proof);
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
    let Some(mut value) = result else {
        return "ERR codex-accounts unavailable\n".into();
    };
    // A process replacement or changed PTY foreground invalidates even a read.
    if !same_binding(&before, claim(ctx, master).as_ref(), before.alive()) {
        return "ERR codex-accounts stale session\n".into();
    }
    let Some(object) = value.as_object_mut() else {
        return "ERR codex-accounts unavailable\n".into();
    };
    object.insert("request".into(), Value::String(id.into()));
    match aterm_json::to_string(&value) {
        Ok(body) if body.len() + "OK codex-accounts \n".len() <= MAX_FRAME => {
            format!("OK codex-accounts {body}\n")
        }
        _ => "ERR codex-accounts snapshot too large\n".into(),
    }
}

#[cfg(test)]
#[path = "codex_accounts_control_tests.rs"]
mod tests;
