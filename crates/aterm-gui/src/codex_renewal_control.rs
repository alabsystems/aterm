// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The only bridge from a proven Codex auth wall to a PrivateRestart plan.
//! Wire input names a wall and identity digests, never a command or credential path.
use super::*;
use aterm_agent::harness::{accounts, cli::Env, codex_renewal as renewal, self_claimed};
use renewal::{Intent, Phase, Receipt};
use std::path::{Path, PathBuf};

#[path = "codex_renewal_intent.rs"]
mod intent;
use intent::{IntentFence, RecordWitness};
#[path = "codex_renewal_continue.rs"]
mod continuation;
pub(super) use continuation::{closed_predecessor, stage as stage_continuation};
#[path = "codex_renewal_service.rs"]
mod service;
pub(super) use service::Service;
#[path = "codex_rotation_control.rs"]
mod rotation;
pub(super) use rotation::Preparation as RotationPreparation;
#[cfg(unix)]
pub(super) use rotation::observe_exec;
#[cfg(all(test, unix))]
pub(super) use service::append_child as fixture_append_child;
#[cfg(all(test, unix))]
pub(super) use service::observe_terminal_by as fixture_observe_terminal;
#[cfg(all(test, unix))]
pub(super) use service::retire_unstarted as fixture_retire_unstarted;
#[cfg(all(test, unix))]
pub(super) use service::settle_terminal_native as fixture_settle_terminal_native;
pub(crate) use service::{cancel_owner, pending};

#[derive(Clone, Debug)]
pub(super) struct Seal {
    receipt: Receipt,
    prepare: Prepare,
    proof: crate::self_claim::Proven,
    ctx: std::sync::Weak<crate::SessionCtx>,
    env: Env,
    intent: Arc<IntentFence>,
    record: Option<RecordWitness>,
    continuation: Option<Arc<ClosedRestart>>,
    fresh_snapshot: Option<SupervisorAuthority>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(u64::MAX)
}

fn same_native_host(receipt: &Receipt) -> bool {
    matches!(aterm_agent::harness::native_act::Request::parse(&receipt.native_prepare),
        Some(aterm_agent::harness::native_act::Request::Prepare(prepare))
            if Some(prepare.host_epoch.as_str()) == host_epoch())
}

fn receipt_reply(receipt: &Receipt) -> String {
    if !matches!(receipt.phase, Phase::Consumed | Phase::Retired) && !same_native_host(receipt) {
        let mut held = receipt.clone();
        held.phase = Phase::Committing;
        held.reply()
    } else {
        receipt.reply()
    }
}

fn human_grace_elapsed(ctx: &crate::SessionCtx, seconds: u32, now: u64) -> bool {
    ctx.human_input
        .ms_since(now)
        .is_none_or(|age| age >= u64::from(seconds) * 1000)
}

pub(super) fn human_grace_ready(ctx: &crate::SessionCtx, env: &Env) -> bool {
    let seconds = if let Some(policy) = ctx.native_policy.config() {
        policy.human_grace_s
    } else {
        let (policy, warnings) =
            aterm_agent::supervise::SupervisorConfig::from_path(env.config.as_deref());
        if !warnings.is_empty() {
            return false;
        }
        policy.human_grace_s
    };
    human_grace_elapsed(ctx, seconds, crate::metrics::now_us())
}

impl Seal {
    pub(super) fn receipt_id(&self) -> &str {
        &self.receipt.id
    }
    pub(super) fn admits(&self, snapshot: &SupervisorAuthority, prepare: &Prepare) -> bool {
        let authority = if let Some(fresh) = &self.fresh_snapshot {
            snapshot == fresh
                && self.continuation.is_some()
                && snapshot
                    .identity()
                    .is_none_or(|identity| identity == &self.proof && !self.proof.alive())
        } else {
            snapshot.identity() == Some(&self.proof)
                && snapshot.revision() == self.receipt.claim_revision
        };
        authority
            && snapshot
                .scopes()
                .is_none_or(|scopes| scopes.contains(Scope::Relaunch))
            && prepare == &self.prepare
            && self.current()
    }

    fn environment(&self) -> Env {
        let mut env = self.env.clone();
        env.now = i64::try_from(now_ms() / 1000).unwrap_or(i64::MAX);
        env
    }

    pub(super) fn bound_to_weak(&self, ctx: &crate::SessionCtx) -> bool {
        self.ctx
            .upgrade()
            .is_some_and(|original| std::ptr::eq(original.as_ref(), ctx))
    }

    pub(super) fn bound_to(&self, ctx: &Arc<crate::SessionCtx>) -> bool {
        self.ctx
            .upgrade()
            .is_some_and(|original| Arc::ptr_eq(&original, ctx))
    }

    pub(super) fn intent_active(&self) -> bool {
        self.ctx
            .upgrade()
            .is_some_and(|ctx| self.intent.current(&ctx))
    }

    pub(super) fn durably_cancelled(&self, ctx: &crate::SessionCtx) -> bool {
        self.bound_to_weak(ctx) && self.intent.durably_cancelled()
    }

    pub(super) fn current(&self) -> bool {
        aterm_agent::harness::native_act::monotonic_us()
            .is_some_and(|now| now < self.prepare.deadline_us)
            && self.ctx.upgrade().is_some_and(|ctx| {
                self.prepare.expected_human_seq == Some(ctx.human_input.seq())
                    && ctx.fabric.hold().is_none()
                    && self.intent.current(&ctx)
            })
    }

    pub(super) fn continuation(&self) -> Option<&Arc<ClosedRestart>> {
        self.continuation.as_ref()
    }

    pub(super) fn did_signal(&self) {
        self.intent.did_signal();
    }

    /// The owner hook invokes this at the Meta/native cancellation boundary.
    /// Durable publication is separate and can never clear the in-memory fence.
    pub(super) fn cancel(&self, kind: renewal::CancellationKind) {
        self.intent.cancel(kind);
    }

    fn active_receipt(&self, env: &Env) -> Option<Receipt> {
        let receipt = renewal::read_receipt(env, &self.receipt.id)?;
        if receipt.revoked
            || !receipt.same_intent(&self.receipt)
            || receipt.accepted_turn.is_some()
            || receipt.phase != Phase::Committing
        {
            return None;
        }
        if self.continuation.is_some() {
            let attempt = renewal::read_active_attempt(env, &receipt).ok()??;
            if attempt.reference.id != self.prepare.id
                || aterm_agent::harness::native_act::Request::parse(&attempt.native_prepare)
                    != Some(aterm_agent::harness::native_act::Request::prepare(
                        self.prepare.clone(),
                    ))
            {
                return None;
            }
        } else if receipt.current_attempt.is_some()
            || receipt.native_prepare != self.receipt.native_prepare
        {
            return None;
        }
        Some(receipt)
    }

    /// A recorded Signal may have ended the source process. Subsequent
    /// mutations still need today's hold and account policy, without asking
    /// that intentionally ended process to be alive again.
    pub(super) fn continuing(&self, phase: ImagePhase) -> bool {
        if !self.current()
            || self
                .ctx
                .upgrade()
                .is_none_or(|ctx| ctx.fabric.hold().is_some())
        {
            return false;
        }
        let env = self.environment();
        if self
            .ctx
            .upgrade()
            .is_none_or(|ctx| !human_grace_ready(&ctx, &env))
        {
            return false;
        }
        let Some(receipt) = self.active_receipt(&env) else {
            return false;
        };
        if self.continuation.is_some()
            && !self
                .record
                .as_ref()
                .is_some_and(|record| record.current(&receipt))
        {
            return false;
        }
        account_phase_allowed(&env, &receipt, phase)
    }

