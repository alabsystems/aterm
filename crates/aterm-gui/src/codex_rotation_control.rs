// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Provider reads produce observations on a worker. Only a fresh, proven host
//! request may publish them as a native restart receipt. The reads run under the
//! asking process's read pace, which its tab keeps for this control and the exec
//! handoff's alike ([`observe_paced`]), and to which every ask is noted ([`note_ask`]).
//! A read out for longer than one can take is taken for one that will not end
//! ([`READ_LAPSES`]).

use super::*;
use aterm_agent::harness::{accounts_pool, codex_rotation as pool, relaunch};
use std::collections::BTreeSet;
use std::time::Instant;

const WAITING: &str = "OK codex-rotation state=waiting\n";
const RETRY_MS: u64 = 5_000;
const OBSERVATION_MS: u64 = 60_000;
/// How long before its observation lapses a selection is no longer published. The session
/// commits a receipt in the step after it reads it, and [`commit_pool`] refuses a selection whose
/// observation is [`OBSERVATION_MS`] old: one published with less than this left could lapse
/// before that commit.
const COMMIT_MARGIN_MS: u64 = 10_000;
/// How long a usage-limit job's read may be out, and its observation hold the process's read
/// pace, before it is taken for one that will not end. An observation's provider reads end
/// within [`OBSERVATION_MS`]; what it does around them (the kernel's and the records' reads, each
/// lock among them waited on for a few seconds at most) takes far less than the half minute more.
/// Past it the job's next ask starts another read ([`answer`], and the exec handoff's alike),
/// which gets a new pace in place of the held one ([`pool::Paces::of`]), and what the read that
/// was taken to never end answers, should it end, changes nothing ([`observed`]). So no one
/// wedged read stops its process's reads until a person's key, a new wall turn or claim revision,
/// or an aterm restart replaces its job.
pub(in crate::native_act) const READ_LAPSES: Duration =
    Duration::from_millis(OBSERVATION_MS + 30_000);

/// A process as its tab keeps what it asks a usage-limit handoff for: its read pace, both
/// controls', and an exec handoff's preparation and operation (`exec_handoff::Table`). Its pid and
/// kernel birth: a same-PID execve keeps both, so the successor an exec move execs is still the
/// process that armed it and keeps its pace, and a pid the kernel gives a later process is
/// another key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::native_act) struct ProcessKey {
    pid: u32,
    birth: u64,
}

impl ProcessKey {
    pub(in crate::native_act) fn of(process: &Process) -> Self {
        Self {
            pid: process.pid,
            birth: process.birth,
        }
    }

    /// Whether the kernel shows this process gone: its pid absent or exited, or held by a
    /// process of another birth. A read that fails shows nothing, and the process is not gone.
    pub(in crate::native_act) fn gone(self) -> bool {
        let pid = i32::try_from(self.pid).unwrap_or(-1);
        aterm_uds::process::read_birth(pid).is_some_and(|birth| birth.0 != self.birth)
            || matches!(
                aterm_uds::process::process_presence(pid),
                aterm_uds::process::ProcessPresence::Absent
                    | aterm_uds::process::ProcessPresence::Exited
            )
    }
}

/// The read pace `ctx`'s tab keeps for the process `process` at `now`, or a new one in place of
/// one an observation has held for longer than [`READ_LAPSES`] ([`pool::Paces::of`]). Takes the
/// tab's table on the calling thread, never under a job's progress lock.
fn read_pace(ctx: &crate::SessionCtx, process: ProcessKey, now: Instant) -> Arc<pool::KeptPace> {
    ctx.fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .rotation_paces
        .of(process, now, READ_LAPSES)
}

