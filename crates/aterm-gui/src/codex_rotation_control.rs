// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Provider reads produce observations on a worker. Only a fresh, proven host
//! request may publish them as a native restart receipt.

use super::*;
use aterm_agent::harness::{accounts_pool, codex_rotation as pool, relaunch};
use std::time::Instant;

const WAITING: &str = "OK codex-rotation state=waiting\n";
const RETRY_MS: u64 = 5_000;
const OBSERVATION_MS: u64 = 60_000;

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

#[derive(Debug)]
enum Progress {
    Reading,
    Retry(Instant),
    NotNeeded,
    Selected(Box<Selection>),
}

#[derive(Debug)]
struct Selection {
    proposal: pool::Proposal,
    observed: Instant,
    receipt: Option<Receipt>,
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

    fn current(self: &Arc<Self>, ctx: &crate::SessionCtx) -> bool {
        ctx.fabric.hold().is_none()
            && ctx.human_input.seq() == self.human_seq
            && ctx
                .fenced_writes
                .native
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .rotation_prepare
                .as_ref()
                .is_some_and(|job| Arc::ptr_eq(job, self))
            && Process::observe(self.process.pid).as_ref() == Some(&self.process)
            && self_claimed::read_copy(self.process.pid, &self.start, None).and_then(|copy| {
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

fn index(env: &Env, job: &Preparation) -> Option<PathBuf> {
    let key = aterm_json::to_string(&(&job.intent, job.process.pid, job.process.birth, &job.start))
        .ok()?;
    let digest = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(key.as_bytes()));
    Some(
        env.accounts_store_for(accounts::Cli::Codex)?
            .dir
            .join("renewal-follow")
            .join(format!("quota-intent-{digest}")),
    )
}

#[cfg(debug_assertions)]
struct FixtureUsage<'a> {
    live: pool::LiveUsage<'a>,
    roots: Vec<Vec<u8>>,
}

#[cfg(debug_assertions)]
impl pool::UsageReader for FixtureUsage<'_> {
    fn read(
        &self,
        home: &Path,
        expected_email: Option<&str>,
        now: i64,
        wait: Duration,
    ) -> Option<aterm_agent::harness::codex_app_login::IdentifiedUsage> {
        aterm_agent::harness::codex_app_login::read_identified_login_with_fixture_roots(
            home,
            self.live.config_home,
            self.live.user_home,
            wait,
            now,
            expected_email,
            self.roots.clone(),
        )
    }
}

fn observe(
    env: &Env,
    resume: &self_claimed::Resume,
    job: &Preparation,
    fixture_roots: Option<Vec<Vec<u8>>>,
) -> Result<Option<pool::Proposal>, &'static str> {
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
    let shell = Process::observe(job.process.parent).ok_or("shell")?;
    let shell_path = std::str::from_utf8(&shell.image.path).map_err(|_| "shell")?;
    let dialect = aterm_agent::harness::upgrade::Dialect::from_exe_name(
        Path::new(shell_path)
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("shell")?,
    )
    .ok_or("shell")?;
    let exe = std::str::from_utf8(&job.process.image.path).map_err(|_| "image")?;
    let session = accounts_pool::Session {
        cli: accounts::Cli::Codex,
        sid: &job.sid,
        supervisor: accounts::Supervisor::Own,
        env: &variables,
        sample: None,
        point: accounts::Point::Wall {
            window: "seven_day".into(),
            at: Some(env.now),
        },
        mode: seat.mode_for(accounts::Cli::Codex),
        relaunch: relaunch::PoolRelaunch {
            dialect,
            cd: resume.cwd.as_deref(),
            exe: Path::new(exe),
            flags: resume.argv.get(1..).ok_or("argv")?,
            resume: Some(&job.intent.thread),
            home: None,
        },
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
    #[cfg(debug_assertions)]
    if let Some(roots) = fixture_roots {
        return pool::prepare(
            &seat,
            &session,
            &job.intent.identity,
            &FixtureUsage { live, roots },
            Duration::from_millis(OBSERVATION_MS),
        );
    }
    #[cfg(not(debug_assertions))]
    let _ = fixture_roots;
    pool::prepare(
        &seat,
        &session,
        &job.intent.identity,
        &live,
        Duration::from_millis(OBSERVATION_MS),
    )
}

fn start(
    ctx: &Arc<crate::SessionCtx>,
    env: &Env,
    resume: &self_claimed::Resume,
    job: &Arc<Preparation>,
) {
    let weak = Arc::downgrade(ctx);
    let fixture_roots = ctx.native_policy.quota_fixture_roots();
    let env = env.clone();
    let resume = resume.clone();
    let worker = Arc::clone(job);
    if std::thread::Builder::new()
        .name("codex-quota-probe".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                observe(&env, &resume, &worker, fixture_roots)
            }))
            .unwrap_or(Err("probe-panicked"));
            let current = weak.upgrade().is_some_and(|ctx| worker.current(&ctx));
            *worker.progress.lock().unwrap_or_else(|p| p.into_inner()) = match result {
                Ok(Some(proposal)) if current => Progress::Selected(Box::new(Selection {
                    proposal,
                    observed: Instant::now(),
                    receipt: None,
                })),
                Err("not-weekly-exhausted") if current => Progress::NotNeeded,
                _ => Progress::Retry(Instant::now() + Duration::from_millis(RETRY_MS)),
            };
        })
        .is_err()
    {
        *job.progress.lock().unwrap_or_else(|p| p.into_inner()) =
            Progress::Retry(Instant::now() + Duration::from_millis(RETRY_MS));
    }
}