    pub(super) fn preflight(&self) -> bool {
        if self.continuation.is_some() {
            return self.continuing(ImagePhase::Forward);
        }
        if !self.current()
            || !self.proof.alive()
            || self
                .ctx
                .upgrade()
                .is_none_or(|ctx| ctx.fabric.hold().is_some())
        {
            return false;
        }
        let env = self.environment();
        if self
            .ctx
            .upgrade()
            .is_none_or(|ctx| !human_grace_ready(&ctx, &env))
        {
            return false;
        }
        let Some(receipt) = renewal::read_receipt(&env, &self.receipt.id) else {
            return false;
        };
        if !matches!(receipt.phase, Phase::Ready | Phase::Committing)
            || receipt.old_pid != self.proof.pid
            || receipt.native_prepare != self.receipt.native_prepare
        {
            return false;
        }
        let Some(copy) = self_claimed::read_copy(self.proof.pid, &self.proof.start, None) else {
            return false;
        };
        let Some(record) = record(&copy) else {
            return false;
        };
        if !receipt.intent.matches_record(&record, Some(&receipt.id)) {
            return false;
        }
        if !renewal::history(&copy.home, &receipt.intent).is_some_and(|history| {
            receipt.intent.matches_history(&history)
                && receipt.intent.marker(&record)["starts"].as_u64() == Some(history.starts)
        }) {
            return false;
        }

        renewal::with_selected(&env, &receipt, |next| {
            if receipt.matches_successor(next) {
                Ok(())
            } else {
                Err("changed")
            }
        })
        .is_ok_and(|result| result.is_some())
    }
}

fn record(copy: &self_claimed::Copy) -> Option<aterm_json::Value> {
    renewal::native_record(copy)
}

fn current_claim(
    ctx: &crate::SessionCtx,
    master: i32,
) -> Option<(crate::self_claim::Proven, self_claimed::Copy)> {
    if crate::control::serving_principal() == crate::control::Principal::Harness {
        return None;
    }
    let pid = crate::control::serving_peer_pid()?;
    let holder = format!("codex-alab@{pid}");
    let proof = crate::control::prove_self_claim_for(ctx, &holder, master)?;
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let authority = meta.supervisor_authority(crate::metrics::now_us());
    if authority
        .scopes()
        .is_some_and(|scopes| !scopes.contains(Scope::Relaunch))
        || authority.identity() != Some(&proof)
        || authority.connection() != crate::control::serving_connection()
    {
        return None;
    }
    drop(meta);
    let copy = self_claimed::read_copy(pid, &proof.start, None)?;
    copy.resume.as_ref().ok()?;
    Some((proof, copy))
}

fn opts(env: &Env, sid: &str) -> Option<aterm_agent::harness::upgrade_drive::Opts> {
    let mut opts = crate::harness_host::window_drive_opts(
        env.home.clone()?,
        env.state.clone(),
        crate::proxy::self_sock_path(),
    );
    opts.only_sid = Some(sid.into());
    Some(opts)
}

fn index_path(
    env: &Env,
    next: &renewal::Successor,
    proof: &crate::self_claim::Proven,
    intent: &Intent,
) -> Option<PathBuf> {
    let key = format!(
        "{}:{}:{}:{}",
        proof.pid, proof.start, intent.wall, next.generation
    );
    let hash = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(key.as_bytes()));
    Some(
        env.accounts_store_for(accounts::Cli::Codex)?
            .dir
            .join("renewal-follow")
            .join(format!("intent-{hash}")),
    )
}

fn make_prepare(
    ctx: &crate::SessionCtx,
    resume: &self_claimed::Resume,
    process: Process,
    next: &renewal::Successor,
    id: &str,
    revision: u64,
) -> Option<Prepare> {
    let shell = Process::observe(process.parent)?;
    let shell_path = std::str::from_utf8(&shell.image.path).ok()?;
    let dialect = aterm_agent::harness::upgrade::Dialect::from_exe_name(
        std::path::Path::new(shell_path).file_name()?.to_str()?,
    )?;
    let program = std::str::from_utf8(&process.image.path).ok()?.to_owned();
    let mut env = Vec::new();
    if let Some(home) = &resume.codex_home {
        env.push(("CODEX_HOME".into(), home.clone()));
    }
    if let Some(path) = &resume.codex_auth_path {
        env.push(("CODEX_AUTH_PATH".into(), path.clone()));
    }
    let rollback = RestartCommand {
        account_intent: None,
        image: process.image.clone(),
        program: program.clone(),
        argv: resume.argv.get(1..)?.to_vec(),
        env: env.clone(),
        unset: None,
        cwd: resume.cwd.clone(),
        heal: None,
        dialect,
    };
    env.retain(|(name, _)| name != "CODEX_AUTH_PATH");
    env.push(("CODEX_AUTH_PATH".into(), next.new_auth.to_str()?.into()));
    let forward = RestartCommand {
        account_intent: None,
        env,
        ..rollback.clone()
    };
    Some(Prepare {
        id: id.into(),
        host_epoch: host_epoch()?.into(),
        holder: crate::harness_host::holder(),
        revision,
        expected_human_seq: Some(ctx.human_input.seq()),
        deadline_us: aterm_agent::harness::native_act::monotonic_us()?.checked_add(120_000_000)?,
        duty: Scope::Relaunch,
        plan: NativePlan::Restart {
            cold: None,
            process,
            observation: None,
            shell,
            launcher: Image::at(&std::env::current_exe().ok()?)?,
            socket: crate::proxy::self_sock_path()?,
            forward: Box::new(forward),
            rollback: Some(Box::new(rollback)),
        },
    })
}

pub(crate) fn serve(ctx: &Arc<crate::SessionCtx>, master: i32, line: &str) -> String {
    let result = serve_inner(ctx, master, line, renewal::Purpose::Login);
    #[cfg(all(unix, any(test, debug_assertions)))]
    {
        let verb = match renewal::Request::parse(line) {
            Some(renewal::Request::Prepare(_)) => "prepare",
            Some(renewal::Request::Commit(_)) => "commit",
            Some(renewal::Request::Status(_)) => "status",
            Some(renewal::Request::Consume { .. }) => "consume",
            None => "invalid",
        };
        let outcome = [
            "waiting",
            "ready",
            "committing",
            "committed",
            "consumed",
            "retired",
        ]
        .into_iter()
        .find(|state| result.starts_with(&format!("OK codex-renewal state={state}")))
        .unwrap_or(if result.starts_with("ERR ") {
            "error"
        } else {
            "unknown"
        });
        ctx.native_policy
            .renewal_control_observed(&aterm_json::json!({"kind":"renewal",
            "verb":verb,"outcome":outcome,"humanSeq":ctx.human_input.seq(),
            "monotonicUs":aterm_agent::harness::native_act::monotonic_us()}));
    }
    result
}

pub(crate) fn serve_rotation(ctx: &Arc<crate::SessionCtx>, master: i32, line: &str) -> String {
    let result = serve_inner(ctx, master, line, renewal::Purpose::WeeklyQuota);
    #[cfg(all(unix, any(test, debug_assertions)))]
    ctx.native_policy
        .renewal_control_observed(&aterm_json::json!({
            "kind":"rotation", "verb":line.split_ascii_whitespace().next(),
            "reply":result.trim(), "humanSeq":ctx.human_input.seq(),
            "monotonicUs":aterm_agent::harness::native_act::monotonic_us()
        }));
    if let Some(rest) = result.strip_prefix("OK codex-renewal ") {
        format!("OK codex-rotation {rest}")
    } else if let Some(rest) = result.strip_prefix("ERR codex-renewal ") {
        format!("ERR codex-rotation {rest}")
    } else {
        result
    }
}

