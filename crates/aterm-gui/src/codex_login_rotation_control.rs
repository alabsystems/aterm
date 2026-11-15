// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `codex-login-rotation`: a native Codex session's move at a lost login. The lost-login decision
//! ([`login::prepare`]) runs on a worker within its read ledger. The table keeps that ledger, and
//! the move the decision selected, for one process, thread and wall turn ([`LedgerKey`]): across
//! the jobs a person's input replaces, and across the other wall turns the process is asked for
//! in between, so a loss is read once whichever job asks and a move selected while a person typed
//! is published once they are done, with no further read. Only a fresh, proven host request
//! publishes a selection, as a native restart receipt its session-scoped writer judges again. A
//! publication that keeps failing never reads again: the selection lapses into not-needed once it
//! is [`OBSERVATION_MS`] old.

use super::*;
use aterm_agent::harness::{accounts_pool, codex_login_rotation as login, codex_rotation as pool};
use std::time::Instant;

const WAITING: &str = "OK codex-login-rotation state=waiting\n";
const NOT_NEEDED: &str = "OK codex-login-rotation state=not-needed\n";
const RETRY_MS: u64 = 5_000;
const OBSERVATION_MS: u64 = 60_000;
/// How many lost logins (wall turns of its threads) one process is served. A further one stays a
/// stop for a person, so the provider reads one process can cause are bounded outright, each
/// loss's by its own ledger.
const LEDGERS: usize = 32;

/// What a job reads of its host beyond the session's own tables: the claimed process's live
/// record, and the provider. [`System`] is the host itself; a test stands in for both, so the job
/// runs whole with no agent process and no provider.
pub(super) trait Host: Send + Sync {
    /// The live session record of the process `pid` started at `start`
    /// ([`self_claimed::read_copy`]).
    fn copy(&self, pid: u32, start: &str) -> Option<self_claimed::Copy>;

    /// The reader one decision reads its logins with: the live one, or under the pinned
    /// `fixture_roots` of a Rust fixture's isolated usage server.
    fn reader<'a>(
        &'a self,
        live: pool::LiveUsage<'a>,
        fixture_roots: Option<Vec<Vec<u8>>>,
    ) -> Box<dyn pool::UsageReader + 'a>;
}

/// The host as it is.
pub(super) struct System;

impl Host for System {
    fn copy(&self, pid: u32, start: &str) -> Option<self_claimed::Copy> {
        self_claimed::read_copy(pid, start, None)
    }

    fn reader<'a>(
        &'a self,
        live: pool::LiveUsage<'a>,
        fixture_roots: Option<Vec<Vec<u8>>>,
    ) -> Box<dyn pool::UsageReader + 'a> {
        #[cfg(debug_assertions)]
        if let Some(roots) = fixture_roots {
            return Box::new(rotation::FixtureUsage::new(live, roots));
        }
        #[cfg(not(debug_assertions))]
        let _ = fixture_roots;
        Box::new(live)
    }
}

/// What one process's lost login on one wall turn of one thread is: what its decision read and
/// selected is kept under it ([`Reads`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::native_act) struct LedgerKey {
    pid: u32,
    birth: u64,
    thread: String,
    turn: String,
}

/// What the table keeps of one [`LedgerKey`]'s loss across the jobs that ask for it.
#[derive(Debug, Default)]
pub(in crate::native_act) struct Reads {
    /// The provider reads its decision still admits.
    ledger: login::ReadLedger,
    /// The move its one pass over the candidates selected, until it lapses unpublished.
    selected: Option<Selection>,
}

/// The losses of the session's claimed process, oldest first, at most [`LEDGERS`]. The lock of a
/// [`Reads`] is held for a whole decision, so a job that replaced another for the same key
/// neither reads nor publishes before that one's reads are done.
pub(in crate::native_act) type Ledgers = Vec<(LedgerKey, Arc<Mutex<Reads>>)>;