/// The process `process` asks a usage-limit handoff at `now` (Unix seconds), noted to its read
/// pace before the ask is answered ([`pool::KeptPace::asks`]): every request of
/// `codex-rotation` (`prepare`, and `commit`, `status`, `consume` and `complete` of a receipt)
/// and of `codex-exec-handoff` (`prepare`, `arm`, and each verb of the operation it armed),
/// whatever it is answered, a gate's refusal, a read that is out, a selection published again
/// and a published receipt's or an armed operation's answer among them. The paces of the
/// processes the kernel shows gone are forgotten first, by the rule the exec handoff's table
/// forgets its processes by ([`ProcessKey::gone`]), read with the table unlocked: a gone process
/// does not come back.
pub(in crate::native_act) fn note_ask(ctx: &crate::SessionCtx, process: ProcessKey, now: i64) {
    let kept = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .rotation_paces
        .processes()
        .collect::<Vec<_>>();
    let gone = kept
        .into_iter()
        .filter(|key| key.gone())
        .collect::<BTreeSet<_>>();
    if !gone.is_empty() {
        ctx.fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .rotation_paces
            .forget(&gone);
    }
    read_pace(ctx, process, Instant::now()).asks(now);
}

/// Run `observe` on a new thread named `name`, under the read pace `ctx`'s tab keeps for the
/// process `process`, then `done` with what it answered and the pace released. The pace is
/// fetched on the calling thread ([`read_pace`]), so the tab's table is never taken under a job's
/// progress lock. One observation holds the pace at a time ([`pool::KeptPace::observe`]): one
/// that finds it held reads nothing and answers `Ok(None)`, so its job is asked again, and only
/// one that has held it for longer than [`READ_LAPSES`] is given up on, its process paced anew,
/// so that at most two observations of a process are out at once. A panicking observation
/// answers `probe-panicked`, and the pace it held is the next observation's as it left it. The
/// one way a usage-limit control's worker reaches a pace.
pub(in crate::native_act) fn observe_paced<T>(
    ctx: &crate::SessionCtx,
    name: &str,
    process: ProcessKey,
    observe: impl FnOnce(&mut pool::ReadPace) -> Result<Option<T>, &'static str> + Send + 'static,
    done: impl FnOnce(Result<Option<T>, &'static str>) + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    let kept = read_pace(ctx, process, Instant::now());
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let result = kept
                .observe(Instant::now(), |pace| {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observe(pace)))
                        .unwrap_or(Err("probe-panicked"))
                })
                .unwrap_or(Ok(None));
            done(result);
        })
}

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
    /// A read is out, started at this time.
    Reading(Instant),
    Retry(Instant),
    NotNeeded,
    Selected(Box<Selection>),
}

#[derive(Debug)]
struct Selection {
    proposal: pool::Proposal,
    observed: Instant,
    receipt: Option<Receipt>,
    /// When a publication that failed may be tried again; `None` where none has failed since
    /// the selection was made or last published.
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

fn index(job: &Preparation) -> Option<renewal::IndexKey> {
    renewal::IndexKey::moved(&job.intent, job.process.pid, job.process.birth, &job.start).ok()
}

/// The live reader against the isolated loopback provider a Rust fixture pins the roots of: its
/// HTTPS usage server, and the token endpoint of a login whose issuer it serves too.
#[cfg(debug_assertions)]
pub(super) struct FixtureUsage<'a> {
    live: pool::LiveUsage<'a>,
    roots: Vec<Vec<u8>>,
}

#[cfg(debug_assertions)]
impl<'a> FixtureUsage<'a> {
    pub(super) const fn new(live: pool::LiveUsage<'a>, roots: Vec<Vec<u8>>) -> Self {
        Self { live, roots }
    }
}

#[cfg(debug_assertions)]
impl pool::UsageReader for FixtureUsage<'_> {
    fn read(
        &self,
        home: &Path,
        expected_email: Option<&str>,
        now: i64,
        wait: Duration,
        use_token: aterm_agent::harness::provider_read::Use,
    ) -> aterm_agent::harness::codex_app_login::IdentifiedRead {
        aterm_agent::harness::codex_app_login::read_identified_login_with_fixture_roots(
            home,
            self.live.config_home,
            self.live.user_home,
            wait,
            now,
            expected_email,
            self.roots.clone(),
            use_token,
        )
    }
}