fn serve_inner(
    ctx: &Arc<crate::SessionCtx>,
    master: i32,
    line: &str,
    purpose: renewal::Purpose,
) -> String {
    let request = match purpose {
        renewal::Purpose::Login => renewal::Request::parse(line),
        renewal::Purpose::WeeklyQuota => renewal::Request::parse_rotation(line),
    };
    let Some(request) = request else {
        return "ERR codex-renewal invalid request\n".into();
    };
    let Some((proof, copy)) = current_claim(ctx, master) else {
        return "ERR codex-renewal denied\n".into();
    };
    let Ok(env) = Env::from_process() else {
        return "OK codex-renewal state=waiting\n".into();
    };
    let Some(current) = record(&copy) else {
        return "OK codex-renewal state=waiting\n".into();
    };
    let Some(resume) = copy.resume.as_ref().ok() else {
        return "OK codex-renewal state=waiting\n".into();
    };
    if let renewal::Request::Commit(id)
    | renewal::Request::Status(id)
    | renewal::Request::Consume { id, .. } = &request
        && renewal::read_receipt(&env, id).is_some_and(|receipt| receipt.intent.purpose != purpose)
    {
        return "ERR codex-renewal wrong recovery purpose\n".into();
    }
    match request {
        renewal::Request::Prepare(intent) => {
            if ctx.fabric.hold().is_some() || !human_grace_ready(ctx, &env) {
                return "OK codex-renewal state=waiting\n".into();
            }
            if !intent.matches_record(&current, None) {
                return "ERR codex-renewal wall changed\n".into();
            }
            if !renewal::history(&copy.home, &intent).is_some_and(|history| {
                intent.matches_history(&history)
                    && intent.marker(&current)["starts"].as_u64() == Some(history.starts)
            }) {
                return "ERR codex-renewal native wall changed\n".into();
            }

            if purpose == renewal::Purpose::WeeklyQuota {
                return rotation::prepare(ctx, &env, &proof, &copy, &current, intent);
            }

            let Some(process) = Process::observe(proof.pid) else {
                return DENIED.into();
            };
            let authority = ctx
                .meta
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .supervisor_authority(crate::metrics::now_us());
            let old = resume
                .codex_auth_path
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(|| copy.home.join("auth.json"));
            let result = renewal::with_successor(&env, &old, &intent.identity, |next| {
                let index = index_path(&env, next, &proof, &intent).ok_or("index")?;
                if let Some(id) = renewal::read_index(&index)? {
                    let mut receipt = renewal::read_receipt(&env, &id).ok_or("indexed-receipt")?;
                    let same_source = receipt.sid == ctx.self_id.as_str()
                        && receipt.old_pid == proof.pid
                        && receipt.old_birth == process.birth
                        && receipt.intent == intent
                        && receipt.matches_successor(next);
                    let stale_ready = same_native_host(&receipt)
                        && same_source
                        && ready_native_no_go(ctx, &receipt.id)
                        && receipt.retire_stale_ready(
                            authority.revision(),
                            ctx.human_input.seq(),
                            now_ms(),
                        );
                    if stale_ready {
                        renewal::write_receipt(&env, &receipt)?;
                        return Ok(receipt);
                    }
                    // A wall-clock deadline never closes an already claimed
                    // native operation. Its coordinator reconciles the exact
                    // request and retires it only against a positive outcome.
                    if !receipt.may_replace() {
                        return Ok(receipt);
                    }
                }
                let id = aterm_agent::harness::native_act::operation_id().map_err(|_| "entropy")?;
                let prepare = make_prepare(
                    ctx,
                    resume,
                    process.clone(),
                    next,
                    &id,
                    authority.revision(),
                )
                .ok_or("plan")?;
                let receipt = Receipt {
                    destination: None,
                    id,
                    intent: intent.clone(),
                    sid: ctx.self_id.as_str().into(),
                    old_pid: proof.pid,
                    old_birth: process.birth,
                    claim_revision: authority.revision(),
                    human_seq: ctx.human_input.seq(),
                    old_auth: next.old_auth.clone(),
                    new_auth: next.new_auth.clone(),
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
                renewal::write_receipt(&env, &receipt)?;
                aterm_agent::harness::upgrade_models::write_atomic_mode(
                    &index,
                    &receipt.id,
                    Some(0o600),
                )
                .map_err(|_| "index")?;
                Ok(receipt)
            });
            match result {
                Ok(Some(receipt)) => receipt_reply(&receipt),
                _ => "OK codex-renewal state=waiting\n".into(),
            }
        }
        renewal::Request::Commit(id) => commit(ctx, &env, &proof, &copy, &current, &id),
        other @ (renewal::Request::Status(_) | renewal::Request::Consume { .. }) => {
            let id = match &other {
                renewal::Request::Status(id) | renewal::Request::Consume { id, .. } => id.clone(),
                _ => unreachable!(),
            };
            status(ctx, &env, &proof, &copy, &current, &id, other)
        }
    }
}

/// A Ready receipt has no native mutation authority. An unexpected retained
/// reservation or live capability must be resolved instead of inferred absent.
pub(super) fn ready_native_no_go(ctx: &crate::SessionCtx, id: &str) -> bool {
    let table = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if table.reservations.contains(id) || table.renewal_seals.contains_key(id) {
        return false;
    }
    let Some(operation) = table.by_id.get(id).cloned() else {
        return true;
    };
    drop(table);
    let op = operation.lock().unwrap_or_else(|p| p.into_inner());
    op.phase == super::Phase::Completed(Decision::ProvenNotCommittedRollbackSafe)
        && !op.signal_sent
        && !op.resume_sent
        && op.resume_attempt.is_none()
        && op
            .children
            .iter()
            .flatten()
            .all(|child| !child.execute_sent)
}

/// Return terminal NoGo only after the exact Ready record was durably retired.
/// Codex owns clearing its published marker before a later fresh preparation.
fn retire_stale_ready_bound(
    ctx: &crate::SessionCtx,
    env: &Env,
    proof: &crate::self_claim::Proven,
    record: &aterm_json::Value,
    expected: &Receipt,
) -> Result<Option<Receipt>, &'static str> {
    if expected.intent.purpose == renewal::Purpose::WeeklyQuota {
        return rotation::retire_stale_ready(ctx, env, proof, record, expected);
    }
    if expected.phase != Phase::Ready
        || !same_native_host(expected)
        || expected.sid != ctx.self_id.as_str()
        || expected.old_pid != proof.pid
        || Process::observe(proof.pid).is_none_or(|process| process.birth != expected.old_birth)
        || !expected.intent.matches_record(record, Some(&expected.id))
    {
        return Ok(None);
    }
    let authority = ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .supervisor_authority(crate::metrics::now_us());
    if authority.identity() != Some(proof)
        || authority.connection() != crate::control::serving_connection()
    {
        return Err("claim-changed");
    }
    let mut retired = expected.clone();
    if !retired.retire_stale_ready(authority.revision(), ctx.human_input.seq(), now_ms()) {
        return Ok(None);
    }
    renewal::with_selected(env, expected, |next| {
        if !expected.matches_successor(next) {
            return Err("selection-changed");
        }
        if renewal::read_receipt(env, &expected.id).as_ref() != Some(expected)
            || !ready_native_no_go(ctx, &expected.id)
        {
            return Ok(None);
        }
        renewal::write_receipt(env, &retired)?;
        Ok(Some(retired))
    })
    .map(Option::flatten)
}