#[derive(Debug)]
pub(in crate::native_act) struct Preparation {
    sid: String,
    intent: Intent,
    process: Process,
    start: String,
    human_seq: u64,
    starts: u64,
    progress: Mutex<Progress>,
}

/// Where one job stands. A selected move is no state of the job: it is kept with its key's
/// [`Reads`], and an ask whose retry has come publishes it before it would read.
#[derive(Debug)]
enum Progress {
    Reading,
    Retry(Instant),
    NotNeeded,
}

#[derive(Debug)]
struct Selection {
    next: login::LoginMove,
    observed: Instant,
    /// When a publication that failed may be tried again, with no new read.
    publish_at: Option<Instant>,
}

impl Preparation {
    fn matches(
        &self,
        intent: &Intent,
        process: &Process,
        proof: &crate::self_claim::Proven,
        human_seq: u64,
        starts: u64,
    ) -> bool {
        self.intent == *intent
            && self.process == *process
            && self.start == proof.start
            && process.pid == proof.pid
            && self.human_seq == human_seq
            && self.starts == starts
    }

    fn key(&self) -> LedgerKey {
        LedgerKey {
            pid: self.process.pid,
            birth: self.process.birth,
            thread: self.intent.thread.clone(),
            turn: self.intent.wall.clone(),
        }
    }

    fn current(self: &Arc<Self>, ctx: &crate::SessionCtx, host: &dyn Host) -> bool {
        ctx.fabric.hold().is_none()
            && ctx.human_input.seq() == self.human_seq
            && ctx
                .fenced_writes
                .native
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .login_rotation_prepare
                .as_ref()
                .is_some_and(|job| Arc::ptr_eq(job, self))
            && Process::observe(self.process.pid).as_ref() == Some(&self.process)
            && host.copy(self.process.pid, &self.start).and_then(|copy| {
                let current = record(&copy)?;
                Some(
                    self.intent.matches_record(&current, None)
                        && renewal::history(&copy.home, &self.intent).is_some_and(|history| {
                            self.intent.matches_history(&history) && history.starts == self.starts
                        }),
                )
            }) == Some(true)
    }
}

fn index(job: &Preparation) -> Option<renewal::IndexKey> {
    renewal::IndexKey::moved(&job.intent, job.process.pid, job.process.birth, &job.start).ok()
}

/// What `ctx`'s table keeps of `job`'s key: the entry there for that key, whatever other wall
/// turns the process was asked for since, else a new one. Another process's entries are over (the
/// session has one claimed process, and a relaunch is a new loss to observe). `None` once the
/// process has been served [`LEDGERS`] losses and this would be a further one.
fn reads(ctx: &crate::SessionCtx, job: &Preparation) -> Option<Arc<Mutex<Reads>>> {
    let key = job.key();
    let mut table = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let kept = &mut table.login_rotation_reads;
    kept.retain(|(held, _)| held.pid == key.pid && held.birth == key.birth);
    if let Some((_, reads)) = kept.iter().find(|(held, _)| *held == key) {
        return Some(Arc::clone(reads));
    }
    if kept.len() >= LEDGERS {
        return None;
    }
    let reads = Arc::new(Mutex::new(Reads::default()));
    kept.push((key, Arc::clone(&reads)));
    Some(reads)
}