pub(super) fn prepare(
    ctx: &Arc<crate::SessionCtx>,
    env: &Env,
    proof: &crate::self_claim::Proven,
    copy: &self_claimed::Copy,
    current: &aterm_json::Value,
    intent: Intent,
) -> String {
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
            .rotation_prepare
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
            table.rotation_prepare = Some(Arc::clone(&job));
            job
        }
    };
    let Some(index) = index(env, &job) else {
        return WAITING.into();
    };
    match renewal::read_index(&index) {
        Ok(Some(id)) => {
            let Some(receipt) = renewal::read_receipt(env, &id) else {
                return WAITING.into();
            };
            if receipt.intent != job.intent
                || receipt.old_pid != job.process.pid
                || receipt.old_birth != job.process.birth
                || receipt.sid != ctx.self_id.as_str()
            {
                return WAITING.into();
            }
            if !receipt.may_replace() {
                if let Ok(Some(retired)) = retire_stale_ready(ctx, env, proof, current, &receipt) {
                    return retired.reply();
                }
                return receipt_reply(&receipt);
            }
        }
        Err(_) => return WAITING.into(),
        Ok(None) => {}
    }
    let mut progress = job.progress.lock().unwrap_or_else(|p| p.into_inner());
    match &mut *progress {
        Progress::Reading => WAITING.into(),
        Progress::Retry(at) if Instant::now() < *at => WAITING.into(),
        Progress::Retry(_) => {
            *progress = Progress::Reading;
            drop(progress);
            start(ctx, env, resume, &job);
            WAITING.into()
        }
        Progress::NotNeeded => "OK codex-rotation state=not-needed\n".into(),
        Progress::Selected(selected) => {
            if selected.observed.elapsed() >= Duration::from_millis(OBSERVATION_MS) {
                *progress = Progress::Retry(Instant::now());
                return WAITING.into();
            }
            let result = publish(ctx, env, proof, resume, &job, selected, &index);
            match result {
                Ok(receipt) => {
                    selected.receipt = Some(receipt.clone());
                    receipt.reply()
                }
                Err(_) => {
                    *progress = Progress::Retry(Instant::now() + Duration::from_millis(RETRY_MS));
                    WAITING.into()
                }
            }
        }
    }
}