fn commit(
    ctx: &Arc<crate::SessionCtx>,
    env: &Env,
    proof: &crate::self_claim::Proven,
    copy: &self_claimed::Copy,
    current: &aterm_json::Value,
    id: &str,
) -> String {
    let Some(mut receipt) = renewal::read_receipt(env, id) else {
        return "ERR codex-renewal unknown receipt\n".into();
    };
    if receipt.sid != ctx.self_id.as_str()
        || receipt.old_pid != proof.pid
        || Process::observe(proof.pid).is_none_or(|p| p.birth != receipt.old_birth)
        || !receipt.intent.matches_record(current, Some(id))
        || receipt.intent.thread
            != copy
                .resume
                .as_ref()
                .ok()
                .map(|r| r.session.as_str())
                .unwrap_or("")
    {
        return "ERR codex-renewal receipt changed\n".into();
    }
    match retire_stale_ready_bound(ctx, env, proof, current, &receipt) {
        Ok(Some(retired)) => return receipt_reply(&retired),
        Err(_) => return "ERR codex-renewal record unavailable\n".into(),
        Ok(None) => {}
    }
    if !same_native_host(&receipt) || receipt.phase != Phase::Ready {
        return receipt_reply(&receipt);
    }
    if now_ms() >= receipt.expires_ms
        || ctx.human_input.seq() != receipt.human_seq
        || !human_grace_ready(ctx, env)
    {
        return "ERR codex-renewal expired or interrupted\n".into();
    }
    let authority = ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .supervisor_authority(crate::metrics::now_us());
    if authority.revision() != receipt.claim_revision || authority.identity() != Some(proof) {
        return "ERR codex-renewal claim changed\n".into();
    }
    let Some(aterm_agent::harness::native_act::Request::Prepare(prepare)) =
        aterm_agent::harness::native_act::Request::parse(&receipt.native_prepare)
    else {
        return "ERR codex-renewal invalid receipt\n".into();
    };
    let Some(opts) = opts(env, ctx.self_id.as_str()) else {
        return "ERR codex-renewal unavailable\n".into();
    };
    {
        let operations = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if operations.renewal_seals.len() >= MAX_COMPLETED
            && !operations.renewal_seals.contains_key(id)
        {
            return "ERR codex-renewal capacity\n".into();
        }
    }
    if receipt.intent.purpose == renewal::Purpose::WeeklyQuota
        && rotation::commit_pool(ctx, env, &receipt).is_err()
    {
        return rotation::retire_ready(ctx, env, proof, current, &receipt)
            .ok()
            .flatten()
            .map_or_else(
                || "OK codex-renewal state=waiting\n".into(),
                |retired| retired.reply(),
            );
    }
    let Ok(Some(committing)) = renewal::commit_receipt(env, &receipt) else {
        if receipt.intent.purpose == renewal::Purpose::WeeklyQuota
            && let Ok(Some(retired)) = rotation::retire_ready(ctx, env, proof, current, &receipt)
        {
            return retired.reply();
        }
        return "ERR codex-renewal selection changed\n".into();
    };
    receipt = committing;
    let seal = Seal {
        receipt: receipt.clone(),
        prepare: (*prepare).clone(),
        proof: proof.clone(),
        ctx: Arc::downgrade(ctx),
        env: env.clone(),
        intent: Arc::new(IntentFence::new(ctx)),
        record: RecordWitness::capture(copy, current, &receipt),
        continuation: None,
        fresh_snapshot: None,
    };
    let inserted = {
        let mut operations = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if operations.renewal_seals.len() >= MAX_COMPLETED
            && !operations.renewal_seals.contains_key(id)
        {
            false
        } else {
            operations
                .renewal_services
                .insert(id.into(), Arc::new(Service::new(seal.clone())));
            operations.renewal_seals.insert(id.into(), seal);
            true
        }
    };
    if !inserted {
        reset_before_signal(env, id);
        return "ERR codex-renewal capacity\n".into();
    }
    let worker_ctx = Arc::downgrade(ctx);
    let sid = ctx.self_id.as_str().to_owned();
    let id = id.to_owned();
    let worker_env = env.clone();
    let spawned = std::thread::Builder::new()
        .name("codex-renewal".into())
        .spawn(move || {
            loop {
                let Some(ctx) = worker_ctx.upgrade() else {
                    break;
                };
                let cancelled = service::poll_cancellation(&ctx, &id);
                drop(ctx);
                match cancelled {
                    Ok(true) => break,
                    Err(_) => {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        continue;
                    }
                    Ok(false) => {}
                }
                // The receipt CAS admits one coordinator. A worker panic or lost
                // response retries only its exact retained native operation and
                // original deadline; it never invents another plan or signal.
                let _attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    // All nested control/network waits run on this private worker, never
                    // the dispatch lane handling the original Codex request.
                    if renewal::prepare_restart(&opts, &sid, &prepare).is_err() {
                        // A failed or lost preparation reply can follow a durable
                        // native claim. Keep this exact root and its seal for
                        // retry; only positive native completion can retire it.
                        return;
                    }
                    let Some(store) = worker_env.accounts_store_for(accounts::Cli::Codex) else {
                        return;
                    };
                    {
                        let Ok(Some(_guard)) = store.acquire_bounded() else {
                            return;
                        };
                        let Some(mut receipt) = renewal::read_receipt(&worker_env, &id) else {
                            return;
                        };
                        if receipt.phase != Phase::Committing {
                            return;
                        }
                        receipt.launch_attempted = true;
                        if renewal::write_receipt(&worker_env, &receipt).is_err() {
                            return;
                        }
                    }
                    if let Some(ctx) = worker_ctx.upgrade() {
                        ctx.native_policy.renewal_before_signal();
                    }
                    let observed = renewal::drive_restart(&opts, &sid, &prepare);
                    if observed.is_some() {
                        remove_seal(&worker_ctx, &id);
                    }
                    let Ok(Some(_guard)) = store.acquire_bounded() else {
                        return;
                    };
                    let Some(mut receipt) = renewal::read_receipt(&worker_env, &id) else {
                        return;
                    };
                    if receipt.phase != Phase::Committing {
                        return;
                    }
                    match observed {
                        Some(renewal::RestartCompletion::NotStarted) => {
                            receipt.phase = Phase::Retired;
                            receipt.launch_attempted = false;
                            let _saved = renewal::write_receipt(&worker_env, &receipt);
                        }
                        Some(renewal::RestartCompletion::Forward(observed)) => {
                            receipt.phase = Phase::Committed;
                            receipt.successor_pid = Some(observed.registration.vendor.pid);
                            receipt.successor_birth = Some(observed.registration.vendor.birth);
                            let _saved = renewal::write_receipt(&worker_env, &receipt);
                        }
                        Some(renewal::RestartCompletion::Rollback(observed)) => {
                            receipt.phase = Phase::Retired;
                            receipt.successor_pid = Some(observed.registration.vendor.pid);
                            receipt.successor_birth = Some(observed.registration.vendor.birth);
                            let _saved = renewal::write_receipt(&worker_env, &receipt);
                        }
                        None => {}
                    }
                    // Unknown outcome retains the original operation and its persisted
                    // native record. It never authorizes a second launch.
                }));
                let Some(current) = renewal::read_receipt(&worker_env, &id) else {
                    break;
                };
                if current.phase != Phase::Committing
                    || !aterm_agent::harness::native_act::monotonic_us()
                        .is_some_and(|now| now < prepare.deadline_us)
                {
                    break;
                }
                let retained = worker_ctx.upgrade().is_some_and(|ctx| {
                    let operation = ctx
                        .fenced_writes
                        .native
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .by_id
                        .get(&id)
                        .cloned();
                    operation.is_some_and(|operation| {
                        operation.lock().unwrap_or_else(|p| p.into_inner()).request == *prepare
                    })
                });
                if current.launch_attempted && !retained {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            service::continue_closed(&worker_ctx, &opts, &id);
            if let Some(ctx) = worker_ctx.upgrade() {
                service::observe_terminal(&ctx, &id);
                ctx.native_policy
                    .renewal_coordinator_stopped(&id, prepare.deadline_us);
            }
        });
    if spawned.is_err() {
        reset_before_signal(env, &receipt.id);
        service::observe_terminal(ctx, &receipt.id);
        remove_seal(&Arc::downgrade(ctx), &receipt.id);
        return "ERR codex-renewal worker unavailable\n".into();
    }
    receipt_reply(&receipt)
}

fn remove_seal(ctx: &std::sync::Weak<crate::SessionCtx>, id: &str) {
    if let Some(ctx) = ctx.upgrade() {
        let retired = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .renewal_seals
            .remove(id);
        drop(retired);
    }
}

fn reset_before_signal(env: &Env, id: &str) {
    let Some(store) = env.accounts_store_for(accounts::Cli::Codex) else {
        return;
    };
    let Ok(Some(_guard)) = store.acquire_bounded() else {
        return;
    };
    let Some(mut receipt) = renewal::read_receipt(env, id) else {
        return;
    };
    if receipt.reset_before_signal(now_ms()) {
        let _saved = renewal::write_receipt(env, &receipt);
    }
}

