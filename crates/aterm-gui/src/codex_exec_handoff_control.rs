// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native exec account preparation. Peer/claim proof and kernel launch facts
//! replace the TUI record requirement; account policy remains the shared pool's.
//! A selected proposal is not a commit, launcher capability or execution grant.

use super::*;
use aterm_agent::harness::{
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
    Reading,
    Retry(Instant),
    NotNeeded(Instant),
    Selected(Box<codex_rotation::Proposal<ExecCommand>>, Instant),
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
    let Ok(env) = Env::from_process() else {
        return WAITING.into();
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
    let mut progress = job.progress.lock().unwrap_or_else(|p| p.into_inner());
    match &*progress {
        Progress::Reading => WAITING.into(),
        Progress::Retry(at) if Instant::now() < *at => WAITING.into(),
        Progress::Retry(_) => {
            *progress = Progress::Reading;
            drop(progress);
            start(ctx, env, job);
            WAITING.into()
        }
        Progress::NotNeeded(observed) if observed.elapsed() < Duration::from_secs(5) => {
            "OK codex-exec-handoff state=not-needed\n".into()
        }
        Progress::Selected(proposal, observed) if observed.elapsed() < Duration::from_secs(60) => {
            format!(
                "OK codex-exec-handoff state=selected thread={} turn={} source_account={} source_user={} target_account={} target_user={}\n",
                job.intent.thread,
                job.intent.wall,
                proposal.source_identity.account,
                proposal.source_identity.user,
                proposal.destination_identity.account,
                proposal.destination_identity.user
            )
        }
        Progress::Selected(_, _) | Progress::NotNeeded(_) => {
            *progress = Progress::Retry(Instant::now());
            WAITING.into()
        }
    }
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
            *worker.progress.lock().unwrap_or_else(|p| p.into_inner()) = match result {
                Ok(Some(proposal)) if current => {
                    Progress::Selected(Box::new(proposal), Instant::now())
                }
                Err("not-weekly-exhausted") if current => Progress::NotNeeded(Instant::now()),
                _ => Progress::Retry(Instant::now() + Duration::from_secs(5)),
            };
        })
        .is_err()
    {
        *job.progress.lock().unwrap_or_else(|p| p.into_inner()) =
            Progress::Retry(Instant::now() + Duration::from_secs(5));
    }
}
