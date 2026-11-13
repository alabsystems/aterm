// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native exec account preparation. Peer/claim proof and kernel launch facts
//! replace the TUI record requirement; account policy remains the shared pool's.
//! A selected proposal is not a commit, launcher capability or execution grant.

use super::*;
use aterm_agent::harness::{
    accounts,
    cli::Env,
    codex_exec_handoff::{ExecCommand, ExecSource},
    codex_renewal, codex_rotation,
};
use std::time::Instant;

#[path = "codex_exec_operation.rs"]
mod operation;
pub(super) use operation::Operation;
pub(super) use operation::{cancel_owner, pending, retain_claim};

const WAITING: &str = "OK codex-exec-handoff state=waiting\n";
const NOT_NEEDED: &str = "OK codex-exec-handoff state=not-needed\n";

/// How long an answer that the session's login stands at no wall a handoff leaves
/// ([`codex_rotation::NoHandoff::NoWallToLeave`]) answers its later asks. A headless run is one
/// root turn that keeps its request in flight, so its job here is the same one at every wall it
/// meets, and an answer that stood for good would be given at a later wall, the week's included,
/// with no read. The latest reset that answer waited out is a five-hour one
/// [`accounts::RESET_SOON_S`] away; by then it has passed and so has its grace
/// ([`accounts::RESET_GRACE_S`]), so a session still asking stands at a wall again, or still, and
/// its login is read again. A wait that is read as no wall again stands longer each time
/// ([`no_wall_stands`]). The clock is the monotonic one, which a sleeping host may hold still: a
/// sleep only lengthens the stand.
const NO_WALL_STANDS: Duration =
    Duration::from_secs((accounts::RESET_SOON_S + accounts::RESET_GRACE_S).unsigned_abs());

/// The longest time between two asks of one wait. A session asks every few seconds while it
/// stands at its wall, so one that asks again after longer than this had stopped asking: it ran,
/// and stands at another wall, whose wait takes up none of the stands the earlier one climbed to.
const STILL_ASKING: Duration = Duration::from_secs(60);

/// How long an answer of no wall to leave stands when it is the `reads`th such read in a row of
/// one wait: [`NO_WALL_STANDS`] for the first, and for one whose session has since asked anew,
/// then the rungs a wall that names no reset stands by ([`accounts::NO_RESET_WALL_S`]). The
/// first stand ends past a reset a quarter of an hour away. A login read as no wall again while
/// its session kept asking stands at something no such reset ends (a spent window that names no
/// reset, a limit the read does not show), so its reads thin out as the pool's do for a wall
/// with no reset, to one in six hours, where a flat stand would read it every
/// [`NO_WALL_STANDS`] for as long as the run asks.
fn no_wall_stands(reads: u32) -> Duration {
    match reads.checked_sub(1) {
        None | Some(0) => NO_WALL_STANDS,
        Some(again) => {
            Duration::from_secs(accounts::rung(&accounts::NO_RESET_WALL_S, again).unsigned_abs())
        }
    }
}

#[derive(Debug)]
pub(super) struct Preparation {
    source: ExecSource,
    intent: codex_renewal::Intent,
    proof: crate::self_claim::Proven,
    revision: u64,
    human_seq: u64,
    progress: Mutex<Progress>,
}

#[derive(Debug)]
enum Progress {
    /// A read is out. Before it, this many reads in a row of the wait it was started in said
    /// there is no wall to leave.
    Reading(u32),
    Retry(Instant),
    NotNeeded(NoneComing),
    Selected(Box<codex_rotation::Proposal<ExecCommand>>, Instant),
}

/// The answer that no handoff is coming, as its job keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NoneComing {
    /// Why none is.
    end: codex_rotation::NoHandoff,
    /// When its read said so.
    decided: Instant,
    /// How many reads in a row of the session's wait, this one included, said there is no wall
    /// to leave; none where the session has since stopped asking and asked again.
    reads: u32,
    /// The session's latest ask, or `decided` before one.
    asked: Instant,
}