fn status(
    ctx: &Arc<crate::SessionCtx>,
    env: &Env,
    proof: &crate::self_claim::Proven,
    copy: &self_claimed::Copy,
    current: &aterm_json::Value,
    id: &str,
    request: renewal::Request,
) -> String {
    let sid = ctx.self_id.as_str();
    let Some(store) = env.accounts_store_for(accounts::Cli::Codex) else {
        return "ERR codex-renewal unavailable\n".into();
    };
    let Some(mut receipt) = renewal::read_receipt(env, id) else {
        return "ERR codex-renewal unknown receipt\n".into();
    };
    if receipt.sid != sid {
        return "ERR codex-renewal wrong session\n".into();
    }

    let Some(resume) = copy.resume.as_ref().ok() else {
        return DENIED.into();
    };
    match retire_stale_ready_bound(ctx, env, proof, current, &receipt) {
        Ok(Some(retired)) => return receipt_reply(&retired),
        Err(_) => return "ERR codex-renewal record unavailable\n".into(),
        Ok(None) => {}
    }
    if receipt.phase == Phase::Committing
        && same_native_host(&receipt)
        && let Some(opts) = opts(env, &receipt.sid)
        && let Some((forward, observed)) = renewal::completed_restart(
            &opts,
            receipt
                .current_attempt
                .as_ref()
                .map_or(id, |attempt| attempt.id.as_str()),
        )
        && observed.registration.vendor.pid == proof.pid
        && Process::observe(proof.pid).as_ref() == Some(&observed.registration.vendor)
    {
        let Ok(Some(_guard)) = store.acquire_bounded() else {
            return receipt_reply(&receipt);
        };
        if renewal::read_receipt(env, id).as_ref() == Some(&receipt) {
            receipt.phase = if forward {
                Phase::Committed
            } else {
                Phase::Retired
            };
            receipt.successor_pid = Some(observed.registration.vendor.pid);
            receipt.successor_birth = Some(observed.registration.vendor.birth);
            if renewal::write_receipt(env, &receipt).is_err() {
                return "ERR codex-renewal record unavailable\n".into();
            }
        }
    }

    let process = Process::observe(proof.pid);
    if receipt.phase == Phase::Committing
        && same_native_host(&receipt)
        && let (Some(candidate), Some(process), Some(prepare)) = (
            receipt.candidate.as_ref(),
            process.as_ref(),
            renewal::active_prepare(env, &receipt),
        )
    {
        let auth = resume
            .codex_auth_path
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| copy.home.join("auth.json"));
        let expected = if candidate.forward {
            &receipt.new_auth
        } else {
            &receipt.old_auth
        };
        let identity = auth.parent().and_then(|home| {
            let _lock = aterm_agent::harness::codex_app_login::AuthLock::try_take(&auth).ok()??;
            aterm_agent::harness::codex_app_login::renewal_identity(home)
        });
        if receipt.exec_candidate(Some(process), &prepare).is_some()
            && auth == *expected
            && resume.session == receipt.intent.thread
            && identity.as_ref()
                == if candidate.forward {
                    receipt.target_identity()
                } else {
                    Some(&receipt.intent.identity)
                }
        {
            let Ok(Some(_guard)) = store.acquire_bounded() else {
                return receipt_reply(&receipt);
            };
            if renewal::read_receipt(env, id).as_ref() == Some(&receipt) {
                receipt.phase = if candidate.forward {
                    Phase::Committed
                } else {
                    Phase::Retired
                };
                receipt.successor_pid = Some(process.pid);
                receipt.successor_birth = Some(process.birth);
                if renewal::write_receipt(env, &receipt).is_err() {
                    return "ERR codex-renewal record unavailable\n".into();
                }
            }
        }
    }
    if same_native_host(&receipt)
        && matches!(receipt.phase, Phase::Committed | Phase::Consumed)
        && resume.session == receipt.intent.thread
        && (receipt.phase == Phase::Consumed
            || receipt.intent.marker(current)["receipt"].as_str() == Some(id))
        && let Some(process) = process.as_ref()
        && (Some(process.pid) != receipt.successor_pid
            || Some(process.birth) != receipt.successor_birth)
        && let Some(options) = opts(env, sid)
        && renewal::authorized_descendant(&options, &receipt, process)
        && resume.codex_auth_path.as_deref().map(Path::new) == Some(receipt.new_auth.as_path())
    {
        let identity = receipt.new_auth.parent().and_then(|home| {
            let _auth =
                aterm_agent::harness::codex_app_login::AuthLock::try_take(&receipt.new_auth)
                    .ok()??;
            aterm_agent::harness::codex_app_login::renewal_identity(home)
        });
        if identity.as_ref() != receipt.target_identity() {
            return "ERR codex-renewal identity changed\n".into();
        }
        let Ok(Some(_guard)) = store.acquire_bounded() else {
            return receipt_reply(&receipt);
        };
        if renewal::read_receipt(env, id).as_ref() == Some(&receipt) {
            receipt.successor_pid = Some(process.pid);
            receipt.successor_birth = Some(process.birth);
            if renewal::write_receipt(env, &receipt).is_err() {
                return "ERR codex-renewal record unavailable\n".into();
            }
        }
    }
    let same_session = resume.session == receipt.intent.thread;
    let successor = process.as_ref().is_some_and(|process| {
        Some(process.pid) == receipt.successor_pid && Some(process.birth) == receipt.successor_birth
    }) && resume.codex_auth_path.as_deref().map(std::path::Path::new)
        == Some(receipt.new_auth.as_path());
    let source = process.as_ref().is_some_and(|process| {
        process.pid == receipt.old_pid && process.birth == receipt.old_birth
    });
    let current_auth = resume
        .codex_auth_path
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| copy.home.join("auth.json"));
    let abandoned_before_launch = same_native_host(&receipt)
        && !receipt.launch_attempted
        && matches!(receipt.phase, Phase::Ready | Phase::Committing)
        && Process::observe(receipt.old_pid).is_none_or(|old| old.birth != receipt.old_birth)
        && current_auth == receipt.old_auth
        && receipt.intent.marker(current)["receipt"].as_str() == Some(id);
    if same_session && abandoned_before_launch {
        let identity = current_auth.parent().and_then(|home| {
            let _auth =
                aterm_agent::harness::codex_app_login::AuthLock::try_take(&current_auth).ok()??;
            aterm_agent::harness::codex_app_login::renewal_identity(home)
        });
        if identity.as_ref() != Some(&receipt.intent.identity) {
            return "ERR codex-renewal identity changed\n".into();
        }
        let Ok(Some(_guard)) = store.acquire_bounded() else {
            return receipt_reply(&receipt);
        };
        if renewal::read_receipt(env, id).as_ref() == Some(&receipt) {
            let mut retired = receipt.clone();
            retired.phase = Phase::Retired;
            if renewal::write_receipt(env, &retired).is_err() {
                return "ERR codex-renewal record unavailable\n".into();
            }
            receipt = retired;
        }
        return receipt_reply(&receipt);
    }

    let awaiting_forward = receipt.phase == Phase::Committing
        && resume.codex_auth_path.as_deref().map(std::path::Path::new)
            == Some(receipt.new_auth.as_path())
        && receipt.intent.marker(current)["receipt"].as_str() == Some(id);
    if same_session && awaiting_forward {
        return receipt_reply(&receipt);
    }
    let rolled_back = receipt.phase == Phase::Retired
        && process.as_ref().is_some_and(|process| {
            Some(process.pid) == receipt.successor_pid
                && Some(process.birth) == receipt.successor_birth
        })
        && resume.codex_auth_path.as_deref().map(std::path::Path::new)
            == Some(receipt.old_auth.as_path());
    if same_session && rolled_back {
        if let Some(options) = opts(env, sid) {
            let _ = renewal::with_receipt_writer(env, |writer| {
                writer.retire_rolled_back_pool_record(&receipt, &options)
            });
        }
        return receipt_reply(&receipt);
    }
    if !same_session || !(source || successor) {
        return "ERR codex-renewal wrong process\n".into();
    }
    if !matches!(receipt.phase, Phase::Consumed | Phase::Retired) && !same_native_host(&receipt) {
        return receipt_reply(&receipt);
    }
    // Terminal observation is never new execution authority. A later owner
    // hold must not prevent this same lineage from acknowledging its result.
    if receipt.phase == Phase::Consumed {
        if let renewal::Request::Consume { ref turn, .. } = request
            && receipt.accepted_turn.as_ref() != Some(turn)
        {
            return "ERR codex-renewal receipt changed\n".into();
        }
        return receipt_reply(&receipt);
    }
    if receipt.phase == Phase::Committing
        && !receipt.launch_attempted
        && now_ms() >= receipt.expires_ms
    {
        let Ok(Some(_guard)) = store.acquire_bounded() else {
            return receipt_reply(&receipt);
        };
        if renewal::read_receipt(env, id).as_ref() == Some(&receipt) {
            let mut retired = receipt.clone();
            retired.phase = Phase::Retired;
            if renewal::write_receipt(env, &retired).is_err() {
                return "ERR codex-renewal record unavailable\n".into();
            }
            receipt = retired;
        }
    }
    if receipt.phase == Phase::Committed {
        if !human_grace_ready(ctx, env) {
            return DENIED.into();
        }
        if !renewal::with_selected(env, &receipt, |next| {
            if receipt.matches_successor(next) {
                Ok(())
            } else {
                Err("changed")
            }
        })
        .is_ok_and(|ready| ready.is_some())
        {
            return DENIED.into();
        }
        let identity = receipt.new_auth.parent().and_then(|home| {
            let _auth =
                aterm_agent::harness::codex_app_login::AuthLock::try_take(&receipt.new_auth)
                    .ok()??;
            aterm_agent::harness::codex_app_login::renewal_identity(home)
        });
        if !successor || identity.as_ref() != receipt.target_identity() {
            return "ERR codex-renewal identity changed\n".into();
        }
    }
    if let renewal::Request::Consume { turn, .. } = request {
        let marker = receipt.intent.marker(current);
        if !successor
            || !renewal::history(&copy.home, &receipt.intent).is_some_and(|history| {
                history.latest_is_wall
                    && marker["starts"]
                        .as_u64()
                        .is_some_and(|before| history.starts > before)
            })
            || marker["receipt"].as_str() != Some(id)
            || marker["turn"].as_str() != Some(&turn)
            || marker["accepted"].as_bool() != Some(true)
            || marker["account"].as_str() != Some(&receipt.intent.identity.account)
            || marker["user"].as_str() != Some(&receipt.intent.identity.user)
            || (receipt.intent.purpose == renewal::Purpose::WeeklyQuota
                && receipt.target_identity().is_none_or(|target| {
                    marker["destination"]["account"].as_str() != Some(&target.account)
                        || marker["destination"]["user"].as_str() != Some(&target.user)
                }))
        {
            return "ERR codex-renewal acceptance changed\n".into();
        }
        // The potentially large history read and auth-file observation above
        // hold no account writer guard. Only the final receipt CAS is serialized.
        let Ok(Some(_guard)) = store.acquire_bounded() else {
            return receipt_reply(&receipt);
        };
        if renewal::read_receipt(env, id).as_ref() != Some(&receipt) || !receipt.consume(&turn) {
            return "ERR codex-renewal receipt changed\n".into();
        }
        if renewal::write_receipt(env, &receipt).is_err() {
            return "ERR codex-renewal record unavailable\n".into();
        }
    }
    if receipt.intent.purpose == renewal::Purpose::WeeklyQuota
        && matches!(receipt.phase, Phase::Consumed | Phase::Retired)
    {
        let _cleanup = renewal::retire_pool_record(env, &receipt);
    }
    receipt_reply(&receipt)
}