fn observe(
    env: &Env,
    resume: &self_claimed::Resume,
    job: &Preparation,
    fixture_roots: Option<Vec<Vec<u8>>>,
    pace: &mut pool::ReadPace,
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
        seen: &aterm_agent::harness::observe::Observation::Unread,
        sample: None,
        point: pool::wall_asked(env.now),
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
            &job.intent,
            &FixtureUsage { live, roots },
            Duration::from_millis(OBSERVATION_MS),
            pace,
        );
    }
    #[cfg(not(debug_assertions))]
    let _ = fixture_roots;
    pool::prepare(
        &seat,
        &session,
        &job.intent,
        &live,
        Duration::from_millis(OBSERVATION_MS),
        pace,
    )
}

fn progress_after_observation(
    result: Result<Option<pool::Proposal>, &'static str>,
    current: bool,
    now: Instant,
) -> Progress {
    match result {
        Ok(Some(proposal)) if current => Progress::Selected(Box::new(Selection {
            proposal,
            observed: now,
            receipt: None,
            publish_at: None,
        })),
        Err(why) if current && pool::prepare_refusal_stands(why).is_some() => {
            if let Some(end) = pool::prepare_refusal_stands(why) {
                aterm_log::info!("codex rotation: none is coming: {}", end.as_str());
            }
            Progress::NotNeeded
        }
        _ => Progress::Retry(now + Duration::from_millis(RETRY_MS)),
    }
}

/// The job's `progress` once the read it started at `since` answered `result` at `now`, where
/// `current` says the job is still its session's: written only while that read is still the one
/// out. A read [`answer`] took for one that will not end ([`READ_LAPSES`]) and started another in
/// place of changes nothing should it end after all.
fn observed(
    progress: &mut Progress,
    since: Instant,
    result: Result<Option<pool::Proposal>, &'static str>,
    current: bool,
    now: Instant,
) {
    if matches!(progress, Progress::Reading(at) if *at == since) {
        *progress = progress_after_observation(result, current, now);
    }
}

/// Start `job`'s read, the one its ask started at `since`.
fn start(
    ctx: &Arc<crate::SessionCtx>,
    env: &Env,
    resume: &self_claimed::Resume,
    job: &Arc<Preparation>,
    since: Instant,
) {
    let weak = Arc::downgrade(ctx);
    let fixture_roots = ctx.native_policy.quota_fixture_roots();
    let env = env.clone();
    let resume = resume.clone();
    let reader = Arc::clone(job);
    let worker = Arc::clone(job);
    if observe_paced(
        ctx,
        "codex-quota-probe",
        ProcessKey::of(&job.process),
        move |pace| observe(&env, &resume, &reader, fixture_roots, pace),
        move |result| {
            let current = weak.upgrade().is_some_and(|ctx| worker.current(&ctx));
            observed(
                &mut worker.progress.lock().unwrap_or_else(|p| p.into_inner()),
                since,
                result,
                current,
                Instant::now(),
            );
        },
    )
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
                if let Ok(Some(retired)) = retire_stale_ready(ctx, env, proof, current, &receipt) {
                    return retired.reply();
                }
                return acknowledged_receipt_reply(env, &receipt);
            }
        }
        Err(_) | Ok(None) => return WAITING.into(),
        Ok(Some(None)) => {}
    }
    ask(
        &job.progress,
        Instant::now(),
        |selected| publish(ctx, env, proof, resume, &job, selected, &index),
        |since| start(ctx, env, resume, &job, since),
    )
}