fn publish(
    ctx: &crate::SessionCtx,
    env: &Env,
    proof: &crate::self_claim::Proven,
    resume: &self_claimed::Resume,
    job: &Preparation,
    selected: &Selection,
    index: &Path,
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
    let next = selected.proposal.successor().ok_or("selection")?;
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
    pool::with_prepared_writer(env, &receipt, |_, _| {
        if let Some(id) = renewal::read_index(index)?
            && renewal::read_receipt(env, &id).is_none_or(|old| !old.may_replace())
        {
            return Err("existing-receipt");
        }
        renewal::write_receipt(env, &receipt)?;
        aterm_agent::harness::upgrade_models::write_atomic_mode(index, &receipt.id, Some(0o600))
            .map_err(|_| "index")?;
        Ok(receipt.clone())
    })?
    .ok_or("writer-busy")
}

pub(super) fn commit_pool(
    ctx: &crate::SessionCtx,
    env: &Env,
    receipt: &Receipt,
) -> Result<(), &'static str> {
    if renewal::with_selected(env, receipt, |_| Ok(())).is_ok_and(|value| value.is_some()) {
        return Ok(());
    }
    let job = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .rotation_prepare
        .clone()
        .ok_or("proposal-missing")?;
    let progress = job.progress.lock().unwrap_or_else(|p| p.into_inner());
    let Progress::Selected(selected) = &*progress else {
        return Err("proposal-missing");
    };
    if selected.receipt.as_ref() != Some(receipt) {
        return Err("proposal-changed");
    }
    if selected.observed.elapsed() >= Duration::from_millis(OBSERVATION_MS) {
        return Err("proposal-expired");
    }
    if pool::with_prepared_writer(env, receipt, |_, _| Ok(()))?.is_none() {
        return Err("selection-unavailable");
    }
    selected.proposal.commit(receipt, env.now)
}

pub(super) fn retire_stale_ready(
    ctx: &crate::SessionCtx,
    env: &Env,
    proof: &crate::self_claim::Proven,
    current: &aterm_json::Value,
    receipt: &Receipt,
) -> Result<Option<Receipt>, &'static str> {
    let authority = ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .supervisor_authority(crate::metrics::now_us());
    let mut retired = receipt.clone();
    if receipt.phase != Phase::Ready
        || (same_native_host(receipt)
            && !retired.retire_stale_ready(authority.revision(), ctx.human_input.seq(), now_ms()))
    {
        return Ok(None);
    }
    retire_ready(ctx, env, proof, current, receipt)
}

pub(super) fn retire_ready(
    ctx: &crate::SessionCtx,
    env: &Env,
    proof: &crate::self_claim::Proven,
    current: &aterm_json::Value,
    receipt: &Receipt,
) -> Result<Option<Receipt>, &'static str> {
    if receipt.intent.purpose != renewal::Purpose::WeeklyQuota
        || receipt.phase != Phase::Ready
        || receipt.sid != ctx.self_id.as_str()
        || receipt.old_pid != proof.pid
        || Process::observe(proof.pid).is_none_or(|process| process.birth != receipt.old_birth)
        || !(receipt.intent.matches_record(current, Some(&receipt.id))
            || receipt.intent.matches_record(current, None))
    {
        return Ok(None);
    }
    renewal::with_receipt_writer(env, |writer| {
        if renewal::read_receipt(env, &receipt.id).as_ref() != Some(receipt)
            || !ready_native_no_go(ctx, &receipt.id)
        {
            return Ok(None);
        }
        let mut retired = receipt.clone();
        retired.phase = Phase::Retired;
        renewal::write_receipt(env, &retired)?;
        let _cleanup = writer.retire_pool_record(&retired);
        Ok(Some(retired))
    })
    .map(Option::flatten)
}