/// A trusted unit world supplies the same previously authorized live-source
/// seal; it does not create a runtime activation path or a package claim.
#[cfg(all(test, unix))]
pub(super) fn fixture_after_signal(
    ctx: &Arc<crate::SessionCtx>,
    receipt: Receipt,
    prepare: Prepare,
    env: Env,
    home: std::path::PathBuf,
) -> Seal {
    let proof = ctx
        .meta
        .lock()
        .unwrap()
        .supervisor_authority(crate::metrics::now_us())
        .identity()
        .unwrap()
        .clone();
    let intent = Arc::new(IntentFence::new(ctx));
    intent.did_signal();
    let record = Some(RecordWitness {
        home,
        pid: receipt.old_pid,
        starts: 1,
        goal: aterm_json::Value::Null,
    });
    Seal {
        receipt,
        prepare,
        proof,
        ctx: Arc::downgrade(ctx),
        env,
        intent,
        record,
        continuation: None,
        fresh_snapshot: None,
    }
}

/// The trusted launcher has attached but has received no Execute permission.
/// Persist its exact birth/image and captured recipe before any Go can be sent.
pub(super) fn persist_attached(operation: &Arc<Mutex<Operation>>, phase: ImagePhase) -> bool {
    let (seal, child) = {
        let op = operation.lock().unwrap_or_else(|p| p.into_inner());
        let Some(seal) = op.authority.renewal.clone() else {
            return true;
        };
        let Some(child) = op.children[phase_index(phase)].as_ref() else {
            return false;
        };
        (seal, child.process.clone())
    };
    let env = seal.environment();
    let Some(store) = env.accounts_store_for(accounts::Cli::Codex) else {
        return false;
    };
    let Ok(Some(_guard)) = store.acquire_bounded() else {
        return false;
    };
    let Some(mut receipt) = seal.active_receipt(&env) else {
        return false;
    };
    receipt.candidate = Some(renewal::Candidate::of(&child, phase == ImagePhase::Forward));
    renewal::write_receipt(&env, &receipt).is_ok()
}

/// Disk and account observations happen before entering the Execute predicate.
/// Generic native operations keep their original path untouched.
pub(super) fn execute_recorded(operation: &Arc<Mutex<Operation>>, phase: ImagePhase) -> bool {
    let (seal, child) = {
        let op = operation.lock().unwrap_or_else(|p| p.into_inner());
        let Some(seal) = op.authority.renewal.clone() else {
            return true;
        };
        let Some(child) = op.children[phase_index(phase)].as_ref() else {
            return false;
        };
        (seal, child.process.clone())
    };
    if !seal.current()
        || seal
            .ctx
            .upgrade()
            .is_none_or(|ctx| ctx.fabric.hold().is_some())
    {
        return false;
    }
    let env = seal.environment();
    if seal
        .ctx
        .upgrade()
        .is_none_or(|ctx| !human_grace_ready(&ctx, &env))
    {
        return false;
    }
    let Some(receipt) = seal.active_receipt(&env) else {
        return false;
    };
    if seal.continuation.is_some()
        && !seal
            .record
            .as_ref()
            .is_some_and(|record| record.current(&receipt))
    {
        return false;
    }

    if !receipt
        .candidate
        .as_ref()
        .is_some_and(|candidate| candidate.matches_launcher(&child, phase == ImagePhase::Forward))
    {
        return false;
    }
    account_phase_allowed(&env, &receipt, phase)
}