impl NoneComing {
    /// Take an ask at `now`, and say whether this answer still answers it. No wall to leave is a
    /// reading of the login at one time, and stands [`no_wall_stands`]; a login that is no row
    /// of the pool, or that names another account than its row's, is nothing a wait changes, and
    /// stands while the job does.
    fn stands(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.asked) > STILL_ASKING {
            self.reads = 0;
        }
        self.asked = now;
        match self.end {
            codex_rotation::NoHandoff::NoWallToLeave => {
                now.saturating_duration_since(self.decided) < no_wall_stands(self.reads)
            }
            codex_rotation::NoHandoff::NoRowOfThePool
            | codex_rotation::NoHandoff::LoginNamesAnother => true,
        }
    }
}

impl Preparation {
    fn invalidate_selection(&self, receipt: &codex_renewal::Receipt) {
        if self.intent != receipt.intent {
            return;
        }
        let mut progress = self.progress.lock().unwrap_or_else(|p| p.into_inner());
        if matches!(&*progress, Progress::Selected(proposal, _)
            if proposal.successor().is_some_and(|next| receipt.matches_successor(&next)))
        {
            *progress = Progress::Retry(Instant::now());
        }
    }

    fn matches(
        &self,
        source: &ExecSource,
        intent: &codex_renewal::Intent,
        proof: &crate::self_claim::Proven,
        revision: u64,
        human_seq: u64,
    ) -> bool {
        self.source == *source
            && self.intent == *intent
            && self.proof == *proof
            && self.revision == revision
            && self.human_seq == human_seq
    }
}

/// What an ask at `now` is answered from its job's progress, and whether it starts a read of the
/// session's login, which [`ask`] does once the progress is unlocked.
fn answer(progress: &mut Progress, intent: &codex_renewal::Intent, now: Instant) -> (String, bool) {
    match progress {
        Progress::Reading(_) => (WAITING.into(), false),
        Progress::Retry(at) if now < *at => (WAITING.into(), false),
        Progress::Retry(_) => {
            *progress = Progress::Reading(0);
            (WAITING.into(), true)
        }
        Progress::NotNeeded(none) => {
            if none.stands(now) {
                return (NOT_NEEDED.into(), false);
            }
            *progress = Progress::Reading(none.reads);
            (WAITING.into(), true)
        }
        Progress::Selected(proposal, observed)
            if now.saturating_duration_since(*observed) < Duration::from_secs(60) =>
        {
            (
                format!(
                    "OK codex-exec-handoff state=selected thread={} turn={} source_account={} source_user={} target_account={} target_user={}\n",
                    intent.thread,
                    intent.wall,
                    proposal.source_identity.account,
                    proposal.source_identity.user,
                    proposal.destination_identity.account,
                    proposal.destination_identity.user
                ),
                false,
            )
        }
        Progress::Selected(_, _) => {
            *progress = Progress::Retry(now);
            (WAITING.into(), false)
        }
    }
}

/// Answer an ask at `now` from its job's progress and, where that answer starts a read of the
/// session's login, run `read`, once and with the progress unlocked: a read that cannot start
/// writes the progress itself.
fn ask(
    progress: &Mutex<Progress>,
    intent: &codex_renewal::Intent,
    now: Instant,
    read: impl FnOnce(),
) -> String {
    let (reply, starts) = answer(
        &mut progress.lock().unwrap_or_else(|p| p.into_inner()),
        intent,
        now,
    );
    if starts {
        read();
    }
    reply
}

/// The job's progress once the read `before` was waiting on came back at `now`; `current` says
/// the session, its claim and the person's input are still the ones the read was started for.
fn progress_after_observation(
    before: &Progress,
    result: Result<Option<codex_rotation::Proposal<ExecCommand>>, &'static str>,
    current: bool,
    now: Instant,
) -> Progress {
    let retry = Progress::Retry(now + Duration::from_secs(5));
    match result {
        Ok(Some(proposal)) if current => Progress::Selected(Box::new(proposal), now),
        // No handoff is coming to this session, and a wait would only spend its turn. The log
        // says which kind ended it; [`NoneComing::stands`] says for how long.
        Err(why) if current => match codex_rotation::prepare_refusal_stands(why) {
            Some(end) => {
                aterm_log::info!("codex exec handoff: none is coming: {}", end.as_str());
                let earlier = match before {
                    Progress::Reading(earlier) => *earlier,
                    _ => 0,
                };
                Progress::NotNeeded(NoneComing {
                    end,
                    decided: now,
                    reads: earlier.saturating_add(1),
                    asked: now,
                })
            }
            None => retry,
        },
        _ => retry,
    }
}