fn observe(
    host: &dyn Host,
    env: &Env,
    resume: &self_claimed::Resume,
    job: &Preparation,
    fixture_roots: Option<Vec<Vec<u8>>>,
    ledger: &mut login::ReadLedger,
) -> Result<login::LoginDecision, &'static str> {
    let home = env.home.as_deref().ok_or("home")?;
    let seat = accounts_pool::Seat {
        config_dir: env
            .config
            .as_deref()
            .and_then(Path::parent)
            .ok_or("config")?,
        state: &env.state,
        home,
        now: env.now,
    };
    let mut variables = Vec::new();
    // An explicit, kernel-validated auth file names the managed login. Its
    // shared CODEX_HOME still carries config/history in the native command.
    if resume.codex_auth_path.is_none()
        && let Some(home) = &resume.codex_home
    {
        variables.push(("CODEX_HOME", home.as_str()));
    }
    if let Some(path) = &resume.codex_auth_path {
        variables.push(("CODEX_AUTH_PATH", path.as_str()));
    }
    let session = accounts_pool::Session {
        cli: accounts::Cli::Codex,
        sid: &job.sid,
        supervisor: accounts::Supervisor::Own,
        env: &variables,
        seen: &aterm_agent::harness::observe::Observation::Unread,
        sample: None,
        point: accounts::Point::LoginEnded,
        mode: seat.mode_for(accounts::Cli::Codex),
        relaunch: (),
    };
    let config_home = resume
        .codex_home
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let live = pool::LiveUsage {
        user_home: home,
        config_home: &config_home,
    };
    let wait = Duration::from_millis(OBSERVATION_MS);
    let reader = host.reader(live, fixture_roots);
    login::prepare(
        &seat,
        &session,
        &job.intent.identity,
        &*reader,
        wait,
        ledger,
    )
}

/// Keeps the move `result` selected, at `now`, with its key's reads for an ask to publish. It is
/// kept whether or not the job that read it is still the session's: the decision spent the key's
/// one pass over the candidates, so a job a person's input replaced meanwhile could not select it
/// again, and it is published only by a fresh, proven request, which its writer judges again.
fn keep_selection(
    sid: &str,
    kept: &mut Reads,
    result: &Result<login::LoginDecision, &'static str>,
    now: Instant,
) {
    let Ok(login::LoginDecision::Move(next)) = result else {
        return;
    };
    aterm_log::info!(
        "codex login rotation: session {sid} moves off {} ({}) to {}",
        next.source.label,
        next.reason.code(),
        next.destination.label
    );
    kept.selected = Some(Selection {
        next: (**next).clone(),
        observed: now,
        publish_at: None,
    });
}

/// What the job does after a decision made at `now` (`unix_now` in Unix seconds), `current` where
/// the job is still the session's: after a move ([`keep_selection`]) the next ask publishes it,
/// whichever job asks; a stop is answered as not-needed; and anything else is asked again, no
/// sooner than [`RETRY_MS`] and not before the records can next decide it.
fn progress_after_observation(
    sid: &str,
    result: Result<login::LoginDecision, &'static str>,
    current: bool,
    (now, unix_now): (Instant, i64),
) -> Progress {
    let retry = now + Duration::from_millis(RETRY_MS);
    match result {
        Ok(login::LoginDecision::Move(_)) => Progress::Retry(now),
        Ok(login::LoginDecision::NotNeeded(end)) if current => {
            aterm_log::info!("codex login rotation: none is coming for session {sid}: {end}");
            Progress::NotNeeded
        }
        Ok(login::LoginDecision::Wait { until: Some(until) }) => {
            let after = u64::try_from(until.saturating_sub(unix_now)).unwrap_or(0);
            Progress::Retry(retry.max(now + Duration::from_secs(after)))
        }
        _ => Progress::Retry(retry),
    }
}

fn start(
    host: &Arc<dyn Host>,
    ctx: &Arc<crate::SessionCtx>,
    env: &Env,
    resume: &self_claimed::Resume,
    job: &Arc<Preparation>,
    reads: Arc<Mutex<Reads>>,
) {
    let weak = Arc::downgrade(ctx);
    let fixture_roots = ctx.native_policy.quota_fixture_roots();
    let host = Arc::clone(host);
    let env = env.clone();
    let resume = resume.clone();
    let worker = Arc::clone(job);
    if std::thread::Builder::new()
        .name("codex-login-probe".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut kept = reads.lock().unwrap_or_else(|p| p.into_inner());
                let result = observe(
                    &*host,
                    &env,
                    &resume,
                    &worker,
                    fixture_roots,
                    &mut kept.ledger,
                );
                keep_selection(&worker.sid, &mut kept, &result, Instant::now());
                result
            }))
            .unwrap_or(Err("probe-panicked"));
            let current = weak
                .upgrade()
                .is_some_and(|ctx| worker.current(&ctx, &*host));
            let unix_now = aterm_agent::supervise::limit::unix_now();
            *worker.progress.lock().unwrap_or_else(|p| p.into_inner()) = progress_after_observation(
                &worker.sid,
                result,
                current,
                (Instant::now(), unix_now),
            );
        })
        .is_err()
    {
        *job.progress.lock().unwrap_or_else(|p| p.into_inner()) =
            Progress::Retry(Instant::now() + Duration::from_millis(RETRY_MS));
    }
}