fn account_phase_allowed(env: &Env, receipt: &Receipt, phase: ImagePhase) -> bool {
    if phase == ImagePhase::Rollback && receipt.intent.purpose == renewal::Purpose::WeeklyQuota {
        return aterm_agent::harness::codex_rotation::with_rollback_writer(
            env,
            receipt,
            |_| Ok(()),
        )
        .is_ok_and(|ready| ready.is_some());
    }
    renewal::with_selected(env, receipt, |next| {
        if receipt.matches_successor(next) {
            Ok(())
        } else {
            Err("changed")
        }
    })
    .is_ok_and(|ready| ready.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_timeline::scoped_input_tests::{install, scoped};
    use std::sync::atomic::AtomicBool;

    fn seal(ctx: &Arc<crate::SessionCtx>) -> Seal {
        let proof = scoped(&[Scope::Relaunch]).proven;
        let image = Image {
            path: b"/fixture-image".to_vec(),
            device: 1,
            inode: 2,
        };
        let process = Process {
            pid: 4242,
            birth: 12,
            parent: 42,
            group: 4242,
            session: 42,
            image: image.clone(),
        };
        let command = RestartCommand {
            account_intent: None,
            image: image.clone(),
            program: "/fixture-image".into(),
            argv: vec!["resume".into()],
            env: vec![("CODEX_AUTH_PATH".into(), "/new/auth.json".into())],
            unset: None,
            cwd: None,
            heal: None,
            dialect: aterm_agent::harness::upgrade::Dialect::Zsh,
        };
        let revision = ctx
            .meta
            .lock()
            .unwrap()
            .supervisor_authority(crate::metrics::now_us())
            .revision();
        let prepare = Prepare {
            id: "ab".repeat(16),
            host_epoch: host_epoch().unwrap().into(),
            holder: crate::harness_host::holder(),
            revision,
            expected_human_seq: Some(ctx.human_input.seq()),
            deadline_us: u64::MAX,
            duty: Scope::Relaunch,
            plan: NativePlan::Restart {
                cold: None,
                process,
                shell: Process {
                    pid: 42,
                    birth: 10,
                    parent: 1,
                    group: 42,
                    session: 42,
                    image: image.clone(),
                },
                observation: None,
                launcher: image,
                socket: "/fixture.sock".into(),
                forward: Box::new(command),
                rollback: None,
            },
        };
        let receipt = Receipt {
            destination: None,
            id: prepare.id.clone(),
            intent: Intent {
                purpose: Default::default(),
                thread: "019a4b54-5fa1-7123-8123-123456789abc".into(),
                wall: "019a4b54-5fa2-7123-8123-123456789abc".into(),
                identity: renewal::Identity::from_native("account", "user"),
            },
            sid: ctx.self_id.as_str().into(),
            old_pid: 4242,
            old_birth: 12,
            claim_revision: revision,
            human_seq: ctx.human_input.seq(),
            old_auth: "/old/auth.json".into(),
            new_auth: "/new/auth.json".into(),
            generation: 1,
            row_stamp: String::new(),
            native_prepare: String::new(),
            expires_ms: u64::MAX,
            phase: Phase::Ready,
            successor_pid: None,
            successor_birth: None,
            accepted_turn: None,
            launch_attempted: false,
            candidate: None,
            current_attempt: None,
            revoked: false,
        };
        Seal {
            receipt,
            prepare,
            proof,
            ctx: Arc::downgrade(ctx),
            env: Env::from_process().unwrap(),
            intent: Arc::new(IntentFence::new(ctx)),
            record: None,
            continuation: None,
            fresh_snapshot: None,
        }
    }

    #[test]
    fn codex_renewal_terminal_rollback_keeps_service_until_completion_is_persisted() {
        let handle = crate::session_store::test_handle_on(8893, -1);
        let ctx = Arc::clone(&handle.ctx);
        install(&ctx, 71, &[Scope::Relaunch]);
        let directory = aterm_tempfile::tempdir().unwrap();
        let mut seed = seal(&ctx);
        seed.env.home = Some(directory.path().join("home"));
        seed.env.state = directory.path().join("state");
        seed.env.config = Some(directory.path().join("aterm.toml"));
        seed.receipt.intent.purpose = renewal::Purpose::WeeklyQuota;
        seed.receipt.destination = Some(renewal::Identity::from_native("target", "user"));
        seed.receipt.phase = Phase::Retired;
        seed.receipt.launch_attempted = true;
        let NativePlan::Restart { process, .. } = &seed.prepare.plan else {
            panic!("restart")
        };
        seed.receipt.candidate = Some(renewal::Candidate::of(process, false));
        seed.receipt.successor_pid = Some(process.pid);
        seed.receipt.successor_birth = Some(process.birth);
        seed.receipt.native_prepare =
            aterm_agent::harness::native_act::Request::prepare(seed.prepare.clone())
                .encode()
                .unwrap();
        let id = seed.receipt.id.clone();
        renewal::with_receipt_writer(&seed.env, |_| {
            renewal::write_receipt(&seed.env, &seed.receipt)
        })
        .unwrap();
        ctx.fenced_writes
            .native
            .lock()
            .unwrap()
            .renewal_services
            .insert(id.clone(), Arc::new(Service::new(seed)));
        assert!(!service::observe_terminal(&ctx, &id));
        assert!(
            ctx.fenced_writes
                .native
                .lock()
                .unwrap()
                .renewal_services
                .contains_key(&id)
        );
    }

    #[test]
    fn codex_renewal_unknown_terminal_operation_keeps_launch_uncertainty() {
        let handle = crate::session_store::test_handle_on(8894, -1);
        let ctx = Arc::clone(&handle.ctx);
        install(&ctx, 71, &[Scope::Relaunch]);
        let directory = aterm_tempfile::tempdir().unwrap();
        let mut seed = seal(&ctx);
        seed.env.home = Some(directory.path().join("home"));
        seed.env.state = directory.path().join("state");
        seed.env.config = Some(directory.path().join("aterm.toml"));
        seed.receipt.intent.purpose = renewal::Purpose::WeeklyQuota;
        seed.receipt.destination = Some(renewal::Identity::from_native("target", "user"));
        seed.receipt.phase = Phase::Retired;
        seed.receipt.launch_attempted = true;
        seed.receipt.revoked = true;
        seed.receipt.native_prepare =
            aterm_agent::harness::native_act::Request::prepare(seed.prepare.clone())
                .encode()
                .unwrap();
        let id = seed.receipt.id.clone();
        let env = seed.env.clone();
        renewal::with_receipt_writer(&env, |_| renewal::write_receipt(&env, &seed.receipt))
            .unwrap();
        ctx.fenced_writes
            .native
            .lock()
            .unwrap()
            .renewal_services
            .insert(id.clone(), Arc::new(Service::new(seed)));
        assert!(!service::observe_terminal(&ctx, &id));
        assert!(renewal::read_receipt(&env, &id).unwrap().launch_attempted);
    }

    #[test]
    fn codex_renewal_seal_is_exact_plan_claim_and_private_constructor_only() {
        let handle = crate::session_store::test_handle_on(8873, -1);
        let ctx = Arc::clone(&handle.ctx);
        install(&ctx, 71, &[Scope::Relaunch]);
        let seal = seal(&ctx);
        let meta = ctx.meta.lock().unwrap();
        let snapshot = meta.supervisor_authority(crate::metrics::now_us());
        assert!(seal.admits(&snapshot, &seal.prepare));
        let mut altered = seal.prepare.clone();
        altered.id = "cd".repeat(16);
        assert!(!seal.admits(&snapshot, &altered));
        if let NativePlan::Restart { forward, .. } = &mut altered.plan {
            forward.env.clear();
        }
        altered.id = seal.prepare.id.clone();
        assert!(!seal.admits(&snapshot, &altered));
        let actor = Process {
            pid: 900,
            birth: 1,
            parent: 1,
            group: 900,
            session: 900,
            image: Image {
                path: b"/host".to_vec(),
                device: 1,
                inode: 1,
            },
        };
        let external = SupervisorInputCaller {
            principal: crate::control::Principal::Instance,
            connection: Some(71),
            peer: Some(4242),
            proof: Some(seal.proof.clone()),
        };
        assert!(
            capture_native_authority(
                &ctx,
                &meta,
                &seal.prepare,
                &external,
                Some(actor.clone()),
                Some(Conversation::Facts(Arc::new(AtomicBool::new(true)))),
                crate::metrics::now_us(),
                Some(seal.clone()),
                None,
                &System(&ctx.native_policy, true),
            )
            .is_err()
        );
        let private = SupervisorInputCaller {
            principal: crate::control::Principal::Harness,
            connection: Some(72),
            peer: Some(900),
            proof: None,
        };
        assert!(
            capture_native_authority(
                &ctx,
                &meta,
                &seal.prepare,
                &private,
                Some(actor),
                Some(Conversation::Facts(Arc::new(AtomicBool::new(true)))),
                crate::metrics::now_us(),
                Some(seal.clone()),
                None,
                &System(&ctx.native_policy, true),
            )
            .is_ok()
        );
        drop(meta);
        install(&ctx, 73, &[Scope::Upgrade]);
        let changed = ctx
            .meta
            .lock()
            .unwrap()
            .supervisor_authority(crate::metrics::now_us());
        assert!(!seal.admits(&changed, &seal.prepare));
    }

    #[test]
    fn codex_renewal_retained_prepare_replays_after_source_proof_is_gone() {
        let handle = crate::session_store::test_handle_on(8875, -1);
        let ctx = Arc::clone(&handle.ctx);
        install(&ctx, 71, &[Scope::Relaunch]);
        let seal = seal(&ctx);
        assert!(!seal.preflight());
        let actor = Process::observe(std::process::id()).unwrap();
        let caller = SupervisorInputCaller {
            principal: crate::control::Principal::Harness,
            connection: Some(72),
            peer: Some(actor.pid),
            proof: None,
        };
        let authority = {
            let meta = ctx.meta.lock().unwrap();
            let mut cold_request = seal.prepare.clone();
            let NativePlan::Restart { cold, rollback, .. } = &mut cold_request.plan else {
                unreachable!();
            };
            *cold = Some(Box::new(aterm_agent::harness::native_act::ColdIntent {
                login: None,
                home: None,
                auth_observed: false,
                stored_login: true,
                conversation: Some("captured-conversation".into()),
                profile: None,
                mode: aterm_agent::harness::accounts::Mode::Wall,
                billing: aterm_agent::harness::native_act::ColdBilling::Keep,
            }));
            *rollback = None;
            assert_eq!(
                cold_request.plan.constructor(),
                aterm_agent::harness::native_act::Constructor::ColdRestart
            );
            assert!(
                capture_native_authority(
                    &ctx,
                    &meta,
                    &cold_request,
                    &caller,
                    Some(actor.clone()),
                    Some(Conversation::Facts(Arc::new(AtomicBool::new(true)))),
                    crate::metrics::now_us(),
                    Some(seal.clone()),
                    None,
                    &System(&ctx.native_policy, true),
                )
                .is_err(),
                "a real renewal seal cannot authorize a different cold plan"
            );
            capture_native_authority(
                &ctx,
                &meta,
                &seal.prepare,
                &caller,
                Some(actor),
                Some(Conversation::Facts(Arc::new(AtomicBool::new(true)))),
                crate::metrics::now_us(),
                Some(seal.clone()),
                None,
                &System(&ctx.native_policy, true),
            )
            .unwrap()
        };
        let binding = Binding {
            id: seal.prepare.id.clone(),
            capability: "ab".repeat(32),
        };
        let operation = Arc::new(Mutex::new(Operation {
            request: seal.prepare.clone(),
            binding: binding.clone(),
            authority,
            custody: None,
            phase: super::super::Phase::Started,
            preparation: super::super::Preparation::recorded_for_test(),
            grant_record: super::super::GrantRecord::None,
            signal_sent: true,
            signal_accepted: Some(true),
            resume_sent: false,
            resume_attempt: None,
            human_seq: ctx.human_input.seq(),
            channel: Some(72),
            channel_closed: false,
            claim_outcome: ClaimOutcome::Builtin,
            commit_version: None,
            completion_drain: None,
            resume_phase: None,
            children: [None, None],
            cold_facts: None,
            cold_resolving: false,
            resolved_cold: None,
            effect_started: Arc::new(AtomicBool::new(true)),
            clock: Clock::System,
            observation: None,
            outcome_slot: None,
            completed_observation: None,
            source_death: None,
            closed_restart: None,
            resume_only: false,
        }));
        {
            let mut table = ctx.fenced_writes.native.lock().unwrap();
            table.renewal_seals.insert(binding.id.clone(), seal.clone());
            table.by_id.insert(binding.id.clone(), operation);
        }
        let reply = super::super::prepare(
            &ctx,
            -1,
            seal.prepare.clone(),
            caller.clone(),
            &System(&ctx.native_policy, true),
        );
        assert_eq!(reply, admitted(&binding));
        let mut altered = seal.prepare.clone();
        if let NativePlan::Restart { forward, .. } = &mut altered.plan {
            forward.env.clear();
        }
        assert_eq!(
            super::super::prepare(&ctx, -1, altered, caller, &System(&ctx.native_policy, true)),
            DENIED
        );
    }

    #[test]
    fn codex_renewal_cold_host_holds_nonterminal_but_keeps_terminal_evidence() {
        let handle = crate::session_store::test_handle_on(8879, -1);
        let mut sealed = seal(&handle.ctx);
        sealed.prepare.host_epoch = "ef".repeat(16);
        sealed.receipt.native_prepare =
            aterm_agent::harness::native_act::Request::prepare(sealed.prepare)
                .encode()
                .unwrap();
        for phase in [Phase::Ready, Phase::Committing, Phase::Committed] {
            sealed.receipt.phase = phase;
            assert!(receipt_reply(&sealed.receipt).contains("state=waiting"));
        }
        sealed.receipt.phase = Phase::Consumed;
        assert!(receipt_reply(&sealed.receipt).contains("state=consumed"));
        sealed.receipt.phase = Phase::Retired;
        assert!(receipt_reply(&sealed.receipt).contains("state=retired"));
    }

    #[test]
    fn codex_renewal_grace_uses_native_human_clock_not_sequence_alone() {
        let handle = crate::session_store::test_handle_on(8877, -1);
        assert!(human_grace_elapsed(&handle.ctx, 2, 10_000_000));
        handle
            .ctx
            .human_input
            .note_reaching(10_000_000, crate::human_input::Gesture::Typed, true);
        let sequence = handle.ctx.human_input.seq();
        assert!(!human_grace_elapsed(&handle.ctx, 2, 10_001_000));
        assert!(human_grace_elapsed(&handle.ctx, 2, 12_000_000));
        assert_eq!(handle.ctx.human_input.seq(), sequence);
    }

    #[test]
    fn codex_renewal_requires_sealed_platform_authority() {
        let policy = crate::native_session_policy::SessionPolicy::default();
        for kind in [Constructor::OwnImage, Constructor::PrivateRestart] {
            assert!(
                !System(&policy, false).constructor_enabled(kind),
                "an unsealed request cannot use a generic native constructor"
            );
        }
        assert!(!System(&policy, true).constructor_enabled(Constructor::OwnImage));
        assert_eq!(
            System(&policy, true).constructor_enabled(Constructor::PrivateRestart),
            cfg!(all(target_os = "macos", target_arch = "aarch64"))
        );
        for sealed in [false, true] {
            assert_eq!(
                System(&policy, sealed).constructor_enabled(Constructor::ColdRestart),
                crate::native_session_policy::cold_restart_qualified(),
                "the cold constructor is independent of a Codex renewal seal"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn codex_renewal_respects_the_fixture_constructor_gate() {
        let mut handle = crate::session_store::test_handle_on(8874, -1);
        let image = Image {
            path: b"/fixture".to_vec(),
            device: 1,
            inode: 1,
        };
        let policy = crate::native_session_policy::FixturePolicy::new(
            std::slice::from_ref(
                aterm_agent::harness::contract::row_named("grok-alab").expect("rostered"),
            ),
            vec![crate::native_session_policy::FixtureImages {
                row: "grok-alab",
                images: vec![image],
            }],
            aterm_agent::supervise::SupervisorConfig::default(),
            &[Constructor::PrivateRestart],
        )
        .unwrap();
        let gate = policy.constructors();
        assert!(
            Arc::get_mut(&mut handle.ctx)
                .unwrap()
                .install_fixture_policy(policy)
                .is_ok()
        );
        assert!(
            System(&handle.ctx.native_policy, true)
                .constructor_enabled(Constructor::PrivateRestart)
        );
        gate.set_enabled(&[]);
        assert!(
            !System(&handle.ctx.native_policy, true)
                .constructor_enabled(Constructor::PrivateRestart)
        );
    }
}