pub(crate) fn serve(ctx: &Arc<crate::SessionCtx>, master: i32, line: &str) -> String {
    if line.split_ascii_whitespace().next() != Some("prepare") {
        return operation::serve(ctx, master, line);
    }
    let Some(codex_renewal::Request::Prepare(intent)) =
        codex_renewal::Request::parse_rotation(line)
    else {
        return "ERR codex-exec-handoff unsupported request\n".into();
    };
    if crate::control::serving_principal() == crate::control::Principal::Harness
        || !ctx.native_policy.quota_rotation_enabled()
    {
        return DENIED.into();
    }
    let Some(pid) = crate::control::serving_peer_pid() else {
        return DENIED.into();
    };
    let Some(proof) =
        crate::control::prove_self_claim_for(ctx, &format!("codex-alab@{pid}"), master)
    else {
        return DENIED.into();
    };
    let Some(source) = ExecSource::capture(pid) else {
        return DENIED.into();
    };
    let env = match Env::from_process() {
        Ok(env) => env,
        // This process's working directory and state directory do not come back by themselves:
        // no handoff is coming, and a wait would only spend the session's turn.
        Err(why) => {
            aterm_log::warn!("codex exec handoff: none is coming: {why}");
            return NOT_NEEDED.into();
        }
    };
    if ctx.fabric.hold().is_some() || !renewal::human_grace_ready(ctx, &env) {
        return WAITING.into();
    }
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let authority = meta.supervisor_authority(crate::metrics::now_us());
    if authority.identity() != Some(&proof)
        || authority.connection() != crate::control::serving_connection()
        || authority
            .scopes()
            .is_some_and(|scopes| !scopes.contains(Scope::Relaunch))
    {
        return DENIED.into();
    }
    let revision = authority.revision();
    let human_seq = ctx.human_input.seq();
    let mut table = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let job = if let Some(job) = table
        .exec_prepare
        .as_ref()
        .filter(|job| job.matches(&source, &intent, &proof, revision, human_seq))
    {
        Arc::clone(job)
    } else {
        let job = Arc::new(Preparation {
            source,
            intent,
            proof,
            revision,
            human_seq,
            progress: Mutex::new(Progress::Retry(Instant::now())),
        });
        table.exec_prepare = Some(Arc::clone(&job));
        job
    };
    drop(table);
    drop(meta);
    let reader = Arc::clone(&job);
    ask(&job.progress, &job.intent, Instant::now(), move || {
        start(ctx, env, reader);
    })
}

fn start(ctx: &Arc<crate::SessionCtx>, env: Env, job: Arc<Preparation>) {
    let weak = Arc::downgrade(ctx);
    let roots = ctx.native_policy.quota_fixture_roots();
    let sid = ctx.self_id.as_str().to_owned();
    let worker = Arc::clone(&job);
    if std::thread::Builder::new()
        .name("codex-exec-quota-probe".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                renewal::observe_exec(&env, &sid, &worker.source, &worker.intent, roots)
            }))
            .unwrap_or(Err("probe-panicked"));
            let current = weak.upgrade().is_some_and(|ctx| {
                let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
                let authority = meta.supervisor_authority(crate::metrics::now_us());
                authority.identity() == Some(&worker.proof)
                    && authority.revision() == worker.revision
                    && ctx.fabric.hold().is_none()
                    && ctx.human_input.seq() == worker.human_seq
                    && worker.source.current()
                    && ctx
                        .fenced_writes
                        .native
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .exec_prepare
                        .as_ref()
                        .is_some_and(|stored| Arc::ptr_eq(stored, &worker))
            });
            let mut progress = worker.progress.lock().unwrap_or_else(|p| p.into_inner());
            *progress = progress_after_observation(&progress, result, current, Instant::now());
        })
        .is_err()
    {
        *job.progress.lock().unwrap_or_else(|p| p.into_inner()) =
            Progress::Retry(Instant::now() + Duration::from_secs(5));
    }
}

#[cfg(test)]
#[path = "codex_exec_handoff_control_tests.rs"]
mod tests;