/// The exec source uses the same provider reader and pool as TUI recovery.
#[cfg(unix)]
pub(in crate::native_act) fn observe_exec(
    env: &Env,
    sid: &str,
    source: &aterm_agent::harness::codex_exec_handoff::ExecSource,
    intent: &Intent,
    fixture_roots: Option<Vec<Vec<u8>>>,
) -> Result<
    Option<pool::Proposal<aterm_agent::harness::codex_exec_handoff::ExecCommand>>,
    &'static str,
> {
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
    let live = pool::LiveUsage {
        user_home: home,
        config_home: &source.config_home,
    };
    #[cfg(debug_assertions)]
    if let Some(roots) = fixture_roots {
        return source.prepare(
            &seat,
            sid,
            &intent.identity,
            &FixtureUsage { live, roots },
            Duration::from_millis(OBSERVATION_MS),
        );
    }
    #[cfg(not(debug_assertions))]
    let _ = fixture_roots;
    source.prepare(
        &seat,
        sid,
        &intent.identity,
        &live,
        Duration::from_millis(OBSERVATION_MS),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(proof: &crate::self_claim::Proven) -> Preparation {
        Preparation {
            sid: "rotation-test".into(),
            intent: Intent {
                purpose: renewal::Purpose::WeeklyQuota,
                thread: "00000000-0000-0000-0000-000000000001".into(),
                wall: "00000000-0000-0000-0000-000000000002".into(),
                identity: renewal::Identity::from_native("source", "user"),
            },
            process: Process {
                pid: proof.pid,
                birth: 10,
                parent: 42,
                group: proof.pid,
                session: 42,
                image: Image {
                    path: b"/codex".to_vec(),
                    device: 1,
                    inode: 2,
                },
            },
            start: proof.start.clone(),
            human_seq: 3,
            starts: 1,
            progress: Mutex::new(Progress::Retry(Instant::now())),
        }
    }

    #[test]
    fn codex_rotation_probe_cannot_follow_a_replacement_process_wall_or_person_turn() {
        let proof = crate::session_timeline::scoped_input_tests::scoped(&[Scope::Relaunch]).proven;
        let job = job(&proof);
        assert!(job.matches(&job.intent, &job.process, &proof, 3, 1));
        let mut intent = job.intent.clone();
        intent.wall = "00000000-0000-0000-0000-000000000003".into();
        assert!(!job.matches(&intent, &job.process, &proof, 3, 1));
        intent = job.intent.clone();
        intent.identity = renewal::Identity::from_native("replacement", "user");
        assert!(!job.matches(&intent, &job.process, &proof, 3, 1));
        let mut process = job.process.clone();
        process.birth += 1;
        assert!(!job.matches(&job.intent, &process, &proof, 3, 1));
        process = job.process.clone();
        process.image.inode += 1;
        assert!(!job.matches(&job.intent, &process, &proof, 3, 1));
        assert!(!job.matches(&job.intent, &job.process, &proof, 4, 1));
        assert!(!job.matches(&job.intent, &job.process, &proof, 3, 2));
    }

    #[test]
    fn codex_rotation_index_retains_source_intent_across_candidate_changes() {
        let proof = crate::session_timeline::scoped_input_tests::scoped(&[Scope::Relaunch]).proven;
        let mut job = job(&proof);
        let mut env = Env::from_process().unwrap();
        env.home = Some("/fixture/home".into());
        env.state = "/fixture/state".into();
        let original = index(&env, &job).unwrap();
        // A destination is deliberately not in the key: a lost Ready response
        // must find its original receipt before another candidate can be chosen.
        *job.progress.lock().unwrap() = Progress::NotNeeded;
        assert_eq!(index(&env, &job).unwrap(), original);
        job.intent.wall = "00000000-0000-0000-0000-000000000003".into();
        assert_ne!(index(&env, &job).unwrap(), original);
        job.intent.wall = "00000000-0000-0000-0000-000000000002".into();
        job.process.birth += 1;
        assert_ne!(index(&env, &job).unwrap(), original);
    }
}