/// Whether aterm.toml lets a lost login's move run: as a usage-limit handoff's
/// ([`pool::policy_allows_handoff`]), on a host qualified for it.
fn allowed(ctx: &crate::SessionCtx, env: &Env) -> bool {
    if !ctx.native_policy.login_rotation_enabled() {
        return false;
    }
    let (policy, warnings) =
        aterm_agent::supervise::SupervisorConfig::from_path(env.config.as_deref());
    pool::policy_allows_handoff(&policy, &warnings)
}

pub(super) fn prepare(
    host: &Arc<dyn Host>,
    ctx: &Arc<crate::SessionCtx>,
    env: &Env,
    proof: &crate::self_claim::Proven,
    copy: &self_claimed::Copy,
    current: &aterm_json::Value,
    intent: Intent,
) -> String {
    if !allowed(ctx, env) {
        return NOT_NEEDED.into();
    }
    let Some(process) = Process::observe(proof.pid) else {
        return WAITING.into();
    };
    let Some(starts) = intent.marker(current)["starts"].as_u64() else {
        return WAITING.into();
    };
    let Ok(resume) = &copy.resume else {
        return WAITING.into();
    };
    let job = {
        let mut table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(job) = table
            .login_rotation_prepare
            .as_ref()
            .filter(|job| job.matches(&intent, &process, proof, ctx.human_input.seq(), starts))
        {
            Arc::clone(job)
        } else {
            let job = Arc::new(Preparation {
                sid: ctx.self_id.as_str().into(),
                intent,
                process,
                start: proof.start.clone(),
                human_seq: ctx.human_input.seq(),
                starts,
                progress: Mutex::new(Progress::Retry(Instant::now())),
            });
            table.login_rotation_prepare = Some(Arc::clone(&job));
            job
        }
    };
    let Some(index) = index(&job) else {
        return WAITING.into();
    };
    match renewal::with_receipt_writer(env, |writer| writer.read_indexed(&index)) {
        Ok(Some(Some(receipt))) => {
            if receipt.intent != job.intent
                || receipt.old_pid != job.process.pid
                || receipt.old_birth != job.process.birth
                || receipt.sid != ctx.self_id.as_str()
            {
                return WAITING.into();
            }
            if !receipt.may_replace() {
                if let Ok(Some(retired)) =
                    rotation::retire_stale_ready(ctx, env, proof, current, &receipt)
                {
                    return retired.reply();
                }
                return acknowledged_receipt_reply(env, &receipt);
            }
        }
        Err(_) | Ok(None) => return WAITING.into(),
        Ok(Some(None)) => {}
    }
    // Looked up before the job's own lock: the table's lock is never taken under it.
    let reads = reads(ctx, &job);
    let mut progress = job.progress.lock().unwrap_or_else(|p| p.into_inner());
    match &*progress {
        Progress::Reading => return WAITING.into(),
        Progress::Retry(at) if Instant::now() < *at => return WAITING.into(),
        Progress::NotNeeded => return NOT_NEEDED.into(),
        Progress::Retry(_) => {}
    }
    let Some(reads) = reads else {
        aterm_log::info!(
            "codex login rotation: none is coming for session {}: this process was already \
             served {LEDGERS} lost logins",
            job.sid
        );
        *progress = Progress::NotNeeded;
        return NOT_NEEDED.into();
    };
    // A decision for this key still reading (for a job this one replaced) holds its lock.
    let mut kept = match reads.try_lock() {
        Ok(kept) => kept,
        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return WAITING.into(),
    };
    let Some(selected) = kept.selected.as_mut() else {
        drop(kept);
        *progress = Progress::Reading;
        drop(progress);
        start(host, ctx, env, resume, &job, reads);
        return WAITING.into();
    };
    let next = selected.next.clone();
    if let Some(answer) = publication(selected, Instant::now(), || {
        publish(ctx, env, proof, resume, &job, &next, &index)
    }) {
        return answer;
    }
    aterm_log::info!(
        "codex login rotation: the move of session {} to {} lapsed unpublished",
        job.sid,
        next.destination.label
    );
    kept.selected = None;
    *progress = Progress::NotNeeded;
    NOT_NEEDED.into()
}