/// What an ask at `now` is answered from its job's progress, and whether it starts a read of the
/// accounts' usage, which [`ask`] does once the progress is unlocked. A read that is out answers
/// `waiting` and starts none, until it has been out for longer than [`READ_LAPSES`]: it is then
/// taken for one that will not end, and the ask starts another in its place. A selection is
/// published by `publish`. One whose publication fails is kept, and the first ask [`RETRY_MS`]
/// later publishes it again: a busy receipt writer, a taken sign-in turn or a changed claim is
/// nothing a new read of the provider answers, so a failed publication starts none, and an ask
/// before then publishes nothing. A selection within [`COMMIT_MARGIN_MS`] of its observation's
/// end is not published: it is dropped, and the next ask reads again. A selection the records
/// refute fails its publication like any other, and so waits until then for its next read.
fn answer(
    progress: &mut Progress,
    now: Instant,
    publish: impl FnOnce(&Selection) -> Result<Receipt, &'static str>,
) -> (String, bool) {
    match progress {
        Progress::Reading(since) if now.saturating_duration_since(*since) <= READ_LAPSES => {
            (WAITING.into(), false)
        }
        Progress::Reading(_) => {
            aterm_log::warn!(
                "codex rotation: a read has been out for longer than {READ_LAPSES:?}; it is \
                 taken for one that will not end, and another starts"
            );
            *progress = Progress::Reading(now);
            (WAITING.into(), true)
        }
        Progress::Retry(at) if now < *at => (WAITING.into(), false),
        Progress::Retry(_) => {
            *progress = Progress::Reading(now);
            (WAITING.into(), true)
        }
        Progress::NotNeeded => ("OK codex-rotation state=not-needed\n".into(), false),
        Progress::Selected(selected) => {
            if now.saturating_duration_since(selected.observed)
                >= Duration::from_millis(OBSERVATION_MS - COMMIT_MARGIN_MS)
            {
                *progress = Progress::Retry(now);
                return (WAITING.into(), false);
            }
            if selected.publish_at.is_some_and(|at| now < at) {
                return (WAITING.into(), false);
            }
            match publish(selected) {
                Ok(receipt) => {
                    selected.publish_at = None;
                    selected.receipt = Some(receipt.clone());
                    (receipt.reply(), false)
                }
                Err(_) => {
                    selected.publish_at = Some(now + Duration::from_millis(RETRY_MS));
                    (WAITING.into(), false)
                }
            }
        }
    }
}

/// Answer an ask at `now` from its job's progress and, where that answer starts a read of the
/// accounts' usage, run `read`, once and with the progress unlocked, with `now`, the time the
/// read is out from, by which its answer is written ([`observed`]): a read that cannot start
/// writes the progress itself.
fn ask(
    progress: &Mutex<Progress>,
    now: Instant,
    publish: impl FnOnce(&Selection) -> Result<Receipt, &'static str>,
    read: impl FnOnce(Instant),
) -> String {
    let (reply, starts) = answer(
        &mut progress.lock().unwrap_or_else(|p| p.into_inner()),
        now,
        publish,
    );
    if starts {
        read(now);
    }
    reply
}

fn publish(
    ctx: &crate::SessionCtx,
    env: &Env,
    proof: &crate::self_claim::Proven,
    resume: &self_claimed::Resume,
    job: &Preparation,
    selected: &Selection,
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

/// `codex-rotation complete id=<id> turn=<turn>`: the report, from the bound successor of the
/// consumed usage-wall receipt `id`, that its resumed turn `turn` (the walled turn's own id, which
/// the resumed turn reuses) was served on the destination. The host proves that destination for
/// the pool ([`renewal::record_completed_rotation`]), so each session walled behind its canary
/// moves onto it at its next ask, where it would otherwise wait out the canary's hold
/// (`accounts::CANARY_HOLD_S`), which stays the ceiling for a canary that never reports.
///
/// The report states a fact of the pool and grants this process nothing: no fabric hold, newer
/// input of a person or other host epoch refuses it, as none refuses a consumed receipt's terminal
/// observation (`status`). The shared lane has already judged it as it judges each of its verbs
/// (`serve_claimed`): a proven claim whose record and resume line read, and a receipt on record
/// under `id` of a pool move's purpose, not a lost login's (`wrong recovery purpose`, before any
/// check here). It is then taken only as consume's lane left it, checked in this order: quota
/// rotation on here (`denied`); a receipt on record (`unknown receipt`) that this
/// interactive lane issued, not an exec handoff's ([`interactive`], `wrong recovery purpose`);
/// this session's (`wrong session`); consumed by this very turn, the walled one (`receipt
/// changed`); and the claimed process its bound successor, by pid and birth, with the
/// destination's login file as its `CODEX_AUTH_PATH` and the receipt's thread as the conversation
/// it resumed (`wrong process`). The source process never ran on the destination and cannot
/// report it. A busy receipt writer, sign-in turn or auth lock answers `state=waiting`; any other
/// refusal of the pool's records (the selection, policy or pin changed, the destination no longer
/// runs, or a login, the source's included, names another identity or is held since the wall)
/// answers `selection changed`. A refused report records nothing.
pub(super) fn complete(
    ctx: &crate::SessionCtx,
    env: &Env,
    proof: &crate::self_claim::Proven,
    copy: &self_claimed::Copy,
    id: &str,
    turn: &str,
) -> String {
    if !ctx.native_policy.quota_rotation_enabled() {
        return "ERR codex-rotation denied\n".into();
    }
    let Some(receipt) = renewal::read_receipt(env, id) else {
        return "ERR codex-rotation unknown receipt\n".into();
    };
    if !interactive(&receipt) {
        return "ERR codex-rotation wrong recovery purpose\n".into();
    }
    if receipt.sid != ctx.self_id.as_str() {
        return "ERR codex-rotation wrong session\n".into();
    }
    if receipt.phase != Phase::Consumed
        || receipt.accepted_turn.as_deref() != Some(turn)
        || turn != receipt.intent.wall
    {
        return "ERR codex-rotation receipt changed\n".into();
    }
    let Ok(resume) = &copy.resume else {
        return WAITING.into();
    };
    let successor = Process::observe(proof.pid).is_some_and(|process| {
        Some(process.pid) == receipt.successor_pid && Some(process.birth) == receipt.successor_birth
    });
    if !successor
        || resume.codex_auth_path.as_deref().map(Path::new) != Some(receipt.new_auth.as_path())
        || resume.session != receipt.intent.thread
    {
        return "ERR codex-rotation wrong process\n".into();
    }
    match renewal::record_completed_rotation(env, &receipt) {
        Ok(()) => receipt.completed_reply(),
        Err(renewal::Unrecorded::Busy) => WAITING.into(),
        Err(renewal::Unrecorded::Refused(_)) => "ERR codex-rotation selection changed\n".into(),
    }
}

/// Whether `receipt` is this interactive lane's: its sealed recipe is the native restart its
/// commit runs. An exec handoff's receipt shares the store and the purpose, but binds an exec
/// operation's data, and that operation reports its own completion (`codex-exec-handoff
/// complete`) under its own gates.
fn interactive(receipt: &Receipt) -> bool {
    matches!(
        aterm_agent::harness::native_act::Request::parse(&receipt.native_prepare),
        Some(aterm_agent::harness::native_act::Request::Prepare(_))
    )
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
    if !receipt.intent.purpose.moves_account()
        || receipt.phase != Phase::Ready
        || !receipt.has_no_execution_evidence()
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
        writer.retire_pool_record(&retired)?;
        Ok(Some(retired))
    })
    .map(Option::flatten)
}

/// The exec source uses the same provider reader and pool as TUI recovery, under the asking
/// process's read pace `pace` ([`observe_paced`]).
#[cfg(unix)]
pub(in crate::native_act) fn observe_exec(
    env: &Env,
    sid: &str,
    source: &aterm_agent::harness::codex_exec_handoff::ExecSource,
    intent: &Intent,
    fixture_roots: Option<Vec<Vec<u8>>>,
    pace: &mut pool::ReadPace,
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
            intent,
            &FixtureUsage { live, roots },
            Duration::from_millis(OBSERVATION_MS),
            pace,
        );
    }
    #[cfg(not(debug_assertions))]
    let _ = fixture_roots;
    source.prepare(
        &seat,
        sid,
        intent,
        &live,
        Duration::from_millis(OBSERVATION_MS),
        pace,
    )
}

#[cfg(test)]
#[path = "codex_rotation_control_tests.rs"]
mod tests;