/// What an ask at `now` answers of `selected`, publishing it with `publish` where it may: the
/// receipt's reply once published; waiting while a publication that failed waits out its retry,
/// with no new read; `None` once the selection is [`OBSERVATION_MS`] old, which the job answers
/// as not-needed: a selection is never read again.
fn publication(
    selected: &mut Selection,
    now: Instant,
    publish: impl FnOnce() -> Result<Receipt, &'static str>,
) -> Option<String> {
    if now.saturating_duration_since(selected.observed) >= Duration::from_millis(OBSERVATION_MS) {
        return None;
    }
    if selected.publish_at.is_some_and(|at| now < at) {
        return Some(WAITING.into());
    }
    Some(match publish() {
        Ok(receipt) => {
            selected.publish_at = None;
            receipt.reply()
        }
        Err(_) => {
            selected.publish_at = Some(now + Duration::from_millis(RETRY_MS));
            WAITING.into()
        }
    })
}

fn publish(
    ctx: &crate::SessionCtx,
    env: &Env,
    proof: &crate::self_claim::Proven,
    resume: &self_claimed::Resume,
    job: &Preparation,
    selected: &login::LoginMove,
    index: &renewal::IndexKey,
) -> Result<Receipt, &'static str> {
    let authority = ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .supervisor_authority(crate::metrics::now_us());
    if authority.identity() != Some(proof)
        || ctx.human_input.seq() != job.human_seq
        || ctx.fabric.hold().is_some()
    {
        return Err("claim-changed");
    }
    let next = selected.successor();
    let id = aterm_agent::harness::native_act::operation_id().map_err(|_| "entropy")?;
    let prepare = make_prepare(
        ctx,
        resume,
        job.process.clone(),
        &next,
        &id,
        authority.revision(),
    )
    .ok_or("plan")?;
    let receipt = Receipt {
        destination: Some(next.identity),
        id,
        intent: job.intent.clone(),
        sid: ctx.self_id.as_str().into(),
        old_pid: job.process.pid,
        old_birth: job.process.birth,
        claim_revision: authority.revision(),
        human_seq: job.human_seq,
        old_auth: next.old_auth,
        new_auth: next.new_auth,
        generation: next.generation,
        row_stamp: renewal::row_stamp(&next.row),
        native_prepare: aterm_agent::harness::native_act::Request::prepare(prepare)
            .encode()
            .ok_or("plan")?,
        expires_ms: now_ms().saturating_add(120_000),
        phase: Phase::Ready,
        successor_pid: None,
        successor_birth: None,
        accepted_turn: None,
        launch_attempted: false,
        candidate: None,
        current_attempt: None,
        revoked: false,
    };
    pool::with_prepared_writer(env, &receipt, |_, writer| {
        let prior = writer.read_indexed(index)?;
        if prior.as_ref().is_some_and(|old| !old.may_replace()) {
            return Err("existing-receipt");
        }
        writer.publish_indexed(index, prior.as_ref(), &receipt)?;
        Ok(receipt.clone())
    })?
    .ok_or("writer-busy")
}

#[cfg(test)]
#[path = "codex_login_rotation_control_tests.rs"]
mod tests;
