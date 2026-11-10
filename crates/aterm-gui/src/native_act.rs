// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual native mutation custody. Metadata observations and analytics are never grants.
//! Admission uses Meta -> operation table -> input ordering; mutation uses Meta -> operation
//! predicate -> input ordering. Table lookup drops its guard before an operation predicate.
//! Outcome disposal and predecessor waits hold none of those locks. The table, rather than
//! a request/connection stack, owns every started operation, including a lost final ACK.

#[cfg(unix)]
#[path = "native_recipient_recovery.rs"]
mod recipient_recovery;
#[path = "native_source_restart.rs"]
mod source_restart;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_agent::harness::contract::Scope;
use aterm_agent::harness::native_act::{
    AskedIntent, Binding, BootStamp, CancelPrepare, ClaimOutcome, Closure, ColdEnvironmentFacts,
    ColdIntent, Constructor, Decision, Image, ImagePhase, NativePlan, Prepare, Process, Recorded,
    Request, RestartCommand, Retirement,
};

use crate::session_timeline::{
    NativeCustody, NativeDrain, SupervisorAuthority, SupervisorInputCaller,
};

#[cfg(test)]
#[path = "native_act_tests.rs"]
mod tests;

#[path = "native_closed_restart.rs"]
mod closed_restart;
use closed_restart::ClosedRestart;

#[path = "native_asked_upgrade.rs"]
mod asked_upgrade;
#[path = "native_relaunch_permit.rs"]
mod relaunch_permit;
pub(crate) use relaunch_permit::authorize as authorize_checked_relaunch;

#[path = "native_account_continuation.rs"]
mod account_continuation;
use account_continuation::AccountContinuation;

#[path = "native_account_observation.rs"]
mod account_observation;

#[path = "native_account_claim.rs"]
mod account_claim;

#[path = "native_account_fence.rs"]
mod account_fence;

#[path = "native_account_retention.rs"]
mod account_retention;
use account_retention::{AccountLineages, AccountSlot};

#[path = "native_preparation.rs"]
mod preparation;
use preparation::{Preparation, Preparing, finish_preparation, retry_preparation};

#[path = "native_observe_control.rs"]
mod observe_control;
pub(crate) use observe_control::serve as serve_observation;

#[path = "codex_renewal_control.rs"]
mod renewal;
pub(crate) use renewal::serve as serve_codex_renewal;
pub(crate) use renewal::serve_rotation as serve_codex_rotation;
#[path = "codex_accounts_control.rs"]
mod accounts_control;
pub(crate) use accounts_control::serve as serve_codex_accounts;
#[cfg(unix)]
#[path = "codex_auth_control.rs"]
mod auth_control;
#[cfg(unix)]
#[path = "codex_source_control.rs"]
mod source_control;

/// Credential requests use this app's active configuration, just as owned
/// constructors do. Process defaults cannot select a different account roster.
pub(crate) fn codex_account_env() -> Result<aterm_agent::harness::cli::Env, String> {
    let mut env = aterm_agent::harness::cli::Env::from_process()?;
    env.config = crate::app_config::config_path();
    Ok(env)
}

/// Transport cleanup is independent of session-store enumeration.
pub(crate) fn source_connection_closed(connection: u64) {
    #[cfg(unix)]
    source_control::connection_closed(connection);
    #[cfg(not(unix))]
    let _ = connection;
}

pub(crate) fn serve_codex_auth(
    ctx: &Arc<crate::SessionCtx>,
    master: i32,
    line: &str,
    lifetime: &aterm_agent::harness::account_work::Lifetime,
    registry: &Arc<aterm_agent::harness::codex_source_registry::Registry>,
    connection: Option<u64>,
) -> String {
    #[cfg(unix)]
    {
        if source_control::handles(line) {
            source_control::serve(ctx, master, line, lifetime, registry, connection)
        } else {
            auth_control::serve(ctx, master, line, lifetime)
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (ctx, master, line, lifetime, registry, connection);
        "ERR codex-auth unsupported\n".into()
    }
}
#[cfg(unix)]
#[path = "codex_enrollment_control.rs"]
mod enrollment_control;

pub(crate) fn serve_codex_assignment_policy(
    ctx: &Arc<crate::SessionCtx>,
    master: i32,
    line: &str,
    registry: &Arc<aterm_agent::harness::codex_source_registry::Registry>,
    connection: Option<u64>,
) -> String {
    #[cfg(unix)]
    {
        source_control::serve_policy(ctx, master, line, registry, connection)
    }
    #[cfg(not(unix))]
    {
        let _ = (ctx, master, line, registry, connection);
        "ERR codex-accounts-change unsupported\n".into()
    }
}

pub(crate) fn serve_codex_enrollment(
    ctx: &Arc<crate::SessionCtx>,
    master: i32,
    line: &str,
    lifetime: &aterm_agent::harness::account_work::Lifetime,
    read: bool,
) -> String {
    #[cfg(unix)]
    {
        enrollment_control::serve(ctx, master, line, lifetime, read)
    }
    #[cfg(not(unix))]
    {
        let _ = (ctx, master, line, lifetime);
        if read {
            "ERR codex-accounts unsupported\n".into()
        } else {
            "ERR codex-accounts-change unsupported\n".into()
        }
    }
}

pub(crate) fn cancel_renewal_owner(ctx: &crate::SessionCtx) -> Result<bool, &'static str> {
    #[cfg(unix)]
    let exec = exec_handoff::cancel_owner(ctx);
    #[cfg(not(unix))]
    let exec = false;
    renewal::cancel_owner(ctx).map(|cancelled| cancelled || exec)
}
pub(crate) fn renewal_pending(ctx: &crate::SessionCtx) -> bool {
    #[cfg(unix)]
    if exec_handoff::pending(ctx) {
        return true;
    }
    renewal::pending(ctx)
}
pub(crate) fn retain_exec_claim(ctx: &crate::SessionCtx, connection: u64) -> bool {
    #[cfg(unix)]
    {
        exec_handoff::retain_claim(ctx, connection)
    }
    #[cfg(not(unix))]
    {
        let _ = (ctx, connection);
        false
    }
}

/// A cancelled native cohort is positively closed and its exact original shell
/// is in front. Stale task chrome must not turn Stop into a shell signal.
pub(crate) fn cancelled_native_shell_front(ctx: &crate::SessionCtx, master: i32) -> bool {
    let operations = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .by_id
        .values()
        .cloned()
        .collect::<Vec<_>>();
    let kernel = System(&ctx.native_policy, false);
    operations.iter().any(|operation| {
        let op = operation.lock().unwrap_or_else(|p| p.into_inner());
        let NativePlan::Restart { shell, .. } = &op.request.plan else {
            return false;
        };
        op.authority
            .renewal
            .as_ref()
            .is_some_and(|seal| seal.durably_cancelled(ctx))
            && matches!(op.phase, Phase::Completed(Decision::Retired(_)))
            && op
                .closed_restart
                .as_ref()
                .is_some_and(|proof| proof.quiescent(&kernel))
            && op.custody.is_none()
            && op
                .completion_drain
                .as_ref()
                .is_none_or(NativeDrain::is_drained)
            && kernel.process(shell.pid).as_ref() == Some(shell)
            && kernel.foreground(master) == Some(shell.group)
    })
}

#[cfg(unix)]
#[path = "codex_exec_handoff_control.rs"]
mod exec_handoff;
#[cfg(unix)]
pub(crate) use exec_handoff::serve as serve_codex_exec_handoff;
#[cfg(not(unix))]
pub(crate) fn serve_codex_exec_handoff(_: &Arc<crate::SessionCtx>, _: i32, _: &str) -> String {
    DENIED.into()
}

const DENIED: &str = "ERR denied native operation authority\n";
const RECIPIENT_WRITER_UNAVAILABLE: &str = "ERR native complete-recipient writer unavailable\n";
const UNKNOWN: &str = "OK native-act state=unknown\n";
const NOEXEC: &str = "OK native-act state=retired reason=no-exec-grant\n";
#[cfg(test)]
const COMMITTED: &str = "OK native-act state=committed claim=lost revision=-\n";
const ROLLBACK_SAFE: &str = "OK native-act state=rollback-safe\n";
const DEADLINE: &str = "ERR native operation deadline\n";
const MAX_COMPLETED: usize = 128;
const MAX_PREPARE_US: u64 = 120_000_000;

fn host_epoch() -> Option<&'static str> {
    static EPOCH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    EPOCH
        .get_or_init(|| aterm_agent::harness::native_act::operation_id().ok())
        .as_deref()
}

fn own_image_rows(policy: &crate::native_session_policy::SessionPolicy) -> String {
    let rows = policy
        .rows()
        .iter()
        .filter(|row| row.allows.contains(&Scope::Upgrade))
        .map(|row| format!("{}:upgrade", row.id))
        .collect::<Vec<_>>();
    if rows.is_empty() {
        "-".into()
    } else {
        rows.join(",")
    }
}

/// The one transaction clock is shared with the real self-exec constructor across exec.
pub(crate) fn remaining(deadline_us: u64) -> Option<Duration> {
    Clock::System.remaining(deadline_us)
}

#[derive(Clone, Debug)]
enum Clock {
    System,
    #[cfg(test)]
    Facts(Arc<std::sync::atomic::AtomicU64>),
}

impl Clock {
    fn now_us(&self) -> Option<u64> {
        match self {
            Self::System => aterm_agent::harness::native_act::monotonic_us(),
            #[cfg(test)]
            Self::Facts(now) => Some(now.load(std::sync::atomic::Ordering::Acquire)),
        }
    }
    fn remaining(&self, deadline_us: u64) -> Option<Duration> {
        let left = deadline_us.checked_sub(self.now_us()?)?;
        (left != 0).then(|| Duration::from_micros(left))
    }
}

#[derive(Debug, Default)]
pub(crate) struct Operations {
    asked_binding: asked_upgrade::CurrentBinding,
    relaunch_pending: Option<Arc<relaunch_permit::Permit>>,
    #[cfg(unix)]
    exec_prepare: Option<Arc<exec_handoff::Preparation>>,
    #[cfg(unix)]
    exec_operation: Option<Arc<Mutex<exec_handoff::Operation>>>,
    observations: Arc<Mutex<crate::native_observe::Store>>,
    renewal_seals: BTreeMap<String, renewal::Seal>,
    renewal_services: BTreeMap<String, Arc<renewal::Service>>,
    rotation_prepare: Option<Arc<renewal::RotationPreparation>>,
    by_id: BTreeMap<String, Arc<Mutex<Operation>>>,
    completed: std::collections::VecDeque<String>,
    coordinated_connections: std::collections::BTreeSet<u64>,
    coordinated_actors: Vec<(u32, u64)>,
    builtin_coordinated: bool,
    reservations: BTreeMap<String, Option<aterm_agent::harness::native_account_wire::Relation>>,
    account_fences: BTreeMap<String, aterm_agent::harness::native_account_wire::Relation>,
    cancelled: BTreeMap<String, CancelledPrepare>,
    receipt_flights: BTreeMap<String, usize>,
    account_lineages: AccountLineages,
}

/// A data-only seal for a preparation that had not published an operation. The original
/// absolute deadline also fences late insertion after this bounded record is reclaimed.
#[derive(Debug)]
struct CancelledPrepare {
    request: Prepare,
    principal: crate::control::Principal,
    actor: Process,
}

impl Operations {
    fn slots(&self) -> usize {
        self.by_id
            .len()
            .saturating_add(self.reservations.len())
            .saturating_add(
                self.cancelled
                    .keys()
                    .filter(|id| !self.reservations.contains_key(*id))
                    .count(),
            )
            .saturating_add(self.account_lineages.spares())
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NoExecFlight",
            action = "Reclaim",
            project = "native_act::tests::receipts::flight_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NoExecFlight",
            action = "Expire",
            reason = "The injected monotonic clock passes the closed operation deadline"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUnadmittedPrepare",
            action = "Reclaim",
            project = "aterm_gui::native_act::tests::unadmitted::Host::probe"
        )
    )]
    fn reclaim(&mut self, now_us: u64) -> Vec<Arc<Mutex<Operation>>> {
        relaunch_permit::reclaim_pending(self, now_us);
        let mut retired = self.compact_account_lineages();
        // Compaction can release the committed current root's final account pin.
        // Preserve its positive settlement before expiry removes the operation.
        let _settlement = self.settle_asked_binding();
        let expired_seals = self
            .cancelled
            .iter()
            .filter(|(id, seal)| {
                seal.request.deadline_us <= now_us && !self.reservations.contains_key(*id)
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in expired_seals {
            self.cancelled.remove(&id);
        }
        let expired = self
            .completed
            .iter()
            .filter_map(|id| {
                if self.receipt_flights.contains_key(id) || self.account_lineages.pins(id) {
                    return None;
                }
                let operation = self.by_id.get(id)?;
                let op = operation.try_lock().ok()?;
                (op.request.deadline_us <= now_us
                    && matches!(op.phase, Phase::Completed(_))
                    && !op.continuation_pending()
                    && op
                        .completion_drain
                        .as_ref()
                        .is_none_or(NativeDrain::is_drained))
                .then(|| id.clone())
            })
            .collect::<Vec<_>>();
        for id in expired {
            self.completed.retain(|old| old != &id);
            if let Some(operation) = self.by_id.remove(&id) {
                retired.push(operation);
            }
        }
        retired
    }
}

/// A bounded data slot, acquired before any app/input custody or watcher resource. Drop
/// cancels an unpublished reservation; it cannot publish a native plan or renew a deadline.
struct PrepareSlot<'a> {
    ctx: &'a crate::SessionCtx,
    id: String,
    published: bool,
    account: AccountSlot,
}

/// Transient data-only pin, installed under the same table lock as reclamation
/// before looking up the operation. Drop releases it on every return path.
struct ReceiptFlight<'a> {
    ctx: &'a crate::SessionCtx,
    id: String,
}

impl<'a> ReceiptFlight<'a> {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NoExecFlight",
            action = "BeginA",
            project = "native_act::tests::receipts::flight_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NoExecFlight",
            action = "BeginB",
            project = "native_act::tests::receipts::flight_transition"
        )
    )]
    fn begin(ctx: &'a crate::SessionCtx, request: &Request) -> Result<Option<Self>, ()> {
        let id = match request {
            Request::Prepare(prepare) if matches!(prepare.plan, NativePlan::Restart { .. }) => {
                &prepare.id
            }
            // Cancellation applies only to OwnImage and cannot produce a
            // restart NoExec receipt. Its expired-history sweep must run.
            Request::Prepare(_) | Request::CancelPrepare(_) => return Ok(None),
            Request::Precommit(binding, _)
            | Request::Commit(binding, _, _)
            | Request::Query(binding)
            | Request::Abort(binding)
            | Request::Recover(binding)
            | Request::Signal(binding)
            | Request::Resume(binding, _, _, _)
            | Request::Attach(binding, _)
            | Request::Resolve(binding, _, _)
            | Request::Execute(binding, _)
            | Request::ExecFailed(binding, _, _)
            | Request::Reconcile(binding) => &binding.id,
            _ => return Ok(None),
        };
        let mut table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let count = table.receipt_flights.entry(id.clone()).or_default();
        *count = count.checked_add(1).ok_or(())?;
        Ok(Some(Self {
            ctx,
            id: id.clone(),
        }))
    }
}

impl Drop for ReceiptFlight<'_> {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NoExecFlight",
            action = "EndA",
            project = "native_act::tests::receipts::flight_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NoExecFlight",
            action = "EndB",
            project = "native_act::tests::receipts::flight_transition"
        )
    )]
    fn drop(&mut self) {
        let mut table = self
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(count) = table.receipt_flights.get_mut(&self.id) {
            *count -= 1;
            if *count == 0 {
                table.receipt_flights.remove(&self.id);
            }
        }
    }
}

impl<'a> PrepareSlot<'a> {
    #[cfg(test)]
    fn reserve(
        ctx: &'a crate::SessionCtx,
        id: &str,
        now_us: u64,
        crash: bool,
        account: AccountSlot,
    ) -> Result<Self, String> {
        Self::reserve_relation(ctx, id, now_us, crash, account, None)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUnadmittedPrepare",
            action = "Reserve",
            project = "aterm_gui::native_act::tests::unadmitted::Host::probe"
        )
    )]
    fn reserve_relation(
        ctx: &'a crate::SessionCtx,
        id: &str,
        now_us: u64,
        crash: bool,
        account: AccountSlot,
        relation: Option<&aterm_agent::harness::native_account_wire::Relation>,
    ) -> Result<Self, String> {
        let (retired, result) = {
            let mut table = ctx
                .fenced_writes
                .native
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let retired = table.reclaim(now_us);
            let unresolved = crash
                && (!table.reservations.is_empty()
                    || table.by_id.values().any(|operation| {
                        operation.try_lock().map_or(true, |op| {
                            !matches!(op.phase, Phase::Completed(_))
                                || op
                                    .completion_drain
                                    .as_ref()
                                    .is_some_and(|drain| !drain.is_drained())
                        })
                    }));
            let result = if unresolved {
                Err("ERR busy unresolved native operation\n".into())
            } else if table.by_id.contains_key(id)
                || table.reservations.contains_key(id)
                || table.account_fences.contains_key(id)
                || table.cancelled.contains_key(id)
            {
                Err("ERR busy native operation prepare\n".into())
            } else {
                let occupied = table.slots();
                table
                    .account_lineages
                    .reserve(&account, id, occupied)
                    .map(|()| {
                        table.reservations.insert(id.to_string(), relation.cloned());
                    })
            };
            (retired, result)
        };
        // Completed history has data only; even its final Arc destruction stays outside every
        // metadata, native predicate and table lock, including a retained input observation.
        drop(retired);
        result.map(|()| Self {
            ctx,
            id: id.to_string(),
            published: false,
            account,
        })
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUnadmittedPrepare",
            action = "Publish",
            project = "aterm_gui::native_act::tests::unadmitted::Host::probe"
        )
    )]
    fn publish(&mut self, table: &mut Operations, operation: Arc<Mutex<Operation>>) -> bool {
        {
            let op = operation.lock().unwrap_or_else(|p| p.into_inner());
            if !relaunch_permit::accept(table, &op) {
                return false;
            }
            table.admitted_binding(&op);
        }
        table.reservations.remove(&self.id);
        table.by_id.insert(self.id.clone(), operation);
        self.published = true;
        true
    }
}

impl Drop for PrepareSlot<'_> {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUnadmittedPrepare",
            action = "Drop",
            project = "aterm_gui::native_act::tests::unadmitted::Host::probe"
        )
    )]
    fn drop(&mut self) {
        if !self.published {
            let mut table = self
                .ctx
                .fenced_writes
                .native
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            table.reservations.remove(&self.id);
            table.account_lineages.cancel(&self.account, &self.id);
        }
    }
}

#[derive(Clone, Debug)]
struct Authority {
    asked: Option<asked_upgrade::Qualification>,
    snapshot: SupervisorAuthority,
    caller: SupervisorInputCaller,
    builtin: bool,
    renewal: Option<renewal::Seal>,
    account: Option<AccountContinuation>,
    actor: Option<Process>,
    conversation: Option<Conversation>,
}

impl Authority {
    fn asked_matches(&self, request: &Prepare) -> bool {
        request.asked.is_none()
            || self
                .asked
                .as_ref()
                .is_some_and(|qualified| qualified.admits(request))
    }

    fn continuation(&self) -> Option<&Arc<ClosedRestart>> {
        self.account
            .as_ref()
            .map(AccountContinuation::proof)
            .or_else(|| self.renewal.as_ref().and_then(|seal| seal.continuation()))
    }
}

#[derive(Clone, Debug)]
enum Conversation {
    Host(String),
    #[cfg(test)]
    Facts(Arc<std::sync::atomic::AtomicBool>),
}

impl Conversation {
    fn current(&self, ctx: &crate::SessionCtx) -> bool {
        match self {
            Self::Host(expected) => current_conversation(ctx).as_ref() == Some(expected),
            #[cfg(test)]
            Self::Facts(valid) => valid.load(std::sync::atomic::Ordering::Acquire),
        }
    }
}

fn snapshot_conversation(snapshot: &aterm_agent::harness::relaunch::Snapshot) -> Option<String> {
    let session = snapshot
        .session
        .as_deref()
        .filter(|session| !session.is_empty())?;
    let root = match &snapshot.run {
        aterm_agent::harness::relaunch::AgentRun::Codex(run) => &run.home,
        _ => snapshot.claude_dir.as_ref()?,
    };
    #[cfg(unix)]
    let path = |path: &std::path::Path| {
        use std::os::unix::ffi::OsStrExt;
        aterm_codec::hex::encode_lower(path.as_os_str().as_bytes())
    };
    #[cfg(not(unix))]
    let path = |path: &std::path::Path| path.to_string_lossy().to_string();
    // This is the logical record key. Exact process birth/image and shell facts are bound
    // independently; rotating the vendor must not change the same record's identity.
    let identity = aterm_json::to_string(&vec![path(root), session.to_string()]).ok()?;
    Some(aterm_codec::hex::encode_lower(
        &aterm_digest::Sha256::digest(identity.as_bytes()),
    ))
}

fn current_conversation(ctx: &crate::SessionCtx) -> Option<String> {
    snapshot_conversation(&crate::harness_host::published_snapshot(
        ctx.self_id.as_str(),
    )?)
}

#[derive(Debug)]
struct Operation {
    terminal_acknowledged: Option<Decision>,
    request: Prepare,
    binding: Binding,
    authority: Authority,
    custody: Option<Arc<NativeCustody>>,
    phase: Phase,
    preparation: Preparation,
    grant_record: GrantRecord,
    signal_sent: bool,
    signal_accepted: Option<bool>,
    resume_sent: bool,
    resume_attempt: Option<Arc<NativeInput>>,
    human_seq: u64,
    channel: Option<u64>,
    channel_closed: bool,
    claim_outcome: ClaimOutcome,
    commit_version: Option<String>,
    completion_drain: Option<NativeDrain>,
    resume_phase: Option<ImagePhase>,
    children: [Option<LaunchChild>; 2],
    effect_started: Arc<std::sync::atomic::AtomicBool>,
    clock: Clock,
    observation: Option<Arc<crate::native_observe::Evidence>>,
    outcome_slot: Option<crate::native_observe::OutcomeSlot>,
    completed_observation: Option<aterm_agent::harness::native_observe::SavedObservation>,
    committed_image: Option<ImageOutcome>,
    source_death: Option<Arc<SharedDeath>>,
    closed_restart: Option<Arc<ClosedRestart>>,
    resume_only: bool,
    cold_facts: Option<ColdEnvironmentFacts>,
    cold_resolving: bool,
    resolved_cold: Option<Arc<ResolvedCold>>,
}

trait ColdProof: std::fmt::Debug + Send + Sync {
    fn current(&self) -> bool;
}

impl ColdProof for crate::harness_host::ColdCommandProof {
    fn current(&self) -> bool {
        crate::harness_host::ColdCommandProof::current(self)
    }
}

#[derive(Debug)]
struct ResolvedCold {
    command: RestartCommand,
    facts: ColdEnvironmentFacts,
    proof: Arc<dyn ColdProof>,
}

/// Operation-wide evidence survives child-slot changes. An uncertain append
/// never becomes absence merely because no child has received its grant yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GrantRecord {
    None,
    Writing(u64),
    Uncertain(u64),
    Recorded,
}

impl GrantRecord {
    fn begin(&mut self) -> Option<u64> {
        let attempt = match *self {
            Self::None => 1,
            Self::Uncertain(previous) => previous.checked_add(1)?,
            Self::Writing(_) | Self::Recorded => return None,
        };
        *self = Self::Writing(attempt);
        Some(attempt)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "IssuerGrant",
            action = "Ack",
            project = "native_act::tests::issuer::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "IssuerGrant",
            action = "Fail",
            project = "native_act::tests::issuer::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "IssuerGrant",
            action = "OldCleanup",
            project = "native_act::tests::issuer::transition"
        )
    )]
    fn finish(&mut self, attempt: u64, recorded: bool) -> bool {
        if *self != Self::Writing(attempt) {
            return false;
        }
        *self = if recorded {
            Self::Recorded
        } else {
            Self::Uncertain(attempt)
        };
        true
    }
}

#[derive(Debug)]
struct LaunchChild {
    source_constructor:
        Option<Arc<aterm_agent::harness::codex_source_registry::RestartReservation>>,
    process: Process,
    death: Arc<SharedDeath>,
    connection: u64,
    execute_sent: bool,
    exec_errno: Option<i32>,
}

/// Field order closes the retained descriptor before returning its passive capacity token.
#[derive(Debug)]
struct SharedDeath {
    watch: DeathWatch,
    outcome: std::sync::OnceLock<crate::native_observe::OutcomeHold>,
}

impl SharedDeath {
    fn exited(&self) -> bool {
        self.watch.exited()
    }
}

impl crate::native_observe::DeathWitness for SharedDeath {
    fn exited(&self) -> Option<bool> {
        self.watch.state()
    }
    fn outcome_hold(&self) -> Option<&crate::native_observe::OutcomeHold> {
        self.outcome.get()
    }
}

/// One kernel view drives native predicates and retained-observation predicates alike.
struct ObservationKernel<'a>(&'a dyn Kernel);
impl crate::native_observe::Kernel for ObservationKernel<'_> {
    fn process(&self, pid: u32) -> Option<Process> {
        self.0.process(pid)
    }
    fn foreground(&self, master: i32) -> Option<u32> {
        self.0.foreground(master)
    }
    fn group_gone(&self, group: u32) -> Option<bool> {
        self.0.group_gone(group)
    }
    fn watch(&self, process: &Process) -> Option<Box<dyn crate::native_observe::DeathWitness>> {
        Some(Box::new(SharedDeath {
            watch: self.0.watch(process)?,
            outcome: std::sync::OnceLock::new(),
        }))
    }
}

#[derive(Debug)]
enum DeathWatch {
    #[cfg(target_vendor = "apple")]
    Darwin(aterm_uds::exitwatch::ExitWatch),
    #[cfg(target_os = "linux")]
    Linux(std::os::fd::OwnedFd),
    #[cfg(test)]
    Facts(Process, Arc<Mutex<BTreeMap<u32, Process>>>),
    #[cfg(test)]
    Unknown,
}

#[cfg(target_vendor = "apple")]
impl aterm_uds::spawn_lock::Descriptors for DeathWatch {
    fn mark(&self) -> std::io::Result<()> {
        let raw = self
            .raw_fd()
            .ok_or_else(|| std::io::Error::other("kernel watch has no descriptor"))?;
        // SAFETY: the borrowed descriptor remains exclusively owned by this watch throughout.
        let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw) };
        aterm_uds::keep_out_of_children(&fd)
    }
}

impl DeathWatch {
    fn watch(process: &Process) -> Option<Self> {
        #[cfg(target_vendor = "apple")]
        let watch = aterm_uds::kept_out(|| {
            aterm_uds::exitwatch::ExitWatch::watch(process.pid)
                .map(Self::Darwin)
                .ok_or_else(|| std::io::Error::other("kernel exit watch unavailable"))
        })
        .ok()?;
        #[cfg(target_os = "linux")]
        let watch = Self::Linux(aterm_uds::exitwatch::exit_fd(process.pid)?);
        #[cfg(any(target_vendor = "apple", target_os = "linux"))]
        {
            if aterm_uds::process::read_birth(i32::try_from(process.pid).ok()?)?.0 != process.birth
            {
                return None;
            }
            let raw = watch.raw_fd()?;
            // SAFETY: the watch exclusively owns a live descriptor; F_GETFD only reads flags.
            let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
            (flags >= 0 && flags & libc::FD_CLOEXEC != 0).then_some(watch)
        }
        #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
        {
            let _ = process;
            None
        }
    }

    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    fn raw_fd(&self) -> Option<i32> {
        match self {
            #[cfg(target_vendor = "apple")]
            Self::Darwin(watch) => Some(watch.as_raw_fd()),
            #[cfg(target_os = "linux")]
            Self::Linux(fd) => {
                use std::os::fd::AsRawFd;
                Some(fd.as_raw_fd())
            }
            #[cfg(test)]
            Self::Facts(..) | Self::Unknown => None,
        }
    }

    /// A retained kernel watch proves this exact child exited even when exec+exit happened
    /// between process snapshots. Linux supplies no wait status; its readable pidfd is death.
    fn exited(&self) -> bool {
        self.state() == Some(true)
    }

    fn state(&self) -> Option<bool> {
        // On the place, not the reference: where no variant is compiled in (Windows), the type
        // has no value, and an empty match is exhaustive only on a place of it.
        match *self {
            #[cfg(target_vendor = "apple")]
            Self::Darwin(ref watch) => {
                if watch.exit_status().is_some() {
                    Some(true)
                } else {
                    watch_readable(watch.as_raw_fd()).and_then(|ready| (!ready).then_some(false))
                }
            }
            #[cfg(target_os = "linux")]
            Self::Linux(ref fd) => {
                use std::os::fd::AsRawFd;
                watch_readable(fd.as_raw_fd())
            }
            #[cfg(test)]
            Self::Facts(ref original, ref processes) => Some(
                processes
                    .lock()
                    .unwrap()
                    .get(&original.pid)
                    .is_none_or(|actual| actual.birth != original.birth),
            ),
            #[cfg(test)]
            Self::Unknown => None,
            // A platform with no kernel death watch has no value of this type to match; a
            // reference to it counts as inhabited, so the match names that arm.
            #[cfg(not(any(target_vendor = "apple", target_os = "linux", test)))]
            _ => None,
        }
    }
}

#[cfg(unix)]
fn watch_readable(fd: i32) -> Option<bool> {
    let mut item = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one initialized slot naming a still-owned watch descriptor, and zero timeout.
    match unsafe { libc::poll(&mut item, 1, 0) } {
        0 => Some(false),
        1 if item.revents & (libc::POLLERR | libc::POLLNVAL) == 0
            && item.revents & libc::POLLIN != 0 =>
        {
            Some(true)
        }
        _ => None,
    }
}

const fn phase_index(phase: ImagePhase) -> usize {
    match phase {
        ImagePhase::Forward => 0,
        ImagePhase::Rollback => 1,
    }
}

fn command_for(op: &Operation, phase: ImagePhase) -> Option<&RestartCommand> {
    let NativePlan::Restart {
        forward,
        rollback,
        cold,
        ..
    } = &op.request.plan
    else {
        return None;
    };
    match phase {
        ImagePhase::Forward if cold.is_some() => {
            op.resolved_cold.as_ref().map(|resolved| &resolved.command)
        }
        ImagePhase::Forward => Some(forward),
        ImagePhase::Rollback => rollback.as_deref(),
    }
}

/// Sole ownership of terminal completion side effects. Created while the operation's
/// predicate lock is still held; it cannot be cloned or minted for an already terminal op.
struct Completion {
    id: String,
    custody: Option<Arc<NativeCustody>>,
    children: [Option<LaunchChild>; 2],
    observation: Option<Arc<crate::native_observe::Evidence>>,
    outcome_capture: Option<crate::native_observe::OutcomeCapture>,
    outcome_slot: Option<crate::native_observe::OutcomeSlot>,
}

impl Operation {
    fn continuation_pending(&self) -> bool {
        !self.preparation.recorded
            || self
                .closed_restart
                .as_ref()
                .is_some_and(|proof| proof.pinned())
            || (self.resume_only
                && matches!(
                    self.phase,
                    Phase::Completed(Decision::ProvenNotCommittedRollbackSafe)
                )
                && self
                    .authority
                    .renewal
                    .as_ref()
                    .is_some_and(|seal| seal.intent_active()))
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountNoEffectClosure",
            action = "Close",
            project = "native_act::tests::account_continuation_tests::claim_model::noeffect_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountNoEffectClosure",
            action = "CohortGone",
            reason = "The retained kernel witness observes cohort drainage"
        )
    )]
    fn seal_without_effect(&mut self, kernel: &dyn Kernel) -> Option<Completion> {
        if !self.preparation.can_publish() || self.grant_record != GrantRecord::None {
            return None;
        }
        if self.resume_only && retirement(self, kernel) == Some(Retirement::NotGranted) {
            self.seal_retirement(Retirement::NotGranted, kernel)
        } else if self.authority.account.is_some() {
            // Missing cohort evidence is not an opaque terminal account outcome.
            None
        } else {
            self.seal_completion(Decision::ProvenNotCommittedRollbackSafe)
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ContinuationPhase",
            action = "RetireChild",
            project = "native_act::tests::continuation_phase::transition"
        )
    )]
    fn seal_retirement(&mut self, reason: Retirement, kernel: &dyn Kernel) -> Option<Completion> {
        if !self.preparation.can_publish()
            || (reason == Retirement::NotGranted && self.grant_record != GrantRecord::None)
        {
            return None;
        }
        if !matches!(self.phase, Phase::Completed(_)) {
            self.closed_restart = ClosedRestart::capture(self, reason, kernel).map(Arc::new);
        }
        self.seal_completion(Decision::Retired(reason))
    }

    fn seal_completion(&mut self, decision: Decision) -> Option<Completion> {
        if !self.preparation.can_publish()
            || matches!(self.grant_record, GrantRecord::Writing(_))
            || (matches!(self.grant_record, GrantRecord::Uncertain(_))
                && decision != Decision::Unknown)
        {
            return None;
        }
        if matches!(self.phase, Phase::Completed(_)) {
            return None;
        }
        self.phase = Phase::Completed(decision);
        self.completion_drain = self
            .custody
            .as_ref()
            .map(|custody| custody.drain())
            .or_else(|| self.completion_drain.take());
        Some(Completion {
            id: self.request.id.clone(),
            custody: self.custody.take(),
            children: std::mem::take(&mut self.children),
            observation: self.observation.take(),
            outcome_capture: None,
            outcome_slot: self.outcome_slot.take(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Queued,
    Started,
    Precommitted(ImagePhase),
    Committing(ImagePhase),
    Completed(Decision),
}

/// Process/image facts are injected in conformance tests; the production arm reads the
/// kernel, never a caller's holder, argv text, pathname alone or status-as-permission.
trait Kernel {
    fn source_constructor(
        &self,
        _sid: &str,
        _command: &RestartCommand,
        _child: &Process,
    ) -> Result<Option<Arc<aterm_agent::harness::codex_source_registry::RestartReservation>>, ()>
    {
        Ok(None)
    }
    fn fence_account_claim(
        &self,
        _relation: &aterm_agent::harness::native_account_wire::Relation,
    ) -> Option<account_fence::FenceRecord> {
        None
    }
    fn record_account_closure(
        &self,
        _prepare: &Prepare,
        _binding: &Binding,
        _phase: ImagePhase,
    ) -> Option<account_observation::ClosureRecord> {
        None
    }
    fn recover_account_observation(
        &self,
        _relation: &aterm_agent::harness::native_account_wire::Relation,
        _binding: &Binding,
    ) -> Option<account_observation::ClaimRecord> {
        None
    }
    fn record_prepared(&self, prepare: &Prepare, binding: &Binding) -> bool;
    fn record_account_claim(
        &self,
        _relation: &aterm_agent::harness::native_account_wire::Relation,
        _binding: &Binding,
    ) -> Option<aterm_agent::harness::native_account_wire::ChildState> {
        None
    }
    fn recover_account_claim(
        &self,
        _relation: &aterm_agent::harness::native_account_wire::Relation,
        _binding: &Binding,
    ) -> Option<aterm_agent::harness::native_account_wire::ChildState> {
        None
    }
    fn record_successor(
        &self,
        predecessor: &Prepare,
        predecessor_binding: &Binding,
        prepare: &Prepare,
        binding: &Binding,
    ) -> bool;
    fn record_may_execute(&self, prepare: &Prepare, binding: &Binding) -> bool;
    fn record_sealed(&self, prepare: &Prepare, binding: &Binding) -> bool;
    /// The private control worker calls this after sealing and draining, outside
    /// every native/GUI lock. It is never an execution or successor grant.
    fn record_noexec(&self, prepare: &Prepare, binding: &Binding, phase: ImagePhase) -> bool;
    /// Authenticated read-only history; absent, damaged or unavailable is false.
    fn historical_noexec(&self, request: &Request) -> bool;
    /// The issuer journal's lock-free reading of this exact restart prepare
    /// ([`aterm_agent::harness::native_receipts::observe_closure`]); damaged or
    /// unavailable history is an error.
    fn historical_closure(&self, prepare: &Prepare) -> Result<Closure, ()>;
    /// The established issuer custody's `custody=` digest, or `None` when none is
    /// established or it does not read.
    fn custody(&self) -> Option<String>;
    /// This host's machine and boot, or `None` when either does not read.
    fn boot(&self) -> Option<BootStamp>;
    fn recorded_operation(&self, id: &str) -> Result<bool, ()>;
    /// Commit permanent rejection only if this exact restart ID was never admitted.
    fn reject_unadmitted(&self, _prepare: &Prepare) -> Result<bool, ()> {
        Err(())
    }
    /// `None` means unsupported; an error means supported custody is unavailable.
    fn rejection_domain(&self) -> Result<Option<String>, ()> {
        Ok(None)
    }
    fn account_admission(
        &self,
        _: &aterm_agent::harness::accounts_pool::ExecutionIntent,
    ) -> Result<Option<aterm_agent::harness::accounts_pool::ExecutionGuard>, ()> {
        Err(())
    }
    fn account_execution(
        &self,
        command: &RestartCommand,
        pid: u32,
        cold: Option<&ColdEnvironmentFacts>,
    ) -> Result<Option<aterm_agent::harness::accounts_pool::ExecutionGuard>, ()>;
    fn constructor_enabled(&self, kind: Constructor) -> bool;
    fn clock(&self) -> Clock;
    fn process(&self, pid: u32) -> Option<Process>;
    fn image(&self, image: &Image) -> bool;
    fn foreground(&self, master: i32) -> Option<u32>;
    fn own_target(&self, holder: &str, target: &Image) -> bool;
    fn own_launcher(&self, image: &Image) -> bool;
    fn birth_alive(&self, process: &Process) -> Option<bool> {
        self.birth_is_alive(process.pid, process.birth)
    }
    /// A positive identity observation; unreadable is unknown, never death.
    fn birth_is_alive(&self, pid: u32, birth: u64) -> Option<bool>;
    fn signal(&self, master: i32, process: &Process, ctx: &crate::SessionCtx) -> String;
    fn watch(&self, process: &Process) -> Option<DeathWatch>;
    /// Whether `group` can no longer act ([`crate::native_observe::cohort_gone`] ships).
    fn group_gone(&self, group: u32) -> Option<bool>;
    fn conversation(&self, ctx: &crate::SessionCtx, vendor: &Process) -> Option<Conversation>;
    fn record_key(&self, ctx: &crate::SessionCtx) -> Option<String>;
    fn record_guard(&self, ctx: &crate::SessionCtx) -> Option<Conversation> {
        self.record_key(ctx).map(Conversation::Host)
    }
    fn resolve_cold(
        &self,
        _ctx: &crate::SessionCtx,
        _intent: &ColdIntent,
        _seed: &RestartCommand,
        _facts: &ColdEnvironmentFacts,
    ) -> Result<(RestartCommand, Arc<dyn ColdProof>), String> {
        Err("cold resolution is unavailable".into())
    }
    fn cold_current(&self, proof: &dyn ColdProof) -> bool {
        proof.current()
    }
    fn cold_controls_current(&self, _intent: &ColdIntent) -> bool {
        false
    }
    fn preparing(&self) {}
    fn reserved(&self) {}
    fn completion_sealed(&self) {}
}

struct System<'a>(&'a crate::native_session_policy::SessionPolicy, bool);

impl Kernel for System<'_> {
    fn source_constructor(
        &self,
        sid: &str,
        command: &RestartCommand,
        child: &Process,
    ) -> Result<Option<Arc<aterm_agent::harness::codex_source_registry::RestartReservation>>, ()>
    {
        source_restart::prepare(sid, command, child)
    }
    fn fence_account_claim(
        &self,
        relation: &aterm_agent::harness::native_account_wire::Relation,
    ) -> Option<account_fence::FenceRecord> {
        #[cfg(unix)]
        {
            use aterm_agent::harness::native_receipts;
            let record =
                native_receipts::fence_account_successor(native_receipts::AccountRelation {
                    predecessor: relation.predecessor().prepare(),
                    predecessor_binding: relation.predecessor().binding(),
                    phase: relation.phase(),
                    candidate: relation.candidate(),
                })
                .ok()?;
            (record.outcome == native_receipts::AccountOutcome::Fenced).then_some(
                account_fence::FenceRecord {
                    sequence: record.sequence,
                    recorded_ms: record.recorded_ms,
                },
            )
        }
        #[cfg(not(unix))]
        {
            let _ = relation;
            None
        }
    }
    fn record_account_closure(
        &self,
        prepare: &Prepare,
        binding: &Binding,
        phase: ImagePhase,
    ) -> Option<account_observation::ClosureRecord> {
        #[cfg(unix)]
        {
            let record =
                aterm_agent::harness::native_receipts::noexec_in_phase(prepare, binding, phase)
                    .ok()?;
            Some(account_observation::ClosureRecord {
                phase: record.phase,
                sequence: record.sequence,
                recorded_ms: record.recorded_ms,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (prepare, binding, phase);
            None
        }
    }
    fn recover_account_observation(
        &self,
        relation: &aterm_agent::harness::native_account_wire::Relation,
        binding: &Binding,
    ) -> Option<account_observation::ClaimRecord> {
        #[cfg(unix)]
        {
            use aterm_agent::harness::native_receipts;
            let record = native_receipts::recover_account_successor(
                native_receipts::AccountRelation {
                    predecessor: relation.predecessor().prepare(),
                    predecessor_binding: relation.predecessor().binding(),
                    phase: relation.phase(),
                    candidate: relation.candidate(),
                },
                binding,
            )
            .ok()??;
            Some(account_observation::ClaimRecord {
                state: account_continuation::recorded_state(record.outcome)?,
                sequence: record.sequence,
                recorded_ms: record.recorded_ms,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (relation, binding);
            None
        }
    }
    fn recover_account_claim(
        &self,
        relation: &aterm_agent::harness::native_account_wire::Relation,
        binding: &Binding,
    ) -> Option<aterm_agent::harness::native_account_wire::ChildState> {
        self.recover_account_observation(relation, binding)
            .map(|record| record.state)
    }
    fn record_account_claim(
        &self,
        relation: &aterm_agent::harness::native_account_wire::Relation,
        binding: &Binding,
    ) -> Option<aterm_agent::harness::native_account_wire::ChildState> {
        #[cfg(unix)]
        {
            use aterm_agent::harness::native_receipts;
            let observation = native_receipts::claim_account_successor(
                native_receipts::AccountRelation {
                    predecessor: relation.predecessor().prepare(),
                    predecessor_binding: relation.predecessor().binding(),
                    phase: relation.phase(),
                    candidate: relation.candidate(),
                },
                binding,
            )
            .ok()?;
            account_continuation::recorded_state(observation.outcome)
        }
        #[cfg(not(unix))]
        {
            let _ = (relation, binding);
            None
        }
    }
    fn resolve_cold(
        &self,
        _ctx: &crate::SessionCtx,
        intent: &ColdIntent,
        seed: &RestartCommand,
        facts: &ColdEnvironmentFacts,
    ) -> Result<(RestartCommand, Arc<dyn ColdProof>), String> {
        crate::harness_host::resolve_cold_command(seed, intent, facts)
            .map(|(command, proof)| (command, Arc::new(proof) as Arc<dyn ColdProof>))
    }
    fn cold_controls_current(&self, intent: &ColdIntent) -> bool {
        crate::harness_host::cold_intent_current(intent)
    }
    fn record_prepared(&self, prepare: &Prepare, binding: &Binding) -> bool {
        #[cfg(unix)]
        {
            aterm_agent::harness::native_receipts::prepared(prepare, binding).is_ok()
        }
        #[cfg(not(unix))]
        {
            let _ = (prepare, binding);
            false
        }
    }
    fn record_successor(
        &self,
        predecessor: &Prepare,
        predecessor_binding: &Binding,
        prepare: &Prepare,
        binding: &Binding,
    ) -> bool {
        #[cfg(unix)]
        {
            aterm_agent::harness::native_receipts::prepared_successor(
                predecessor,
                predecessor_binding,
                prepare,
                binding,
            )
            .is_ok()
        }
        #[cfg(not(unix))]
        {
            let _ = (predecessor, predecessor_binding, prepare, binding);
            false
        }
    }
    fn record_may_execute(&self, prepare: &Prepare, binding: &Binding) -> bool {
        #[cfg(unix)]
        {
            aterm_agent::harness::native_receipts::may_execute(prepare, binding).is_ok()
        }
        #[cfg(not(unix))]
        {
            let _ = (prepare, binding);
            false
        }
    }
    fn record_sealed(&self, prepare: &Prepare, binding: &Binding) -> bool {
        #[cfg(unix)]
        {
            aterm_agent::harness::native_receipts::seal(prepare, binding).is_ok()
        }
        #[cfg(not(unix))]
        {
            let _ = (prepare, binding);
            false
        }
    }
    fn recorded_operation(&self, id: &str) -> Result<bool, ()> {
        #[cfg(unix)]
        {
            aterm_agent::harness::native_receipts::contains_operation(id).map_err(|_| ())
        }
        #[cfg(not(unix))]
        {
            let _ = id;
            Ok(false)
        }
    }
    fn reject_unadmitted(&self, prepare: &Prepare) -> Result<bool, ()> {
        #[cfg(unix)]
        {
            aterm_agent::harness::native_receipts::reject_unadmitted(prepare).map_err(|_| ())
        }
        #[cfg(not(unix))]
        {
            let _ = prepare;
            Err(())
        }
    }
    fn rejection_domain(&self) -> Result<Option<String>, ()> {
        #[cfg(unix)]
        {
            aterm_agent::harness::native_receipts::rejection_domain()
                .map(Some)
                .map_err(|_| ())
        }
        #[cfg(not(unix))]
        {
            Ok(None)
        }
    }
    fn record_noexec(&self, prepare: &Prepare, binding: &Binding, phase: ImagePhase) -> bool {
        #[cfg(unix)]
        {
            aterm_agent::harness::native_receipts::noexec_in_phase(prepare, binding, phase).is_ok()
        }
        #[cfg(not(unix))]
        {
            let _ = (prepare, binding, phase);
            false
        }
    }
    fn historical_noexec(&self, request: &Request) -> bool {
        #[cfg(unix)]
        {
            use aterm_agent::harness::native_receipts;
            match request {
                Request::Prepare(prepare) => native_receipts::observe_prepare(prepare),
                Request::Query(binding) | Request::Reconcile(binding) => {
                    native_receipts::observe(binding)
                }
                _ => return false,
            }
            .is_ok_and(|receipt| receipt.is_some())
        }
        #[cfg(not(unix))]
        {
            let _ = request;
            false
        }
    }
    fn historical_closure(&self, prepare: &Prepare) -> Result<Closure, ()> {
        #[cfg(unix)]
        {
            aterm_agent::harness::native_receipts::observe_closure(prepare).map_err(|_| ())
        }
        #[cfg(not(unix))]
        {
            let _ = prepare;
            Err(())
        }
    }
    fn custody(&self) -> Option<String> {
        #[cfg(unix)]
        {
            aterm_agent::harness::native_receipts::custody()
                .ok()
                .flatten()
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
    fn boot(&self) -> Option<BootStamp> {
        BootStamp::current()
    }
    fn account_admission(
        &self,
        intent: &aterm_agent::harness::accounts_pool::ExecutionIntent,
    ) -> Result<Option<aterm_agent::harness::accounts_pool::ExecutionGuard>, ()> {
        use aterm_agent::harness::{accounts, accounts_pool};
        if !seat_admitted(intent) {
            return Err(());
        }
        let home = aterm_primer::home_dir().ok_or(())?;
        let roster = accounts::default_roster_path().ok_or(())?;
        let state = crate::harness_host::harness_state().ok_or(())?;
        let seat = accounts_pool::Seat {
            config_dir: roster.parent().ok_or(())?,
            state: &state,
            home: &home,
            now: aterm_time::unix_secs()
                .and_then(|now| i64::try_from(now).ok())
                .ok_or(())?,
        };
        accounts_pool::admit_execution(&seat, accounts::Cli::Claude, intent)
            .map(Some)
            .map_err(|_| ())
    }
    fn account_execution(
        &self,
        command: &RestartCommand,
        pid: u32,
        cold: Option<&ColdEnvironmentFacts>,
    ) -> Result<Option<aterm_agent::harness::accounts_pool::ExecutionGuard>, ()> {
        use aterm_agent::harness::{accounts, accounts_pool};
        if let Some(facts) = cold {
            return cold_account_execution(command, facts);
        }
        let intent = command.account_intent.as_deref().ok_or(())?;
        let args = atpkg::caller_shell::process_args(pid).ok_or(())?;
        let who = accounts::ProcessLogin::of_exec_env(&args.env).ok_or(())?;
        let home = aterm_primer::home_dir().ok_or(())?;
        let actual_home = args.env_var(accounts::HOMEDIR_ENV).ok_or(())?;
        let same_user_home = std::path::Path::new(actual_home) == home
            || std::fs::canonicalize(actual_home)
                .ok()
                .zip(std::fs::canonicalize(&home).ok())
                .is_some_and(|(actual, expected)| actual == expected);
        // Only a token route asks which entry the intent's account owns.
        let owner_entry = match who.route {
            accounts::AuthRoute::VaultToken { .. } => {
                accounts::Roster::load(&accounts::default_roster_path().ok_or(())?)
                    .ok()
                    .and_then(|roster| intent.owner_entry(&roster))
            }
            accounts::AuthRoute::Stored | accounts::AuthRoute::Outside => None,
        };
        if !intent.matches_process(&who, same_user_home, owner_entry) {
            return Err(());
        }
        if matches!(intent, accounts_pool::ExecutionIntent::External { .. }) {
            match who.route {
                accounts::AuthRoute::Outside => return Ok(None),
                accounts::AuthRoute::Stored | accounts::AuthRoute::VaultToken { .. } => {}
            }
            let mut directories = Vec::new();
            if let Some(custom) = args
                .env_var(accounts::HOME_ENV)
                .filter(|path| !path.is_empty())
            {
                directories.push(std::path::PathBuf::from(custom));
            } else if !same_user_home {
                directories.push(
                    std::path::Path::new(actual_home)
                        .join(accounts::Cli::Claude.accounts().default_home),
                );
            }
            if let Some(login) = who.login.as_deref() {
                directories.push(std::path::PathBuf::from(login));
            }
            for directory in &mut directories {
                if directory.is_relative() {
                    *directory = aterm_sysprobe::process_cwd(pid)
                        .ok_or(())?
                        .join(&*directory);
                }
            }
            let roster = accounts::default_roster_path().ok_or(())?;
            let state = crate::harness_host::harness_state().ok_or(())?;
            let seat = accounts_pool::Seat {
                config_dir: roster.parent().ok_or(())?,
                state: &state,
                home: &home,
                now: aterm_time::unix_secs()
                    .and_then(|now| i64::try_from(now).ok())
                    .ok_or(())?,
            };
            return accounts_pool::admit_external_execution(
                &seat,
                accounts::Cli::Claude,
                &directories,
            )
            .map(Some)
            .map_err(|_| ());
        }
        self.account_admission(intent)
    }
    fn constructor_enabled(&self, kind: Constructor) -> bool {
        self.0.constructor_enabled(kind)
            || (self.1
                && kind == Constructor::PrivateRestart
                && self.0.renewal_constructor_enabled())
    }
    fn clock(&self) -> Clock {
        Clock::System
    }
    fn process(&self, pid: u32) -> Option<Process> {
        Process::observe(pid)
    }

    fn image(&self, image: &Image) -> bool {
        image_exists(image)
    }
    fn own_launcher(&self, image: &Image) -> bool {
        std::env::current_exe()
            .ok()
            .and_then(|path| Image::at(&path))
            .as_ref()
            == Some(image)
    }

    fn foreground(&self, master: i32) -> Option<u32> {
        u32::try_from(crate::quit_safety::foreground_pgrp(master))
            .ok()
            .filter(|p| *p > 1)
    }

    fn own_target(&self, holder: &str, target: &Image) -> bool {
        self.0
            .holder(holder)
            .is_some_and(|(row, _)| self.0.target_image(row, target))
    }

    fn birth_is_alive(&self, pid: u32, birth: u64) -> Option<bool> {
        process_birth_alive(pid, birth)
    }
    fn signal(&self, master: i32, process: &Process, _ctx: &crate::SessionCtx) -> String {
        #[cfg(unix)]
        {
            use aterm_uds::process::EndControl;
            let Ok(pid) = i32::try_from(process.pid) else {
                return DENIED.into();
            };
            let Some(identity) = aterm_uds::process::read_identity(pid) else {
                return DENIED.into();
            };
            let mut control = aterm_uds::process::EndControlLive;
            if identity.birth.0 != process.birth
                || self.process(process.pid).as_ref() != Some(process)
                || self.foreground(master) != Some(process.group)
                || control.identity(pid) != Some(identity)
            {
                return DENIED.into();
            }
            // EndControlLive uses a pidfd on Linux, with a last identity read. Darwin's
            // documented read-to-kill race remains; no pathname/status is a process handle.
            if control.signal(&identity, libc::SIGTERM).is_ok() {
                #[cfg(any(test, debug_assertions))]
                _ctx.native_policy.renewal_signal_effect(process);
                "OK signalled\n".into()
            } else {
                "ERR native signal refused\n".into()
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (master, process);
            DENIED.into()
        }
    }
    fn watch(&self, process: &Process) -> Option<DeathWatch> {
        DeathWatch::watch(process)
    }
    fn group_gone(&self, group: u32) -> Option<bool> {
        crate::native_observe::cohort_gone(group)
    }
    fn conversation(&self, ctx: &crate::SessionCtx, vendor: &Process) -> Option<Conversation> {
        let snapshot = crate::harness_host::published_snapshot(ctx.self_id.as_str())?;
        (snapshot.pid == vendor.pid
            && snapshot.shell == vendor.parent
            && Image::at(&snapshot.program).as_ref() == Some(&vendor.image))
        .then(|| snapshot_conversation(&snapshot))
        .flatten()
        .map(Conversation::Host)
    }
    fn record_key(&self, ctx: &crate::SessionCtx) -> Option<String> {
        current_conversation(ctx)
    }
}

/// Whether an intent is one [`Kernel::account_admission`] admits through the account seat
/// (`accounts_pool::admit_execution`): a stored login, or the account's vault token, which
/// admission holds to its row's custody and its standing in use here
/// (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.2). An external route is admitted apart.
fn seat_admitted(intent: &aterm_agent::harness::accounts_pool::ExecutionIntent) -> bool {
    use aterm_agent::harness::accounts_pool::ExecutionIntent;
    match intent {
        ExecutionIntent::Stored { .. } | ExecutionIntent::Token { .. } => true,
        ExecutionIntent::External { .. } => false,
    }
}

fn cold_account_execution(
    command: &RestartCommand,
    facts: &ColdEnvironmentFacts,
) -> Result<Option<aterm_agent::harness::accounts_pool::ExecutionGuard>, ()> {
    crate::harness_host::cold_execution_guard(command, facts).map_err(|_| ())
}

#[cfg(unix)]
fn image_exists(image: &Image) -> bool {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(&image.path));
    std::fs::metadata(path).is_ok_and(|m| {
        m.is_file() && m.dev() == image.device && m.ino() == image.inode && m.mode() & 0o111 != 0
    })
}

#[cfg(not(unix))]
fn image_exists(_: &Image) -> bool {
    false
}

#[allow(
    clippy::too_many_arguments,
    reason = "Keep the distinct admission facts and existing call boundary together."
)]
fn capture_native_authority(
    ctx: &crate::SessionCtx,
    meta: &crate::session_timeline::SessionMeta,
    request: &Prepare,
    caller: &SupervisorInputCaller,
    actor: Option<Process>,
    conversation: Option<Conversation>,
    now_us: u64,
    renewal: Option<renewal::Seal>,
    account: Option<AccountContinuation>,
    kernel: &dyn Kernel,
) -> Result<Authority, String> {
    let snapshot = meta.supervisor_authority(now_us);
    if !matches!(request.duty, Scope::Upgrade | Scope::Relaunch)
        || request.revision != snapshot.revision()
        || caller.connection.is_none()
        || (renewal.is_some() && account.is_some())
        || account
            .as_ref()
            .is_some_and(|account| !account.current(ctx, request, caller, kernel))
        || renewal
            .as_ref()
            .is_some_and(|seal| !seal.admits(&snapshot, request))
    {
        return Err(DENIED.into());
    }
    let builtin = caller.principal == crate::control::Principal::Harness;
    if builtin {
        if request.holder != crate::harness_host::holder()
            || (!snapshot.builtin_keeps(&request.holder, request.duty) && renewal.is_none())
            || !matches!(request.plan, NativePlan::Restart { .. })
            || request.expected_human_seq != Some(ctx.human_input.seq())
            || actor
                .as_ref()
                .is_none_or(|actor| caller.peer != Some(actor.pid) || actor.birth == 0)
            || conversation
                .as_ref()
                .is_none_or(|guard| !guard.current(ctx))
        {
            return Err(DENIED.into());
        }
    } else if snapshot.contract() != Some(3)
        || snapshot.holder() != Some(request.holder.as_str())
        || !snapshot.scopes().is_some_and(|s| s.contains(request.duty))
        || snapshot.connection() != caller.connection
        || snapshot.identity() != caller.proof.as_ref()
        || snapshot.identity().map(|p| p.pid) != caller.peer
        || actor.as_ref().map(|p| p.pid) != caller.peer
        || !matches!(request.plan, NativePlan::OwnImage { .. })
    {
        return Err(DENIED.into());
    }
    let _ = ctx;
    Ok(Authority {
        asked: None,
        snapshot,
        caller: caller.clone(),
        builtin,
        renewal,
        account,
        actor,
        conversation,
    })
}

fn validate_plan(
    request: &Prepare,
    authority: &Authority,
    master: i32,
    kernel: &dyn Kernel,
    crash_proven: bool,
) -> bool {
    if request.asked.is_some() && !asked_upgrade::shape(request) {
        return false;
    }
    match &request.plan {
        NativePlan::OwnImage {
            child,
            target,
            version,
        } => {
            let Some(actor) = authority.actor.as_ref() else {
                return false;
            };
            // The vendor child may own an inner PTY/session. Preserve those exact child
            // facts across exec; requiring the outer supervisor's session would reject it.
            child.parent == actor.pid
                && *target != actor.image
                && kernel.process(actor.pid).is_some_and(|actual| {
                    own_actor_matches(&actual, actor) && actual.image == actor.image
                })
                && kernel.foreground(master) == Some(actor.group)
                && kernel.process(child.pid).as_ref() == Some(child)
                && kernel.image(target)
                && kernel.own_target(&request.holder, target)
                && aterm_agent::harness::contract::ClaimVersion::parse(version).is_some()
        }
        NativePlan::Restart {
            process,
            observation,
            shell,
            launcher,
            socket,
            forward,
            rollback,
            cold,
        } => {
            authority.builtin
                && cold.as_ref().is_none_or(|intent| {
                    request.duty == Scope::Relaunch
                        && authority.renewal.is_none()
                        && rollback.is_none()
                        && intent.encode().is_some()
                })
                && authority.renewal.as_ref().is_none_or(|seal| seal.current())
                && process.pid != shell.pid
                && process.group == process.pid
                && process.parent == shell.pid
                && process.session == shell.session
                && if let Some(proof) = authority.continuation() {
                    (authority.account.is_some() || proof.predecessor.plan == request.plan)
                        && proof.quiescent(kernel)
                        && kernel.foreground(master) == Some(shell.group)
                } else if observation.is_some() {
                    crash_proven && kernel.foreground(master) == Some(shell.group)
                } else {
                    kernel.foreground(master) == Some(process.group)
                        && kernel.process(process.pid).as_ref() == Some(process)
                }
                && kernel.process(shell.pid).as_ref() == Some(shell)
                && kernel.image(launcher)
                && kernel.own_launcher(launcher)
                && kernel.image(&forward.image)
                && forward.encode().is_some()
                && std::path::Path::new(socket).is_absolute()
                && rollback.as_ref().is_none_or(|back| {
                    back.image == process.image
                        && kernel.image(&back.image)
                        && back.encode().is_some()
                })
        }
    }
}

fn capability() -> Option<String> {
    let mut bytes = [0; 32];
    aterm_uds::rand::fill(&mut bytes).ok()?;
    Some(aterm_codec::hex::encode_lower(&bytes))
}

fn admitted(binding: &Binding) -> String {
    format!(
        "OK native-act state=admitted id={} cap={}\n",
        binding.id, binding.capability
    )
}

fn committed(claim: ClaimOutcome) -> String {
    let (claim, revision) = match claim {
        ClaimOutcome::Bound(revision) => ("bound", revision.to_string()),
        ClaimOutcome::Lost => ("lost", "-".to_string()),
        ClaimOutcome::Builtin => ("builtin", "-".to_string()),
    };
    format!("OK native-act state=committed claim={claim} revision={revision}\n")
}

fn cancelled(request: &Prepare, actor: &Process) -> String {
    Request::cancel_prepare(request.clone(), actor.clone())
        .encode()
        .map_or_else(
            || DENIED.into(),
            |original| {
                format!(
                    "OK native-act state=cancelled request={}\n",
                    aterm_codec::hex::encode_lower(original.as_bytes())
                )
            },
        )
}

fn cancelled_actor(
    seal: &CancelledPrepare,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> bool {
    caller.principal == seal.principal
        && caller.connection.is_some()
        && caller.peer == Some(seal.actor.pid)
        && kernel.process(seal.actor.pid).is_some_and(|actual| {
            own_actor_matches(&actual, &seal.actor) && actual.image == seal.actor.image
        })
}

/// Cancellation is not admission: an absent original needs no current claim/connection.
/// Its fresh roster-image proof, exact live owned child and kernel actor bind the no-future
/// seal; an existing original additionally binds the actor captured before admission.
fn cancel_prepare(
    ctx: &crate::SessionCtx,
    master: i32,
    cancel: CancelPrepare,
    caller: SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> String {
    let CancelPrepare {
        prepare: request,
        actor: original_actor,
    } = cancel;
    if host_epoch() != Some(request.host_epoch.as_str()) {
        return UNKNOWN.into();
    }
    let NativePlan::OwnImage { child, target, .. } = &request.plan else {
        return DENIED.into();
    };
    let Some(now) = kernel.clock().now_us() else {
        return UNKNOWN.into();
    };
    if request.deadline_us.saturating_sub(now) > MAX_PREPARE_US {
        return DENIED.into();
    }
    // Retirement may release a final Arc; keep it outside metadata and operation guards.
    let (previous, retired) = {
        let mut table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let previous = table.by_id.get(&request.id).cloned();
        (previous, table.reclaim(now))
    };
    drop(retired);
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let mut table = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if let Some(operation) = table.by_id.get(&request.id).cloned().or(previous) {
        drop(table);
        let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
        if op.request != request
            || !same_actor(&op, &caller, kernel)
            || op.authority.actor.as_ref().is_none_or(|actor| {
                !own_actor_matches(&original_actor, actor) || original_actor.image != actor.image
            })
        {
            return DENIED.into();
        }
        if matches!(op.phase, Phase::Completed(_)) {
            drop(op);
            drop(meta);
            let reply = completed_reply(&operation);
            return if reply == "OK native-act state=rollback-safe\n" {
                cancelled(&request, &original_actor)
            } else {
                reply
            };
        }
        let before_mutation = op.authority.actor.as_ref().is_some_and(|original| {
            kernel.process(original.pid).is_some_and(|actual| {
                own_actor_matches(&actual, original) && actual.image == original.image
            }) && kernel.process(child.pid).as_ref() == Some(child)
        });
        if !matches!(op.phase, Phase::Queued | Phase::Started) || !before_mutation {
            return UNKNOWN.into();
        }
        let completion = op.seal_without_effect(kernel);
        drop(op);
        drop(meta);
        kernel.completion_sealed();
        let reply = finish(ctx, &operation, completion);
        return if reply == "OK native-act state=rollback-safe\n" {
            cancelled(&request, &original_actor)
        } else {
            reply
        };
    }
    if let Some(seal) = table.cancelled.get(&request.id) {
        return if seal.request == request
            && cancelled_actor(seal, &caller, kernel)
            && own_actor_matches(&seal.actor, &original_actor)
            && seal.actor.image == original_actor.image
        {
            cancelled(&request, &original_actor)
        } else {
            DENIED.into()
        };
    }
    if original_actor.image == *target {
        return UNKNOWN.into();
    }
    let actor = caller.peer.and_then(|pid| kernel.process(pid));
    let Some(actor) = actor else {
        return UNKNOWN.into();
    };
    let rostered = ctx
        .native_policy
        .holder(&request.holder)
        .is_some_and(|(_, pid)| pid == actor.pid);
    if caller.principal == crate::control::Principal::Harness
        || caller.connection.is_none()
        || !rostered
        || caller.proof.as_ref().map(|proof| proof.pid) != Some(actor.pid)
        || actor.birth == 0
        || !own_actor_matches(&actor, &original_actor)
        || actor.image != original_actor.image
        || child.parent != actor.pid
        || kernel.process(child.pid).as_ref() != Some(child)
        || kernel.foreground(master) != Some(actor.group)
    {
        return DENIED.into();
    }
    // A pending reservation already owns this id's bounded slot. Publishing the seal
    // leaves that reservation live so neither its late worker nor pressure can erase it.
    if table.account_fences.contains_key(&request.id) {
        return UNKNOWN.into();
    }
    if !table.reservations.contains_key(&request.id) && table.slots() >= MAX_COMPLETED {
        return "ERR busy native operation history\n".into();
    }
    table.cancelled.insert(
        request.id.clone(),
        CancelledPrepare {
            request: request.clone(),
            principal: caller.principal,
            actor: original_actor.clone(),
        },
    );
    cancelled(&request, &original_actor)
}

#[allow(
    clippy::too_many_arguments,
    reason = "Both continuation authority classes are checked before predecessor persistence."
)]
fn persist_noexec_predecessor(
    ctx: &crate::SessionCtx,
    master: i32,
    request: &Prepare,
    caller: &SupervisorInputCaller,
    renewal: Option<&renewal::Seal>,
    account: Option<&AccountContinuation>,
    kernel: &dyn Kernel,
) -> Result<(), &'static str> {
    let Some(proof) = account
        .map(AccountContinuation::proof)
        .or_else(|| renewal.and_then(|seal| seal.continuation()))
    else {
        return Ok(());
    };
    // Knowing a staged candidate ID does not authorize a caller to make the
    // host write on its behalf. Check the same current authority and exact
    // continuation recipe as admission before any predecessor persistence.
    // Normal admission repeats this check after the unlocked write below.
    {
        let actor = caller.peer.and_then(|pid| kernel.process(pid));
        let conversation = kernel.record_guard(ctx);
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let authority = capture_native_authority(
            ctx,
            &meta,
            request,
            caller,
            actor,
            conversation,
            crate::metrics::now_us(),
            renewal.cloned(),
            account.cloned(),
            kernel,
        )
        .map_err(|_| DENIED)?;
        if !validate_plan(request, &authority, master, kernel, false) {
            return Err(DENIED);
        }
    }
    let _flight = ReceiptFlight::begin(ctx, &Request::prepare(proof.predecessor.clone()))
        .map_err(|()| UNKNOWN)?;
    let previous = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .by_id
        .get(&proof.predecessor.id)
        .cloned()
        .ok_or(UNKNOWN)?;
    let record = {
        let op = previous.lock().unwrap_or_else(|p| p.into_inner());
        if op.phase != Phase::Completed(Decision::Retired(Retirement::NotGranted)) {
            return Ok(());
        }
        if op.request != proof.predecessor
            || op.grant_record != GrantRecord::None
            || !op
                .closed_restart
                .as_ref()
                .is_some_and(|actual| Arc::ptr_eq(actual, proof))
            || !proof.available()
            || !proof.quiescent(kernel)
            || op
                .completion_drain
                .as_ref()
                .is_some_and(|drain| !drain.is_drained())
        {
            return Err(UNKNOWN);
        }
        if op.resume_phase.unwrap_or(ImagePhase::Forward) != proof.closed_phase() {
            return Err(UNKNOWN);
        }
        (op.request.clone(), op.binding.clone(), proof.closed_phase())
    };
    // A sweep may have closed the operation without a control response. Finish
    // publishing its positive NoExec before consuming the in-memory successor
    // slot. A failed write leaves that slot available for an exact retry.
    let _handoff = aterm_types::update_boundary::Boundary::process()
        .try_hold(aterm_types::update_boundary::GuardedItem::HarnessNativeMutation)
        .ok_or(UNKNOWN)?;
    if kernel.record_noexec(&record.0, &record.1, record.2) {
        Ok(())
    } else {
        Err(UNKNOWN)
    }
}

fn prepare(
    ctx: &crate::SessionCtx,
    master: i32,
    request: Prepare,
    caller: SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> String {
    prepare_inner(ctx, master, request, caller, kernel, None)
}

/// The answer to a prepare whose deadline has passed: `unadmitted` when this host proves it
/// never admitted this prepare and can admit no replay of it, and otherwise today's deadline
/// refusal. It writes, reserves and reclaims nothing.
///
/// The proof is read for a plain or supported typed asked restart from the built-in harness,
/// on this host epoch (checked before this), with no renewal seal or account relation:
/// - the deadline was read on this epoch's monotonic clock, which every admission reads too,
///   and nothing publishes once it has passed (the final check under the table lock);
/// - admission writes the issuer record and has it acknowledged before `Phase::Started`
///   (`finish_preparation`), and every effect requires `Started` and an unexpired deadline;
/// - nothing leaves the table unrecorded, and the issuer journal only appends.
///
/// So, read after the clock, an id no table entry, reservation, seal or fence holds and no
/// issuer record names was never started, and no later replay of this prepare can start it (a
/// prepare naming the id with a fresh deadline is a new admission). A self-exec plan
/// writes no issuer record; renewal seals and account continuations keep the refusal.
/// A supported asked predecessor changes only at accepted publication, which leaves an
/// unrecorded table entry. Absence grants no predecessor authority: a replacement must
/// qualify its current predecessor and producer anew. Rollback of the whole custody tree
/// stays outside this claim, as it does
/// for every journal read (`native_receipts`).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeUnadmittedPrepare",
        action = "Answer",
        project = "aterm_gui::native_act::tests::unadmitted::Host::probe"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUnadmittedPrepare",
        action = "Approach",
        reason = "The injected monotonic clock brings a too-far deadline into its window"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUnadmittedPrepare",
        action = "Expire",
        reason = "The injected monotonic clock passes the deadline"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUnadmittedPrepare",
        action = "OwnImage",
        reason = "The caller's plan names the self-exec constructor, which writes no issuer record"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUnadmittedPrepare",
        action = "LostAck",
        reason = "A committed issuer append can lose its acknowledgement without informing the writer"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUnadmittedPrepare",
        action = "Ask",
        reason = "The incoming recipe selects supported typed asked data without qualifying its predecessor or producer"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUnadmittedPrepare",
        action = "UnsupportedAsk",
        reason = "The incoming recipe selects unsupported asked data that cannot satisfy the answer's shape premise"
    )
)]
fn unadmitted_after_deadline(
    ctx: &crate::SessionCtx,
    request: &Prepare,
    caller: &SupervisorInputCaller,
    renewal: Option<&renewal::Seal>,
    account: Option<&AccountContinuation>,
    kernel: &dyn Kernel,
) -> String {
    if !unadmitted_guard(ctx, request, caller, renewal, account)
        || kernel.recorded_operation(&request.id) != Ok(false)
    {
        return DEADLINE.into();
    }
    aterm_agent::harness::native_act::unadmitted_reply(&request.id)
        .unwrap_or_else(|| DEADLINE.into())
}

fn unadmitted_guard(
    ctx: &crate::SessionCtx,
    request: &Prepare,
    caller: &SupervisorInputCaller,
    renewal: Option<&renewal::Seal>,
    account: Option<&AccountContinuation>,
) -> bool {
    if caller.principal != crate::control::Principal::Harness
        || renewal.is_some()
        || account.is_some()
        || (request.asked.is_some() && !asked_upgrade::shape(request))
        || !matches!(request.plan, NativePlan::Restart { .. })
    {
        return false;
    }
    let held = {
        let table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        table.by_id.contains_key(&request.id)
            || table.reservations.contains_key(&request.id)
            || table.cancelled.contains_key(&request.id)
            || table.account_fences.contains_key(&request.id)
            || table.renewal_seals.contains_key(&request.id)
    };
    !held
}

fn prepare_inner(
    ctx: &crate::SessionCtx,
    master: i32,
    request: Prepare,
    caller: SupervisorInputCaller,
    kernel: &dyn Kernel,
    account: Option<AccountContinuation>,
) -> String {
    // Recognize complete-recipient recipes for historical reads, but do not admit
    // their writer until cross-image compatibility and namespace ownership are proven.
    if request.receipt_mode.is_some() {
        return RECIPIENT_WRITER_UNAVAILABLE.into();
    }
    let renewal = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .renewal_seals
        .get(&request.id)
        .cloned();
    if renewal
        .as_ref()
        .is_some_and(|seal| !seal.bound_to_weak(ctx))
    {
        return DENIED.into();
    }

    if host_epoch() != Some(request.host_epoch.as_str()) {
        // The old epoch's clock cannot prove absence. A marked recipe is rejected
        // by pre-journal hosts. Its acknowledged durable rejection races admission
        // under the same journal lock and only in its original domain. If
        // preparation won, this refuses; if rejection won, no old host can
        // publish admission later. This never reconstructs execution authority.
        if request.rejection_domain.is_some()
            && unadmitted_guard(ctx, &request, &caller, renewal.as_ref(), account.as_ref())
            && kernel.reject_unadmitted(&request) == Ok(true)
        {
            return aterm_agent::harness::native_act::unadmitted_reply(&request.id)
                .unwrap_or_else(|| "ERR native host epoch changed\n".into());
        }
        return "ERR native host epoch changed\n".into();
    }
    if let Some(expected) = &request.rejection_domain
        && kernel.rejection_domain().ok().flatten().as_ref() != Some(expected)
    {
        return DENIED.into();
    }
    {
        let table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(seal) = table.cancelled.get(&request.id) {
            return if seal.request == request && cancelled_actor(seal, &caller, kernel) {
                cancelled(&request, &seal.actor)
            } else {
                DENIED.into()
            };
        }
    }
    let previous = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .by_id
        .get(&request.id)
        .cloned();
    if let Some(previous) = previous {
        let op = previous.lock().unwrap_or_else(|p| p.into_inner());
        let same_relation = match (&op.authority.account, &account) {
            (None, None) => true,
            (Some(a), Some(b)) => a.same_relation(b),
            _ => false,
        };
        return if op.request == request && same_relation && same_actor(&op, &caller, kernel) {
            if op.phase == Phase::Queued || !op.preparation.recorded {
                drop(op);
                return retry_preparation(ctx, master, previous, &caller, kernel, true);
            }
            match op.phase {
                Phase::Started | Phase::Precommitted(_) => admitted(&op.binding),
                Phase::Committing(_) => UNKNOWN.into(),
                Phase::Completed(_) => {
                    drop(op);
                    completed_reply(&previous)
                }
                Phase::Queued => preparation::preparation_pending_reply(&op),
            }
        } else {
            DENIED.into()
        };
    }
    // Existing exact operations above are observations/reconnections. Their
    // recorded Signal may have ended the original process; only a genuinely
    // new preparation requires a fresh live-source proof. Resume/Execute gate
    // all later mutations against current policy independently.
    if renewal.as_ref().is_some_and(|seal| !seal.preflight()) {
        return DENIED.into();
    }
    if !kernel.constructor_enabled(request.constructor()) {
        return "ERR native constructor disabled\n".into();
    }
    let clock = kernel.clock();
    let Some(now) = clock.now_us() else {
        return DENIED.into();
    };
    match request.deadline_us.checked_sub(now) {
        // An expired deadline never admits again; a too-far one may once time passes.
        None | Some(0) => {
            return unadmitted_after_deadline(
                ctx,
                &request,
                &caller,
                renewal.as_ref(),
                account.as_ref(),
                kernel,
            );
        }
        Some(left) if left > MAX_PREPARE_US => return DEADLINE.into(),
        Some(_) => {}
    }
    // A stamp is a fact this host vouches for by admitting it: its own machine and boot, its
    // own established custody. A historical `lapsed` answer relies on that (`historical_prepare`).
    if request
        .boot
        .as_ref()
        .is_some_and(|stamp| kernel.boot().as_ref() != Some(stamp))
        || request
            .custody
            .as_ref()
            .is_some_and(|stamp| kernel.custody().as_ref() != Some(stamp))
    {
        return DENIED.into();
    }
    if let Err(refusal) = persist_noexec_predecessor(
        ctx,
        master,
        &request,
        &caller,
        renewal.as_ref(),
        account.as_ref(),
        kernel,
    ) {
        return refusal.into();
    }
    let crash = matches!(
        &request.plan,
        NativePlan::Restart {
            observation: Some(_),
            ..
        }
    );
    let account_slot =
        AccountSlot::for_request(&request, &caller, renewal.is_some(), account.as_ref());
    let mut slot = match PrepareSlot::reserve_relation(
        ctx,
        &request.id,
        now,
        crash,
        account_slot,
        account.as_ref().map(AccountContinuation::relation),
    ) {
        Ok(slot) => slot,
        Err(refusal) => return refusal,
    };
    // Reserve the ID first, then read durable history outside all native/GUI
    // locks. An in-flight receipt cannot be reclaimed; an acknowledged one is
    // here forever, even after its in-memory operation was reclaimed.
    if matches!(request.plan, NativePlan::Restart { .. }) {
        match kernel.recorded_operation(&request.id) {
            Ok(false) => {}
            Ok(true) => return DENIED.into(),
            Err(()) => return UNKNOWN.into(),
        }
    }
    let Some(capability) = capability() else {
        return "ERR native entropy unavailable\n".into();
    };
    kernel.preparing();
    // Arm the original process while its complete identity still matches. The
    // later plan validation brackets this kernel watch before any Signal can
    // be admitted; a disk journal or a later missing PID cannot replace it.
    let resume_only = account.is_some()
        || renewal
            .as_ref()
            .is_some_and(|seal| seal.continuation().is_some());
    let source_death = if let Some(proof) = account
        .as_ref()
        .map(AccountContinuation::proof)
        .or_else(|| renewal.as_ref().and_then(|seal| seal.continuation()))
    {
        Some(proof.source_watch())
    } else {
        match &request.plan {
            NativePlan::Restart {
                process,
                observation: None,
                ..
            } => {
                if kernel.process(process.pid).as_ref() != Some(process) {
                    return DENIED.into();
                }
                let Some(watch) = kernel.watch(process) else {
                    return UNKNOWN.into();
                };
                Some(Arc::new(SharedDeath {
                    watch,
                    outcome: std::sync::OnceLock::new(),
                }))
            }
            _ => None,
        }
    };
    let observations = observe_control::store(ctx);
    // Every watch/slot carrier precedes metadata guards, so refusal disposal stays outside.
    let mut observation = None;
    let outcome_slot = if matches!(request.plan, NativePlan::Restart { .. }) {
        let initial = {
            let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            let Some(context) = observe_control::context(ctx, &meta, &caller, kernel) else {
                return DENIED.into();
            };
            if let NativePlan::Restart {
                observation: Some(binding),
                ..
            } = &request.plan
            {
                observation = observations
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .prove_with(binding, &context, master, now, &ObservationKernel(kernel));
                if observation.is_none() {
                    return UNKNOWN.into();
                }
            }
            context
        };
        let (mut reserved, retired) = observations
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .reserve_outcome(&initial, now)
            .into_parts();
        let reclaimed = !retired.is_empty();
        drop(retired);
        if reserved.is_none() && reclaimed {
            let (next, retired) = observations
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .reserve_outcome(&initial, now)
                .into_parts();
            drop(retired);
            reserved = next;
        }
        let Some(reserved) = reserved else {
            return "ERR busy native passive outcome capacity\n".into();
        };
        Some(reserved)
    } else {
        None
    };
    let actor = caller.peer.and_then(|pid| kernel.process(pid));
    let conversation = if resume_only {
        kernel.record_guard(ctx)
    } else {
        match &request.plan {
            NativePlan::Restart {
                observation: Some(_),
                ..
            } => kernel.record_guard(ctx),
            NativePlan::Restart { process, .. } => kernel.conversation(ctx, process),
            NativePlan::OwnImage { .. } => None,
        }
    };
    // Declared before Meta so refused passive proof cannot close its watch under a guard.
    let account_guard = match account
        .as_ref()
        .map(|account| account.admit(kernel))
        .transpose()
    {
        Ok(guard) => guard,
        Err(()) => return DENIED.into(),
    };
    // A fresh asked move must not retire the current vendor on stale account authority.
    // Acquire outside Meta/native predicates and retain through accepted publication.
    let _asked_account_guard = if request.asked.is_some() {
        let NativePlan::Restart {
            process, forward, ..
        } = &request.plan
        else {
            return DENIED.into();
        };
        match kernel.account_execution(forward, process.pid, None) {
            Ok(guard) => guard,
            Err(()) => return DENIED.into(),
        }
    } else {
        None
    };
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let mut authority = match capture_native_authority(
        ctx,
        &meta,
        &request,
        &caller,
        actor,
        conversation,
        crate::metrics::now_us(),
        renewal,
        account,
        kernel,
    ) {
        Ok(authority) => authority,
        Err(refusal) => return refusal,
    };
    if let NativePlan::Restart {
        process,
        shell,
        observation: Some(binding),
        ..
    } = &request.plan
    {
        let Some(context) = observe_control::context(ctx, &meta, &caller, kernel) else {
            return DENIED.into();
        };
        if observation.as_ref().is_none_or(|proof| {
            proof.vendor() != process
                || proof.shell() != shell
                || proof.binding() != binding
                || !proof.reprove_with(&context, master, now, &ObservationKernel(kernel))
        }) {
            return UNKNOWN.into();
        }
    }
    if !validate_plan(&request, &authority, master, kernel, observation.is_some()) {
        return DENIED.into();
    }
    // Repeated prepare never renews a deadline or allocates another operation. A request
    // differing in any authority/plan field cannot reuse the original idempotency key.
    // Final cancellation/deadline checks, custody reservation and insertion share this
    // Meta -> native-table boundary. A worker delayed before it cannot publish past a seal.
    let mut table = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if let Some(seal) = table.cancelled.get(&request.id) {
        return if seal.request == request && cancelled_actor(seal, &caller, kernel) {
            cancelled(&request, &seal.actor)
        } else {
            DENIED.into()
        };
    }
    if clock.remaining(request.deadline_us).is_none() {
        return DEADLINE.into();
    }
    if !kernel.constructor_enabled(request.constructor()) {
        return "ERR native constructor disabled\n".into();
    }
    if table.by_id.contains_key(&request.id) {
        return "ERR busy native operation prepare\n".into();
    }
    if authority
        .renewal
        .as_ref()
        .is_some_and(|seal| !seal.intent_active())
    {
        return DENIED.into();
    }
    if request.asked.is_some() {
        let qualified = match table.qualify_asked(ctx, &request, &authority, master, kernel) {
            Ok(qualified) => qualified,
            Err(reason) => {
                drop(table);
                drop(meta);
                drop(account_guard);
                drop(_asked_account_guard);
                aterm_log::debug!("asked native admission refused stage={reason}");
                return DENIED.into();
            }
        };
        authority.asked = Some(qualified);
    }
    let continuation = authority.continuation().cloned();
    let mut noexec_predecessor = None;
    if let Some(proof) = &continuation {
        let Some(previous) = table.by_id.get(&proof.predecessor.id).cloned() else {
            return UNKNOWN.into();
        };
        let previous = previous.lock().unwrap_or_else(|p| p.into_inner());
        if authority.account.is_some()
            && (previous.phase != Phase::Completed(Decision::Retired(Retirement::NotGranted))
                || previous.grant_record != GrantRecord::None
                || !proof.noexec())
        {
            return DENIED.into();
        }
        if !matches!(previous.phase, Phase::Completed(Decision::Retired(_)))
            && !(previous.resume_only
                && previous.phase == Phase::Completed(Decision::ProvenNotCommittedRollbackSafe))
        {
            return UNKNOWN.into();
        }
        if previous.request != proof.predecessor
            || !previous
                .closed_restart
                .as_ref()
                .is_some_and(|actual| Arc::ptr_eq(actual, proof))
            || !proof.available()
            || !proof.quiescent(kernel)
        {
            return DENIED.into();
        }
        // Only this terminal class has a durable NoExec receipt. A rollback-safe
        // continuation and an exited child retain their separate live-evidence
        // path; an opaque journal seal must never be relabeled by a later read.
        if previous.phase == Phase::Completed(Decision::Retired(Retirement::NotGranted))
            && proof.noexec()
        {
            noexec_predecessor = Some((previous.request.clone(), previous.binding.clone()));
        }
    }
    let Some(boundary) = aterm_types::update_boundary::Boundary::process()
        .try_hold(aterm_types::update_boundary::GuardedItem::HarnessNativeMutation)
    else {
        return "ERR busy app handoff\n".into();
    };
    let custody = match ctx
        .fenced_writes
        .reserve_native(request.revision, &request.id, boundary)
    {
        Ok(custody) => custody,
        Err(refusal) => return refusal,
    };
    let binding = Binding {
        id: request.id.clone(),
        capability,
    };
    let custody = Arc::new(custody);
    if continuation.as_ref().is_some_and(|proof| {
        if authority.account.is_some() {
            !proof.reserve_account_successor(&request)
        } else {
            !proof.claim_successor(&request)
        }
    }) {
        drop(table);
        drop(meta);
        drop(custody);
        return UNKNOWN.into();
    }
    let actor_history = authority
        .actor
        .as_ref()
        .map(|actor| (actor.pid, actor.birth));
    let claim_outcome = if authority.builtin {
        ClaimOutcome::Builtin
    } else {
        ClaimOutcome::Lost
    };
    let human_seq = request
        .expected_human_seq
        .unwrap_or_else(|| ctx.human_input.seq());
    let preparation = Preparation::new(
        matches!(request.plan, NativePlan::Restart { .. }),
        noexec_predecessor,
    );
    let operation = Arc::new(Mutex::new(Operation {
        terminal_acknowledged: None,
        request,
        binding,
        authority,
        custody: Some(Arc::clone(&custody)),
        phase: Phase::Queued,
        preparation,
        grant_record: GrantRecord::None,
        signal_sent: false,
        signal_accepted: None,
        resume_sent: false,
        resume_attempt: None,
        human_seq,
        channel: caller.connection,
        channel_closed: false,
        claim_outcome,
        commit_version: None,
        completion_drain: None,
        resume_phase: continuation
            .as_ref()
            .and_then(|proof| proof.initial_resume_phase()),
        children: [None, None],
        effect_started: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        clock,
        observation,
        outcome_slot,
        completed_observation: None,
        committed_image: None,
        source_death,
        closed_restart: None,
        resume_only,
        cold_facts: None,
        cold_resolving: false,
        resolved_cold: None,
    }));
    if !slot.publish(&mut table, Arc::clone(&operation)) {
        drop(table);
        drop(meta);
        drop(operation);
        return DENIED.into();
    }
    {
        if let Some(connection) = caller.connection {
            table.coordinated_connections.insert(connection);
        }
        if caller.principal == crate::control::Principal::Harness {
            table.builtin_coordinated = true;
        } else if let Some(identity) = actor_history
            && !table.coordinated_actors.contains(&identity)
        {
            table.coordinated_actors.push(identity);
        }
    }
    drop(table);
    drop(meta);
    drop(account_guard);
    drop(_asked_account_guard);
    let attempt = Preparing {
        operation,
        serial: 1,
        initial_custody: Some(custody),
    };
    kernel.reserved();
    finish_preparation(ctx, master, attempt, &caller, kernel, true)
}

fn lookup(ctx: &crate::SessionCtx, binding: &Binding) -> Option<Arc<Mutex<Operation>>> {
    let op = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .by_id
        .get(&binding.id)?
        .clone();
    let matches = op.lock().unwrap_or_else(|p| p.into_inner()).binding == *binding;
    matches.then_some(op)
}

fn same_actor(op: &Operation, caller: &SupervisorInputCaller, kernel: &dyn Kernel) -> bool {
    if op.authority.builtin {
        caller.principal == crate::control::Principal::Harness
            && caller.peer == op.authority.caller.peer
            && op.authority.actor.as_ref().is_some_and(|old| {
                kernel
                    .process(old.pid)
                    .is_some_and(|now| own_actor_matches(&now, old) && now.image == old.image)
            })
    } else {
        caller.principal == op.authority.caller.principal
            && caller.peer == op.authority.caller.peer
            && op.authority.actor.as_ref().is_some_and(|old| {
                kernel
                    .process(old.pid)
                    .is_some_and(|now| now.pid == old.pid && now.birth == old.birth)
            })
    }
}

/// Data from the same positive kernel evaluation that permits completion. It owns no
/// watch or mutation capability and does not prove subsequent process/policy liveness.
#[derive(Debug)]
struct ImageOutcome {
    process: Process,
}

fn image_outcome(op: &Operation, master: i32, phase: ImagePhase, kernel: &dyn Kernel) -> bool {
    observe_image_outcome(op, master, phase, kernel).is_ok()
}

fn observe_image_outcome(
    op: &Operation,
    master: i32,
    phase: ImagePhase,
    kernel: &dyn Kernel,
) -> Result<ImageOutcome, &'static str> {
    if let NativePlan::Restart { process, shell, .. } = &op.request.plan {
        if !op.signal_sent && op.observation.is_none() && !op.resume_only {
            return Err("restart-source-unissued");
        }
        if !op.resume_sent {
            return Err("restart-resume-unissued");
        }
        if op.resume_phase != Some(phase) {
            return Err("restart-resume-phase");
        }
        match kernel.birth_alive(process) {
            Some(false) => {}
            Some(true) => return Err("restart-source-alive"),
            None => return Err("restart-source-unread"),
        }
        let Some(child) = op.children[phase_index(phase)].as_ref() else {
            return Err("restart-child-absent");
        };
        let Some(command) = command_for(op, phase) else {
            return Err("restart-command-absent");
        };
        let Some(actual) = kernel.process(child.process.pid) else {
            return Err("restart-child-unread");
        };
        if !child.execute_sent {
            return Err("restart-execute-unissued");
        }
        if actual.pid != child.process.pid {
            return Err("restart-child-pid");
        }
        if actual.birth != child.process.birth {
            return Err("restart-child-birth");
        }
        if actual.parent != shell.pid {
            return Err("restart-child-parent");
        }
        if actual.group != child.process.group {
            return Err("restart-child-group");
        }
        if actual.session != child.process.session {
            return Err("restart-child-session");
        }
        if actual.image != *command.settled() {
            return Err("restart-child-image");
        }
        if kernel.process(shell.pid).as_ref() != Some(shell) {
            return Err("restart-shell");
        }
        if kernel.foreground(master) != Some(actual.group) {
            return Err("restart-foreground");
        }
        return Ok(ImageOutcome { process: actual });
    }
    let NativePlan::OwnImage { child, target, .. } = &op.request.plan else {
        return Err("own-plan");
    };
    let Some(original) = op.authority.actor.as_ref() else {
        return Err("own-actor-absent");
    };
    let Some(actual) = kernel.process(original.pid) else {
        return Err("own-actor-unread");
    };
    let image = match phase {
        ImagePhase::Forward => target,
        ImagePhase::Rollback => &original.image,
    };
    if !(own_actor_matches(&actual, original)
        && actual.image == *image
        && kernel.foreground(master) == Some(original.group)
        && kernel.process(child.pid).as_ref() == Some(child))
    {
        return Err("own-image-or-child");
    }
    Ok(ImageOutcome { process: actual })
}

fn own_actor_matches(actual: &Process, original: &Process) -> bool {
    // The outer parent is diagnostic and may change after a queued HUP/reparent. The owned
    // vendor child still binds its parent to this same supervisor PID independently.
    actual.pid == original.pid
        && actual.birth == original.birth
        && actual.group == original.group
        && actual.session == original.session
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeUnadmittedPrepare",
        action = "Complete",
        project = "aterm_gui::native_act::tests::unadmitted::Host::probe"
    )
)]
fn finish(
    ctx: &crate::SessionCtx,
    operation: &Arc<Mutex<Operation>>,
    completion: Option<Completion>,
) -> String {
    let Some(Completion {
        id,
        custody,
        children,
        observation,
        outcome_capture,
        outcome_slot,
    }) = completion
    else {
        return completed_reply(operation);
    };
    // Releasing ordering custody happens after kernel outcome, not reply delivery or EOF.
    drop(children);
    drop(observation);
    drop(outcome_capture);
    drop(outcome_slot);
    drop(custody);
    let mut table = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    table.completed.push_back(id);
    drop(table);
    completed_reply(operation)
}

#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "TerminalPublication",
        action = "Capture",
        project = "aterm_gui::native_act::tests::publication::transition"
    )
)]
fn completed_reply(operation: &Arc<Mutex<Operation>>) -> String {
    let op = operation.lock().unwrap_or_else(|p| p.into_inner());
    let phase = op.phase;
    let claim = op.claim_outcome;
    let drain = op.completion_drain.clone();
    let observation = op.completed_observation.clone();
    let cold = matches!(op.request.plan, NativePlan::Restart { cold: Some(_), .. });
    let account_intent = cold
        .then(|| {
            op.resolved_cold
                .as_ref()
                .and_then(|resolved| resolved.command.account_intent.as_deref())
                .cloned()
        })
        .flatten();
    drop(op);
    if drain.is_some_and(|drain| !drain.is_drained()) {
        return UNKNOWN.into();
    }
    match phase {
        Phase::Completed(Decision::Committed) => {
            if cold {
                let Some(intent) = account_intent else {
                    return UNKNOWN.into();
                };
                let Ok(encoded_intent) = aterm_json::to_string(&intent.to_value()) else {
                    return UNKNOWN.into();
                };
                let mut reply = format!(
                    "{} account-intent={}",
                    committed(claim).trim_end(),
                    aterm_codec::hex::encode_lower(encoded_intent.as_bytes())
                );
                if let Some(encoded) = observation.and_then(|observation| observation.encode()) {
                    reply.push_str(&format!(
                        " observation={}",
                        aterm_codec::hex::encode_lower(encoded.as_bytes())
                    ));
                }
                reply.push('\n');
                return reply;
            }
            if claim == ClaimOutcome::Builtin
                && let Some(encoded) = observation.and_then(|observation| observation.encode())
            {
                format!(
                    "OK native-act state=committed claim=builtin revision=- observation={}\n",
                    aterm_codec::hex::encode_lower(encoded.as_bytes())
                )
            } else {
                committed(claim)
            }
        }
        Phase::Completed(Decision::ProvenNotCommittedRollbackSafe) => ROLLBACK_SAFE.into(),
        Phase::Completed(Decision::Retired(reason)) => {
            format!("OK native-act state=retired reason={}\n", reason.word())
        }
        _ => UNKNOWN.into(),
    }
}

/// Publish passive provenance before releasing the native marker. Readers therefore cannot
/// observe a terminal receipt before its data-only handoff is installed. No GUI lane calls
/// this function: only the private control worker's positive kernel completion can own it.
fn finish_restart(
    ctx: &crate::SessionCtx,
    master: i32,
    operation: &Arc<Mutex<Operation>>,
    mut completion: Option<Completion>,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> String {
    let mut capture = completion
        .as_mut()
        .and_then(|completion| completion.outcome_capture.take());
    if capture.is_some() {
        let observations = observe_control::store(ctx);
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let mut capture_stage = "finish-context";
        let published = if let Some(context) = observe_control::context(ctx, &meta, caller, kernel)
        {
            capture_stage = "finish-publication";
            let (published, retired) = observations
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .publish_outcome_with(
                    capture.take().expect("captured outcome"),
                    &context,
                    master,
                    kernel.clock().now_us().unwrap_or(u64::MAX),
                    &ObservationKernel(kernel),
                )
                .into_parts();
            drop(meta);
            drop(retired);
            published
        } else {
            drop(meta);
            None
        };
        if let Some(published) = published {
            operation
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .completed_observation =
                Some(aterm_agent::harness::native_observe::SavedObservation {
                    registration: published.registration,
                    binding: published.binding,
                });
            capture_stage = "finish-published";
        }
        aterm_log::debug!("native completion observation stage={capture_stage}");
    }
    drop(capture);
    finish(ctx, operation, completion)
}

fn registered_launcher(
    _ctx: &crate::SessionCtx,
    master: i32,
    op: &Operation,
    phase: ImagePhase,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> bool {
    let NativePlan::Restart {
        shell, launcher, ..
    } = &op.request.plan
    else {
        return false;
    };
    let Some(actual) = caller.peer.and_then(|pid| kernel.process(pid)) else {
        return false;
    };
    caller.principal == crate::control::Principal::Instance
        && op.authority.builtin
        && op.phase == Phase::Started
        && op.resume_phase == Some(phase)
        && op.resume_sent
        && actual.parent == shell.pid
        && actual.group == actual.pid
        && actual.session == shell.session
        && actual.image == *launcher
        && kernel.process(shell.pid).as_ref() == Some(shell)
        && kernel.foreground(master) == Some(actual.group)
        && op.children[phase_index(phase)]
            .as_ref()
            .is_some_and(|child| {
                child.process == actual && Some(child.connection) == caller.connection
            })
}

fn launcher_guards_current(
    ctx: &crate::SessionCtx,
    op: &Operation,
    authority_current: bool,
    kernel: &dyn Kernel,
) -> bool {
    op.clock.remaining(op.request.deadline_us).is_some()
        && op.authority.asked_matches(&op.request)
        && kernel.constructor_enabled(op.request.constructor())
        && op.authority.account.as_ref().is_none_or(|account| {
            account.current(ctx, &op.request, &op.authority.caller, kernel)
                && account.proof().claimed_by(&op.request)
        })
        && op
            .authority
            .renewal
            .as_ref()
            .is_none_or(|seal| seal.current())
        && ctx.human_input.seq() == op.human_seq
        && op
            .authority
            .conversation
            .as_ref()
            .is_some_and(|guard| guard.current(ctx))
        && (op.effect_started.load(std::sync::atomic::Ordering::Acquire) || authority_current)
        && match &op.request.plan {
            NativePlan::Restart {
                cold: Some(intent), ..
            } => kernel.cold_controls_current(intent),
            _ => true,
        }
}

fn resolved_reply(command: &RestartCommand, deadline_us: u64) -> String {
    command.encode().map_or_else(
        || DENIED.into(),
        |encoded| {
            format!(
                "OK native-act state=resolved-launch deadline-us={deadline_us} command={}\n",
                aterm_codec::hex::encode_lower(encoded.as_bytes())
            )
        },
    )
}

fn serve_launcher(
    ctx: &Arc<crate::SessionCtx>,
    master: i32,
    request: &Request,
    caller: &SupervisorInputCaller,
    operation: &Arc<Mutex<Operation>>,
    kernel: &dyn Kernel,
) -> String {
    if !matches!(request, Request::Execute(..)) {
        return serve_launcher_step(
            ctx,
            master,
            request,
            caller,
            operation,
            kernel,
            GrantStep::Other,
        );
    }
    let reply = serve_launcher_step(
        ctx,
        master,
        request,
        caller,
        operation,
        kernel,
        GrantStep::Reserve,
    );
    if reply != JOURNAL_GRANT {
        return reply;
    }
    let (prepare, binding, handoff, attempt) = {
        let op = operation.lock().unwrap_or_else(|p| p.into_inner());
        let GrantRecord::Writing(attempt) = op.grant_record else {
            return UNKNOWN.into();
        };
        (
            op.request.clone(),
            op.binding.clone(),
            op.custody.as_ref().map(|custody| custody.handoff_guard()),
            attempt,
        )
    };
    let _writing = GrantWriting {
        operation: Arc::clone(operation),
        attempt,
    };
    // The precheck reserved Writing under the operation lock. Retirement and
    // child replacement cannot mistake this unlocked append for an unused grant.
    let recorded = handoff.is_some() && kernel.record_may_execute(&prepare, &binding);
    {
        let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
        if !op.grant_record.finish(attempt, recorded) {
            return UNKNOWN.into();
        }
    }
    if !recorded {
        return UNKNOWN.into();
    }
    // This reacquires the account guard and repeats every kernel, conversation,
    // image, authority and deadline check. No pre-append permission is cached.
    serve_launcher_step(
        ctx,
        master,
        request,
        caller,
        operation,
        kernel,
        GrantStep::Deliver,
    )
}

const JOURNAL_GRANT: &str = "\0native grant journal";

#[derive(Clone, Copy, PartialEq, Eq)]
enum GrantStep {
    Other,
    Reserve,
    Deliver,
}

struct GrantWriting {
    operation: Arc<Mutex<Operation>>,
    attempt: u64,
}

impl Drop for GrantWriting {
    fn drop(&mut self) {
        let mut op = self.operation.lock().unwrap_or_else(|p| p.into_inner());
        op.grant_record.finish(self.attempt, false);
    }
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "IssuerGrant",
        action = "Reserve",
        project = "native_act::tests::issuer::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "IssuerGrant",
        action = "Grant",
        project = "native_act::tests::issuer::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "IssuerGrant",
        action = "Invalidate",
        reason = "The injected conversation authority becomes stale"
    )
)]
#[cfg_attr(
    not(any(target_vendor = "apple", target_os = "linux", test)),
    expect(
        unreachable_code,
        reason = "no DeathWatch can exist here, so an attach past the armed watch never runs"
    )
)]
fn serve_launcher_step(
    ctx: &Arc<crate::SessionCtx>,
    master: i32,
    request: &Request,
    caller: &SupervisorInputCaller,
    operation: &Arc<Mutex<Operation>>,
    kernel: &dyn Kernel,
    step: GrantStep,
) -> String {
    let phase = match request {
        Request::Attach(_, phase)
        | Request::Resolve(_, phase, _)
        | Request::Execute(_, phase)
        | Request::ExecFailed(_, phase, _) => *phase,
        _ => return DENIED.into(),
    };
    // Authenticate the retained helper and current fast guards before any policy I/O.
    // Retain the exact resolution here; validate its metadata after account admission,
    // whose writer wait must not outlive that observation. Neither read holds Meta/Operation.
    let retained_cold = if matches!(request, Request::Execute(..) | Request::Resolve(..)) {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let op = operation.lock().unwrap_or_else(|p| p.into_inner());
        if !registered_launcher(ctx, master, &op, phase, caller, kernel)
            || !launcher_guards_current(
                ctx,
                &op,
                meta.supervisor_authority(crate::metrics::now_us()) == op.authority.snapshot,
                kernel,
            )
            || matches!(request, Request::Resolve(_, _, facts)
                if op.cold_facts.as_ref().is_some_and(|old| old != facts))
        {
            return DENIED.into();
        }
        let resolved = op.resolved_cold.clone();
        drop(op);
        drop(meta);
        resolved
    } else {
        None
    };
    // Watch creation may acquire spawn/CLOEXEC synchronization. Arm it before either
    // predicate; after locking, the exact complete process identity is read again.
    let mut armed = if matches!(request, Request::Attach(..)) {
        caller
            .peer
            .and_then(|pid| kernel.process(pid))
            .and_then(|process| kernel.watch(&process).map(|death| (process, death)))
    } else {
        None
    };
    // Source selection and filesystem checks run before Meta/Operation. Losing
    // Attach observers can only revoke an ungranted constructor, not an exec.
    let source_command = if matches!(request, Request::Attach(..)) {
        let op = operation.lock().unwrap_or_else(|p| p.into_inner());
        if op.authority.renewal.is_some() && op.children[phase_index(phase)].is_none() {
            command_for(&op, phase).cloned()
        } else {
            None
        }
    } else {
        None
    };
    let source_constructor = match source_command
        .as_ref()
        .map(|command| {
            let child = armed.as_ref().map(|(process, _)| process).ok_or(())?;
            kernel.source_constructor(ctx.self_id.as_str(), command, child)
        })
        .transpose()
    {
        Ok(constructor) => constructor.flatten(),
        Err(()) => return DENIED.into(),
    };
    // The account writer is outside Meta/Operation, then retained through the
    // exact first-exec grant below. The immutable command is checked again under
    // the operation predicate; already granted children retain their outcome.
    let account_command = if matches!(request, Request::Execute(..)) {
        let op = operation.lock().unwrap_or_else(|p| p.into_inner());
        if op.authority.renewal.is_none()
            && op.children[phase_index(phase)]
                .as_ref()
                .is_none_or(|child| !child.execute_sent)
        {
            match command_for(&op, phase) {
                Some(command) => Some((
                    command.clone(),
                    op.resolved_cold
                        .as_ref()
                        .map(|resolved| resolved.facts.clone()),
                )),
                None => return DENIED.into(),
            }
        } else {
            None
        }
    } else {
        None
    };
    let _account_guard = match account_command
        .as_ref()
        .map(|(command, facts)| {
            caller
                .peer
                .ok_or(())
                .and_then(|pid| kernel.account_execution(command, pid, facts.as_ref()))
        })
        .transpose()
    {
        Ok(guard) => guard,
        Err(()) => return DENIED.into(),
    };
    let checked_cold = retained_cold.map(|resolved| {
        let valid = kernel.cold_current(resolved.proof.as_ref());
        (resolved, valid)
    });
    // Only Execute grants the first exec effect. Attach is observation: even after a guard
    // lapses, recording the already launched child's birth/witness is needed to retire it.
    let meta = matches!(request, Request::Execute(..) | Request::Resolve(..))
        .then(|| ctx.meta.lock().unwrap_or_else(|p| p.into_inner()));
    let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
    let NativePlan::Restart {
        shell, launcher, ..
    } = &op.request.plan
    else {
        return DENIED.into();
    };
    if caller.principal != crate::control::Principal::Instance
        || !op.authority.builtin
        || op.phase != Phase::Started
        || op.resume_phase != Some(phase)
        || !op.resume_sent
    {
        return DENIED.into();
    }
    let Some(connection) = caller.connection else {
        return DENIED.into();
    };
    let Some(actual) = caller.peer.and_then(|pid| kernel.process(pid)) else {
        return DENIED.into();
    };
    if actual.parent != shell.pid
        || actual.group != actual.pid
        || actual.session != shell.session
        || actual.image != *launcher
        || kernel.process(shell.pid).as_ref() != Some(shell)
        || kernel.foreground(master) != Some(actual.group)
    {
        return DENIED.into();
    }
    let index = phase_index(phase);
    if let Some(child) = &op.children[index] {
        if child.process != actual || child.connection != connection {
            return DENIED.into();
        }
    } else if matches!(request, Request::Attach(..)) {
        if armed.as_ref().is_none_or(|(watched, _)| watched != &actual) {
            return UNKNOWN.into();
        }
        if source_constructor.as_ref().is_some_and(|constructor| {
            command_for(&op, phase).is_none_or(|command| !constructor.matches(command, &actual))
        }) {
            return DENIED.into();
        }
        let (_, death) = armed.take().expect("exact prearmed process");
        op.children[index] = Some(LaunchChild {
            source_constructor,
            process: actual,
            death: Arc::new(SharedDeath {
                watch: death,
                outcome: std::sync::OnceLock::new(),
            }),
            connection,
            execute_sent: false,
            exec_errno: None,
        });
    } else {
        return DENIED.into();
    }
    match request {
        Request::Attach(..) => {
            if let NativePlan::Restart {
                cold: Some(intent),
                forward,
                ..
            } = &op.request.plan
            {
                if phase != ImagePhase::Forward {
                    return DENIED.into();
                }
                let Some(seed) = forward.encode() else {
                    return DENIED.into();
                };
                let Some(intent) = intent.encode_for_launcher() else {
                    return DENIED.into();
                };
                return format!(
                    "OK native-act state=need-environment deadline-us={} seed={} intent={}\n",
                    op.request.deadline_us,
                    aterm_codec::hex::encode_lower(seed.as_bytes()),
                    aterm_codec::hex::encode_lower(intent.as_bytes()),
                );
            }
            let command = op.children[index]
                .as_ref()
                .and_then(|child| child.source_constructor.as_ref())
                .map(|constructor| constructor.command())
                .or_else(|| command_for(&op, phase));
            let Some(command) = command.and_then(RestartCommand::encode) else {
                return DENIED.into();
            };
            format!(
                "OK native-act state=launch deadline-us={} command={}\n",
                op.request.deadline_us,
                aterm_codec::hex::encode_lower(command.as_bytes())
            )
        }
        Request::Resolve(_, _, facts) => {
            let Some(meta) = meta else {
                return DENIED.into();
            };
            if phase != ImagePhase::Forward
                || !launcher_guards_current(
                    ctx,
                    &op,
                    meta.supervisor_authority(crate::metrics::now_us()) == op.authority.snapshot,
                    kernel,
                )
                || facts.encode().is_none()
                || op.cold_facts.as_ref().is_some_and(|old| old != facts)
            {
                return DENIED.into();
            }
            let NativePlan::Restart {
                cold: Some(intent),
                forward: seed,
                ..
            } = &op.request.plan
            else {
                return DENIED.into();
            };
            if let Some(resolved) = &op.resolved_cold {
                return if checked_cold.as_ref().is_some_and(|(checked, valid)| {
                    *valid && Arc::ptr_eq(checked, resolved) && resolved.facts == *facts
                }) {
                    resolved_reply(&resolved.command, op.request.deadline_us)
                } else {
                    DENIED.into()
                };
            }
            if op.cold_resolving {
                return UNKNOWN.into();
            }
            if op.cold_facts.is_some() {
                // This helper's first resolution was refused. A duplicate observation
                // cannot select again; only lawful retirement and Resume start a new helper.
                return DENIED.into();
            }
            let intent = intent.clone();
            let seed = seed.clone();
            op.cold_facts = Some(facts.clone());
            op.cold_resolving = true;
            drop(op);
            drop(meta);
            let resolution =
                kernel
                    .resolve_cold(ctx, &intent, &seed, facts)
                    .ok()
                    .filter(|(command, proof)| {
                        seed.cold_resolution_agrees(command)
                            && command.encode().is_some()
                            && kernel.image(&command.image)
                            && kernel.cold_current(proof.as_ref())
                    });
            let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
            if !registered_launcher(ctx, master, &op, phase, caller, kernel) {
                return DENIED.into();
            }
            op.cold_resolving = false;
            if !launcher_guards_current(
                ctx,
                &op,
                meta.supervisor_authority(crate::metrics::now_us()) == op.authority.snapshot,
                kernel,
            ) || op.cold_facts.as_ref() != Some(facts)
                || op.resolved_cold.is_some()
            {
                return DENIED.into();
            }
            let Some((command, proof)) = resolution else {
                return DENIED.into();
            };
            let reply = resolved_reply(&command, op.request.deadline_us);
            op.resolved_cold = Some(Arc::new(ResolvedCold {
                command,
                facts: facts.clone(),
                proof,
            }));
            reply
        }
        Request::Execute(..) => {
            let Some(meta) = meta else {
                return DENIED.into();
            };
            let cold_valid = match &op.request.plan {
                NativePlan::Restart { cold: Some(_), .. } => {
                    op.resolved_cold.as_ref().is_some_and(|resolved| {
                        checked_cold.as_ref().is_some_and(|(checked, valid)| {
                            *valid && Arc::ptr_eq(checked, resolved)
                        })
                    })
                }
                _ => true,
            };
            if !cold_valid
                || !launcher_guards_current(
                    ctx,
                    &op,
                    meta.supervisor_authority(crate::metrics::now_us()) == op.authority.snapshot,
                    kernel,
                )
                || op.clock.remaining(op.request.deadline_us).is_none()
                || account_command.as_ref().is_some_and(|(expected, facts)| {
                    command_for(&op, phase) != Some(expected)
                        || op.resolved_cold.as_ref().map(|resolved| &resolved.facts)
                            != facts.as_ref()
                })
                || op
                    .authority
                    .renewal
                    .as_ref()
                    .is_some_and(|seal| !seal.current())
                || ctx.human_input.seq() != op.human_seq
                || op
                    .authority
                    .conversation
                    .as_ref()
                    .is_none_or(|guard| !guard.current(ctx))
                || (!op.effect_started.load(std::sync::atomic::Ordering::Acquire)
                    && meta.supervisor_authority(crate::metrics::now_us()) != op.authority.snapshot)
                || command_for(&op, phase).is_none_or(|command| !kernel.image(&command.image))
                || op.children[index]
                    .as_ref()
                    .is_some_and(|child| child.exec_errno.is_some())
            {
                return DENIED.into();
            }
            match (step, op.grant_record) {
                (_, GrantRecord::Writing(_))
                | (GrantStep::Deliver, GrantRecord::Uncertain(_) | GrantRecord::None) => {
                    return UNKNOWN.into();
                }
                (GrantStep::Reserve, GrantRecord::None | GrantRecord::Uncertain(_)) => {
                    if op.grant_record.begin().is_none() {
                        return UNKNOWN.into();
                    }
                    return JOURNAL_GRANT.into();
                }
                (GrantStep::Reserve | GrantStep::Deliver, GrantRecord::Recorded) => {}
                _ => return UNKNOWN.into(),
            }
            if let Some(child) = op.children[index].as_ref()
                && let Some(constructor) = &child.source_constructor
                && constructor.grant(&child.process).is_err()
            {
                return DENIED.into();
            }
            op.children[index].as_mut().unwrap().execute_sent = true;
            op.effect_started
                .store(true, std::sync::atomic::Ordering::Release);
            format!(
                "OK native-act state=execute deadline-us={}\n",
                op.request.deadline_us
            )
        }
        Request::ExecFailed(_, _, errno) => {
            let child = op.children[index].as_mut().unwrap();
            if !child.execute_sent || child.exec_errno.is_some_and(|old| old != *errno) {
                return DENIED.into();
            }
            child.exec_errno = Some(*errno);
            "OK native-act state=exec-failed\n".into()
        }
        _ => unreachable!(),
    }
}

/// Positive proof that this attempt's helper can neither receive nor use an exec
/// grant. A missing child or an unread death/group/queue fact is not that proof.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "IssuerGrant",
        action = "ProbeReplacement",
        project = "native_act::tests::issuer::transition"
    )
)]
fn launch_unissued(op: &Operation, phase: ImagePhase, kernel: &dyn Kernel) -> bool {
    op.grant_record == GrantRecord::None
        && op.authority.builtin
        && op.resume_sent
        && op.resume_phase == Some(phase)
        && op.children[phase_index(phase)]
            .as_ref()
            .is_some_and(|child| {
                !child.execute_sent
                    && child.death.exited()
                    && kernel.group_gone(child.process.group) == Some(true)
            })
        && op
            .resume_attempt
            .as_ref()
            .is_some_and(|attempt| attempt.settled())
}

fn retirement(op: &Operation, kernel: &dyn Kernel) -> Option<Retirement> {
    if matches!(
        op.grant_record,
        GrantRecord::Writing(_) | GrantRecord::Uncertain(_)
    ) || (op.grant_record == GrantRecord::Recorded
        && !op.children.iter().flatten().any(|child| child.execute_sent))
    {
        return None;
    }
    match &op.request.plan {
        NativePlan::OwnImage { child, .. } => op
            .authority
            .actor
            .as_ref()
            .is_some_and(|actor| {
                [actor, child].iter().all(|process| {
                    kernel.birth_alive(process) == Some(false)
                        && kernel.group_gone(process.group) == Some(true)
                })
            })
            .then_some(Retirement::Exited),
        NativePlan::Restart { process, .. } => {
            if kernel.birth_alive(process) != Some(false)
                || op
                    .source_death
                    .as_ref()
                    .is_some_and(|watch| !watch.exited())
                || kernel.group_gone(process.group) != Some(true)
                || op
                    .resume_attempt
                    .as_ref()
                    .is_some_and(|attempt| !attempt.settled())
                || op
                    .children
                    .iter()
                    .flatten()
                    .filter(|child| child.execute_sent)
                    .any(|child| {
                        !child.death.exited()
                            || kernel.group_gone(child.process.group) != Some(true)
                    })
            {
                return None;
            }
            Some(
                if op.children.iter().flatten().any(|child| child.execute_sent) {
                    Retirement::Exited
                } else {
                    Retirement::NotGranted
                },
            )
        }
    }
}

/// An attempted or acknowledged append without a grant remains an unknown
/// execution outcome. Once every involved cohort and input attempt is positively
/// quiet, the host may revoke the capability and reclaim memory without NoExec.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "IssuerGrant",
        action = "Close",
        project = "native_act::tests::issuer::transition"
    )
)]
fn uncertain_quiescent(op: &Operation, kernel: &dyn Kernel) -> bool {
    let NativePlan::Restart { process, .. } = &op.request.plan else {
        return false;
    };
    (matches!(op.grant_record, GrantRecord::Uncertain(_))
        || (op.grant_record == GrantRecord::Recorded
            && !op.children.iter().flatten().any(|child| child.execute_sent)))
        && kernel.birth_alive(process) == Some(false)
        && kernel.group_gone(process.group) == Some(true)
        && op.source_death.as_ref().is_none_or(|watch| watch.exited())
        && op
            .resume_attempt
            .as_ref()
            .is_none_or(|attempt| attempt.settled())
        && op.children.iter().flatten().all(|child| {
            child.death.exited() && kernel.group_gone(child.process.group) == Some(true)
        })
}

/// Whether `ctx` may hold an operation not yet completed: a table this could not read counts, so
/// the sweep looks again.
pub(crate) fn maybe_unsettled(ctx: &crate::SessionCtx) -> bool {
    match ctx.fenced_writes.native.try_lock() {
        Ok(table) => table.by_id.len() != table.completed.len(),
        Err(std::sync::TryLockError::Poisoned(p)) => {
            let table = p.into_inner();
            table.by_id.len() != table.completed.len()
        }
        Err(std::sync::TryLockError::WouldBlock) => true,
    }
}

/// Close a failed plan without ever waiting for input or GUI-needed work. Every Go-granted
/// child needs retained death/group proof; a terminal no-Go seal rejects delayed helpers,
/// without claiming an unseen/ungranted helper is dead. Pending actual input stays Unknown.
pub(crate) fn reconcile_extinct(ctx: &crate::SessionCtx) {
    reconcile_extinct_by(ctx, &System(&ctx.native_policy, false));
}

/// Probe the actual production conversation guard for a still-live source.
#[cfg(all(unix, debug_assertions))]
pub(crate) fn fixture_live_conversation(
    ctx: &crate::SessionCtx,
    master: i32,
    source: &Process,
) -> bool {
    let kernel = System(&ctx.native_policy, false);
    kernel.process(source.pid).as_ref() == Some(source)
        && kernel.foreground(master) == Some(source.group)
        && kernel
            .conversation(ctx, source)
            .is_some_and(|guard| guard.current(ctx))
}

#[cfg(all(unix, debug_assertions))]
/// Read the actual fixture operation after the production terminal sweep. This
/// is an observation-triggered reconciliation, not an admission or a raw effect.
pub(crate) fn fixture_observe_operation(
    ctx: &crate::SessionCtx,
    id: &str,
) -> Option<aterm_json::Value> {
    reconcile_extinct(ctx);
    fixture_operation_state(ctx, id, true)
}

#[cfg(all(unix, debug_assertions))]
pub(crate) fn fixture_active_signalled_operation(ctx: &crate::SessionCtx) -> Option<String> {
    let operations = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .by_id
        .values()
        .cloned()
        .collect::<Vec<_>>();
    let mut ids = operations.iter().filter_map(|operation| {
        let op = operation.lock().unwrap_or_else(|p| p.into_inner());
        (!op.resume_only
            && op.signal_accepted == Some(true)
            && !matches!(op.phase, Phase::Completed(_))
            && op
                .authority
                .renewal
                .as_ref()
                .is_some_and(|seal| seal.bound_to_weak(ctx)))
        .then(|| op.request.id.clone())
    });
    let id = ids.next()?;
    ids.next().is_none().then_some(id)
}

/// A completed owner gesture reports what Stop itself did, without a diagnostic
/// observer silently completing the operation first.
#[cfg(all(unix, debug_assertions))]
pub(crate) fn fixture_operation_state(
    ctx: &crate::SessionCtx,
    id: &str,
    observation_reconciled: bool,
) -> Option<aterm_json::Value> {
    if !aterm_codec::hex::is_lower(id, 32) {
        return None;
    }
    let operation = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .by_id
        .get(id)?
        .clone();
    let op = operation.lock().unwrap_or_else(|p| p.into_inner());
    let kernel = System(&ctx.native_policy, false);
    let (state, decision) = match op.phase {
        Phase::Completed(Decision::Retired(Retirement::NotGranted)) => {
            ("completed", "retired-no-exec-grant")
        }
        Phase::Completed(Decision::Retired(Retirement::Exited)) => {
            ("completed", "retired-cohort-exited")
        }
        Phase::Completed(Decision::ProvenNotCommittedRollbackSafe) => {
            ("completed", "rollback-safe")
        }
        Phase::Completed(_) => ("completed", "other"),
        _ => ("active", "none"),
    };
    let (source_alive, source_group_gone) = match &op.request.plan {
        NativePlan::Restart { process, .. } => (
            kernel.birth_alive(process),
            kernel.group_gone(process.group),
        ),
        _ => (None, None),
    };
    Some(
        aterm_json::json!({"operation":id,"observationTriggeredReconciliation":observation_reconciled,
        "state":state,"decision":decision,"deadlineExpired":op.clock.remaining(op.request.deadline_us).is_none(),
        "signalSent":op.signal_sent,"signalAccepted":op.signal_accepted,
        "resumeSent":op.resume_sent,"inputAttemptSettled":op.resume_attempt.as_ref().is_none_or(|a|a.settled()),
        "inputCustodySettled":op.custody.is_none() && op.completion_drain.as_ref().is_none_or(NativeDrain::is_drained),
        "goGrantedChildren":op.children.iter().flatten().filter(|child|child.execute_sent).count(),
        "retainedChildren":op.children.iter().flatten().count(),
        "sourceBirthAlive":source_alive,"sourceGroupGone":source_group_gone,
        "retainedSourceWatch":op.source_death.is_some(),"closedRestartWitness":op.closed_restart.is_some(),
        "sourceWatchExited":op.source_death.as_ref().map(|watch|watch.exited()),
        "closedRestartQuiescent":op.closed_restart.as_ref().is_some_and(|proof|proof.quiescent(&kernel)),
        "closedRestartAvailable":op.closed_restart.as_ref().is_some_and(|proof|proof.available()),
        "resumeOnly":op.resume_only,"deadlineUs":op.request.deadline_us,"monotonicUs":op.clock.now_us(),
        "expectedHumanSeq":op.request.expected_human_seq,
        "logicalRoot":op.authority.renewal.as_ref().map(|seal|seal.receipt_id()),
        "nativePredecessor":op.authority.continuation().map(|proof|proof.predecessor.id.as_str())}),
    )
}

fn reconcile_extinct_by(ctx: &crate::SessionCtx, kernel: &dyn Kernel) {
    let operations = {
        let table = match ctx.fenced_writes.native.try_lock() {
            Ok(table) => table,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        // Ordinary sessions and receipt-only history need no metadata clone or process probe.
        // A terminal prefix still owns its separate input lease; its writer rings that drain.
        if table.by_id.len() == table.completed.len() {
            return;
        }
        table.by_id.values().cloned().collect::<Vec<_>>()
    };
    let snapshot = match ctx.meta.try_lock() {
        Ok(meta) => meta.supervisor_authority(crate::metrics::now_us()),
        Err(std::sync::TryLockError::Poisoned(p)) => p
            .into_inner()
            .supervisor_authority(crate::metrics::now_us()),
        Err(std::sync::TryLockError::WouldBlock) => return,
    };
    for operation in operations {
        let mut op = match operation.try_lock() {
            Ok(op) => op,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => continue,
        };
        if matches!(op.phase, Phase::Queued | Phase::Completed(_)) {
            continue;
        }
        let abandoned = match &op.request.plan {
            NativePlan::OwnImage { .. } => true,
            NativePlan::Restart { shell, .. } => {
                op.clock.remaining(op.request.deadline_us).is_none()
                    || op
                        .authority
                        .renewal
                        .as_ref()
                        .is_some_and(|seal| seal.durably_cancelled(ctx))
                    || (!op.effect_started.load(std::sync::atomic::Ordering::Acquire)
                        && snapshot != op.authority.snapshot)
                    || kernel.birth_alive(shell) == Some(false)
            }
        };
        if !abandoned {
            continue;
        }
        if uncertain_quiescent(&op, kernel) {
            let completion = op.seal_completion(Decision::Unknown);
            drop(op);
            let _ = finish(ctx, &operation, completion);
            ctx.watchers.ring_watchers();
            continue;
        }
        if op.authority.builtin
            && (!op.signal_sent || op.signal_accepted == Some(false))
            && !op.resume_sent
            && op.resume_attempt.is_none()
        {
            let Some(completion) = op.seal_without_effect(kernel) else {
                continue;
            };
            drop(op);
            let _receipt = finish(ctx, &operation, Some(completion));
            ctx.watchers.ring_watchers();
            continue;
        }
        let Some(reason) = retirement(&op, kernel) else {
            continue;
        };
        let completion = op.seal_retirement(reason, kernel);
        drop(op);
        let _receipt = finish(ctx, &operation, completion);
        ctx.watchers.ring_watchers();
    }
}

pub(crate) fn serve(
    ctx: &Arc<crate::SessionCtx>,
    master: i32,
    tail: &str,
    resume: &mut dyn FnMut(&str) -> String,
) -> String {
    let Some(request) = Request::parse(tail) else {
        return "ERR usage: native-act <prepare|precommit|commit|query|signal|resume> <bounded fields>\n".into();
    };
    let principal = crate::control::serving_principal();
    let holder = match &request {
        Request::Prepare(request) => request.holder.clone(),
        Request::CancelPrepare(cancel) => cancel.prepare.holder.clone(),
        _ => String::new(),
    };
    let caller = SupervisorInputCaller {
        principal,
        connection: crate::control::serving_connection(),
        peer: crate::control::serving_peer_pid(),
        proof: (principal != crate::control::Principal::Harness)
            .then(|| crate::control::prove_self_claim_for(ctx, &holder, master))
            .flatten(),
    };
    let renewal_id = match &request {
        Request::Prepare(prepare) => Some(prepare.id.as_str()),
        Request::Precommit(binding, _)
        | Request::Commit(binding, _, _)
        | Request::Query(binding)
        | Request::Abort(binding)
        | Request::Recover(binding)
        | Request::Signal(binding)
        | Request::Resume(binding, _, _, _)
        | Request::Attach(binding, _)
        | Request::Resolve(binding, _, _)
        | Request::Execute(binding, _)
        | Request::ExecFailed(binding, _, _)
        | Request::Reconcile(binding) => Some(binding.id.as_str()),
        _ => None,
    };
    let sealed = renewal_id.is_some_and(|id| {
        ctx.fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .renewal_seals
            .contains_key(id)
    });
    serve_by(
        ctx,
        master,
        request,
        caller,
        &System(&ctx.native_policy, sealed),
        resume,
    )
}

fn request_operation(ctx: &crate::SessionCtx, request: &Request) -> Option<Arc<Mutex<Operation>>> {
    match request {
        Request::Prepare(prepare) => ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .by_id
            .get(&prepare.id)
            .cloned(),
        Request::CancelPrepare(cancel) => ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .by_id
            .get(&cancel.prepare.id)
            .cloned(),
        Request::Query(binding)
        | Request::Reconcile(binding)
        | Request::Abort(binding)
        | Request::Recover(binding)
        | Request::Signal(binding)
        | Request::Precommit(binding, _)
        | Request::Commit(binding, _, _)
        | Request::Resume(binding, _, _, _)
        | Request::Attach(binding, _)
        | Request::Resolve(binding, _, _)
        | Request::Execute(binding, _)
        | Request::ExecFailed(binding, _, _) => lookup(ctx, binding),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalRecord {
    NoExec(ImagePhase),
    Sealed,
}

#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "TerminalPublication",
        action = "MayExecute",
        reason = "Possible execution is established by the issuer grant protocol before terminal publication"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "TerminalPublication",
        action = "CloseNoExec",
        reason = "Positive process quiescence and zero grants are premises checked by ClosedRestart and IssuerGrant"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "TerminalPublication",
        action = "CloseUnknown",
        reason = "An opaque terminal fact is supplied by the separately checked completion protocol"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "TerminalPublication",
        action = "Drain",
        reason = "External input producers finish; the real NativeDrain is checked before selecting a record"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "TerminalPublication",
        action = "CaptureUnknown",
        reason = "Other native request handlers can return Unknown for an already terminal operation"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::spec_unmodeled(
        machine = "TerminalPublication",
        action = "StartRollback",
        reason = "The Resume protocol establishes the active phase before terminal publication"
    )
)]
fn terminal_record(op: &Operation, reply: &str) -> Option<TerminalRecord> {
    // These classifications were already acknowledged by exact claim recovery.
    // Observing possible execution must not silently append a new opaque seal.
    if op.preparation.terminal_already_recorded() {
        return None;
    }
    if !op.preparation.can_publish()
        || matches!(op.request.plan, NativePlan::OwnImage { .. })
        || !matches!(op.phase, Phase::Completed(_))
        || op
            .completion_drain
            .as_ref()
            .is_some_and(|drain| !drain.is_drained())
    {
        return None;
    }
    let record = match op.phase {
        Phase::Completed(Decision::Retired(Retirement::NotGranted)) => {
            if op.grant_record != GrantRecord::None {
                return None;
            }
            let phase = op.resume_phase.unwrap_or(ImagePhase::Forward);
            if op
                .closed_restart
                .as_ref()
                .is_some_and(|proof| proof.closed_phase() != phase)
            {
                return None;
            }
            TerminalRecord::NoExec(phase)
        }
        _ => TerminalRecord::Sealed,
    };
    (reply != NOEXEC || matches!(record, TerminalRecord::NoExec(_))).then_some(record)
}

/// The reply may have been captured before the drain settled. Its text cannot
/// choose the durable fact, and a failed or lost append acknowledgement cannot
/// publish a positive observation.
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "TerminalPublication",
        action = "PublishAck",
        project = "aterm_gui::native_act::tests::publication::transition"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "TerminalPublication",
        action = "PublishLostAck",
        project = "aterm_gui::native_act::tests::publication::transition"
    )
)]
#[cfg_attr(
    all(unix, any(test, debug_assertions)),
    aterm_spec_anchor::refines(
        machine = "TerminalPublication",
        action = "PublishFailed",
        project = "aterm_gui::native_act::tests::publication::transition"
    )
)]
fn publish_terminal(
    reply: String,
    record: Option<TerminalRecord>,
    append: impl FnOnce(TerminalRecord) -> bool,
) -> String {
    if record.is_some_and(append) {
        reply
    } else {
        UNKNOWN.into()
    }
}

fn serve_by(
    ctx: &Arc<crate::SessionCtx>,
    master: i32,
    request: Request,
    caller: SupervisorInputCaller,
    kernel: &dyn Kernel,
    resume: &mut dyn FnMut(&str) -> String,
) -> String {
    let historical_reader = caller.principal == crate::control::Principal::Harness
        && caller.peer.and_then(|pid| kernel.process(pid)).is_some()
        && matches!(
            request,
            Request::Prepare(_) | Request::Query(_) | Request::Reconcile(_)
        );
    serve_with_publication(ctx, request, kernel, historical_reader, |request| {
        serve_request(ctx, master, request, caller, kernel, resume)
    })
}

/// Every admission entry shares the same transient pin, handoff hold and
/// terminal publication boundary, including the private account transaction.
fn serve_with_publication(
    ctx: &crate::SessionCtx,
    request: Request,
    kernel: &dyn Kernel,
    historical_reader: bool,
    execute: impl FnOnce(Request) -> String,
) -> String {
    let _flight = match ReceiptFlight::begin(ctx, &request) {
        Ok(flight) => flight,
        Err(()) => return UNKNOWN.into(),
    };
    let saved = request.clone();
    let operation = request_operation(ctx, &request);
    // Clone only the existing app hold, not the input marker: a terminal seal
    // can then drain input without opening a handoff window before persistence.
    let mut receipt_handoff = operation.as_ref().and_then(|operation| {
        operation
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .custody
            .as_ref()
            .map(|custody| custody.handoff_guard())
    });
    let reply = execute(request);
    if matches!(&saved, Request::Prepare(prepare) if prepare.asked.is_some()) {
        let class = if reply == DENIED {
            "denied"
        } else if reply == UNKNOWN {
            "unknown"
        } else if reply.starts_with("ERR busy") {
            "busy"
        } else if reply.starts_with("OK native-act state=admitted ") {
            "admitted"
        } else if reply.starts_with("OK native-act state=pending ") {
            "pending"
        } else {
            "other"
        };
        aterm_log::debug!("asked native prepare result={class}");
    }
    if reply == UNKNOWN
        || reply == ROLLBACK_SAFE
        || reply.starts_with("OK native-act state=committed ")
        || reply.starts_with("OK native-act state=retired ")
    {
        let operation = operation.or_else(|| request_operation(ctx, &saved));
        let evidence = operation.as_ref().and_then(|operation| {
            let op = operation.lock().unwrap_or_else(|p| p.into_inner());
            terminal_record(&op, &reply).and_then(|record| {
                let Phase::Completed(decision) = op.phase else {
                    return None;
                };
                Some((op.request.clone(), op.binding.clone(), record, decision))
            })
        });
        if reply == UNKNOWN && evidence.is_none() {
            return reply;
        }
        if operation.as_ref().is_some_and(|operation| {
            matches!(
                operation
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .request
                    .plan,
                NativePlan::OwnImage { .. }
            )
        }) {
            return reply;
        }
        // No acknowledged receipt may escape before the durable append. This
        // worker owns no Meta, operation or table lock across the journal I/O.
        // A GUI sweep may have sealed it before this request arrived. In that
        // case acquire a fresh hold or defer to an already pending handoff.
        if receipt_handoff.is_none() {
            receipt_handoff = aterm_types::update_boundary::Boundary::process()
                .try_hold(aterm_types::update_boundary::GuardedItem::HarnessNativeMutation)
                .map(Arc::new);
        }
        if receipt_handoff.is_none() {
            return UNKNOWN.into();
        }
        let mut acknowledged = false;
        let published = publish_terminal(
            reply,
            evidence.as_ref().map(|(_, _, record, _)| *record),
            |record| {
                let (prepare, binding, _, _) =
                    evidence.as_ref().expect("selected terminal evidence");
                acknowledged = match record {
                    TerminalRecord::NoExec(phase) => kernel.record_noexec(prepare, binding, phase),
                    TerminalRecord::Sealed => kernel.record_sealed(prepare, binding),
                };
                acknowledged
            },
        );
        if acknowledged
            && let (Some(operation), Some((prepare, binding, _, decision))) =
                (&operation, &evidence)
        {
            asked_upgrade::acknowledge_terminal(ctx, operation, prepare, binding, *decision);
        }
        return published;
    }
    // Keep the request pin through this read too. Account relations need their
    // own durable envelope before they can use ordinary historical fallback.
    if historical_reader
        && (reply == DENIED
            || reply == RECIPIENT_WRITER_UNAVAILABLE
            || reply == "ERR native host epoch changed\n"
            || reply == DEADLINE
            || reply == "ERR native constructor disabled\n")
        && !(matches!(saved, Request::Prepare(_))
            && request_operation(ctx, &saved).is_some_and(|op| {
                op.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .authority
                    .account
                    .is_some()
            }))
        && let Some(answer) = historical(ctx, &saved, kernel)
    {
        return answer;
    }
    reply
}

/// The historical answer to a refused replay: a positive NoExec receipt, else, for a prepare,
/// [`historical_prepare`]'s, which serves the exact prepare's NoExec record as one too.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeHistoricalClosure",
        action = "AnswerNoExec",
        project = "aterm_gui::native_act::tests::historical_closure::Host::probe"
    )
)]
fn historical(ctx: &crate::SessionCtx, saved: &Request, kernel: &dyn Kernel) -> Option<String> {
    if kernel.historical_noexec(saved) {
        return Some(NOEXEC.into());
    }
    match saved {
        Request::Prepare(prepare) => historical_prepare(ctx, prepare, kernel),
        _ => None,
    }
}

/// `lapsed` for a refused restart prepare when no grant under it can leave an agent running,
/// from facts this host reads now; `None` keeps the refusal. It writes, reserves and admits
/// nothing, and claims nothing about the old operation's other resources.
///
/// Only a cold or supported typed asked restart from the built-in harness qualifies (the
/// caller's `historical_reader`), with no reservation, cancellation, account fence, renewal
/// seal or unfinished operation for its id here. A private restart, its Codex renewal, a
/// self-exec plan and an account relation keep the refusal. An issuer older than the
/// journal granted without recording: an exact record proves an issuer that records, and an
/// absence counts only for a stamped prepare, which no such issuer could parse.
///
/// The clock is read before the journal (one CLOCK_MONOTONIC per boot), then:
/// 1. the exact prepare's NoExec is NOEXEC;
/// 2. its seal with no possible execution is `lapsed` at any clock: a grant records
///    possible execution, acknowledged, before it is delivered, and a seal is final;
/// 3. its Prepared record read after the deadline is `lapsed`: a grant acknowledged before
///    the read would show, and one acknowledged after it fails the delivery's own deadline
///    check, while no record that late can start;
/// 4. its absence read after the deadline from the journal instance its `custody=` stamp names
///    is `lapsed`: every restart admission is recorded before it starts, and only a host that
///    records admits a stamped prepare, while the stamp names that one journal by its first
///    record (one enrolled again, or set aside and replaced, is another custody; a journal is
///    never rolled back, the premise of every journal read);
/// 5. a `boot=` stamp naming another boot of this machine makes the exact record (Prepared,
///    possible execution or a seal), or an absence from the journal its `custody=` stamp
///    names, `lapsed` at any clock:
///    every process of that boot has ended, and no host of this boot admits the stamp;
/// 6. a complete recipient history can lapse on this boot after the deadline when every
///    recorded birth and process group has positively retired; its own reader keeps unknown
///    probes, foreign clocks and inconsistent histories held;
/// 7. anything else keeps the refusal: legacy possible execution on this boot, another record
///    of the id, an unreadable journal, a deadline still ahead, or an unstamped absence.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeHistoricalClosure",
        action = "ReadClock",
        project = "aterm_gui::native_act::tests::historical_closure::Host::probe"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeHistoricalClosure",
        action = "ReadJournal",
        project = "aterm_gui::native_act::tests::historical_closure::Host::probe"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeHistoricalClosure",
        action = "AnswerLapsed",
        project = "aterm_gui::native_act::tests::historical_closure::Host::probe"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeHistoricalClosure",
        action = "AnswerBoot",
        project = "aterm_gui::native_act::tests::historical_closure::Host::probe"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeHistoricalClosure",
        action = "StampBoot",
        reason = "The harness stamps its prepare with this process's machine and boot, and prepare_inner admits no boot stamp but its own"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeHistoricalClosure",
        action = "StampCustody",
        reason = "The harness stamps its prepare with its issuer journal's custody once the journal holds a record, and prepare_inner admits no custody stamp but its own"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeHistoricalClosure",
        action = "Legacy",
        reason = "An issuer older than the journal granted restarts with no record, and only an unstamped prepare can name one"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeHistoricalClosure",
        action = "Record",
        reason = "Admission appends the Prepare record and has it acknowledged before the operation starts"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeHistoricalClosure",
        action = "May",
        reason = "An exec grant appends MayExecute, acknowledged, only after Prepared and before it is delivered"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeHistoricalClosure",
        action = "Deliver",
        reason = "Delivery re-reads the monotonic clock after the append and replies state=execute only before the deadline; the launcher execs only on that reply"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeHistoricalClosure",
        action = "DeliverRollback",
        reason = "After the forward recipient and its group retire, rollback delivery uses the same MayExecute record and re-reads the original deadline"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeHistoricalClosure",
        action = "SealOpen",
        reason = "A seal after Prepared reads as sealed with no possible execution, and nothing follows a seal"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeHistoricalClosure",
        action = "SealMay",
        reason = "A seal after MayExecute reads as sealed after possible execution, and nothing follows a seal"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeHistoricalClosure",
        action = "NoExec",
        reason = "A positive no-grant follows only Prepared, and nothing follows it"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeHistoricalClosure",
        action = "Expire",
        reason = "The boot's monotonic clock, which every process of the boot reads, passes the deadline"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeHistoricalClosure",
        action = "Crash",
        reason = "The issuing host epoch ends: a crash, a quit or an in-place update exec"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeHistoricalClosure",
        action = "ChildExits",
        reason = "The grantee the launcher exec'd exits"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeHistoricalClosure",
        action = "RollbackExits",
        reason = "The rollback grantee the launcher exec'd exits independently of the forward recipient"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeHistoricalClosure",
        action = "Reboot",
        reason = "The machine boots again, which ends every process of the old boot"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeHistoricalClosure",
        action = "Move",
        reason = "A process that finds a lab-marked vault sets the whole vault folder aside, or the journal is removed and enrolled again under the same identity; the custody after either is another journal instance, with another first record"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeHistoricalClosure",
        action = "Fresh",
        reason = "The harness ends its lapsed record and plans afresh (native_restart::lapsed, bound by NativeRetirementDebt)"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "RecipientClosure",
        action = "ReadClock",
        project = "aterm_gui::native_act::tests::recipient_recovery::World::probe"
    )
)]
fn historical_prepare(
    ctx: &crate::SessionCtx,
    prepare: &Prepare,
    kernel: &dyn Kernel,
) -> Option<String> {
    if !matches!(prepare.plan, NativePlan::Restart { .. })
        || !matches!(
            prepare.constructor(),
            Constructor::ColdRestart | Constructor::AskedUpgrade | Constructor::AskedRelaunch
        )
        || (prepare.asked.is_some() && !asked_upgrade::shape(prepare))
    {
        return None;
    }
    let clock_us = kernel.clock().now_us()?;
    let unfinished = {
        let table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if table.reservations.contains_key(&prepare.id)
            || table.cancelled.contains_key(&prepare.id)
            || table.account_fences.contains_key(&prepare.id)
            || table.renewal_seals.contains_key(&prepare.id)
        {
            return None;
        }
        table.by_id.get(&prepare.id).cloned()
    };
    if unfinished.is_some_and(|operation| {
        !matches!(
            operation.lock().unwrap_or_else(|p| p.into_inner()).phase,
            Phase::Completed(_)
        )
    }) {
        return None;
    }
    let closure = kernel.historical_closure(prepare).ok()?;
    let expired = clock_us >= prepare.deadline_us;
    let current_boot = prepare.boot.as_ref().and_then(|_| kernel.boot());
    let other_boot = prepare.boot.as_ref().is_some_and(|stamp| {
        current_boot
            .as_ref()
            .is_some_and(|here| here.host == stamp.host && here.boot != stamp.boot)
    });
    let lapsed = match closure {
        Closure::Exact(Recorded::NoExec) => return Some(NOEXEC.into()),
        Closure::Exact(Recorded::Sealed { may_execute: false }) => true,
        Closure::Exact(Recorded::Prepared) => expired || other_boot,
        Closure::Exact(Recorded::MayExecute | Recorded::Sealed { may_execute: true }) => other_boot,
        Closure::Absent(custody) => {
            prepare.custody.is_some() && custody == prepare.custody && (expired || other_boot)
        }
        Closure::Other => false,
        #[cfg(unix)]
        Closure::Recipients(history) => {
            return recipient_recovery::answer(
                prepare,
                &history,
                clock_us,
                current_boot.as_ref(),
                kernel,
            );
        }
    };
    if lapsed {
        aterm_agent::harness::native_act::lapsed_reply(&prepare.id)
    } else {
        None
    }
}

fn serve_request(
    ctx: &Arc<crate::SessionCtx>,
    master: i32,
    request: Request,
    caller: SupervisorInputCaller,
    kernel: &dyn Kernel,
    resume: &mut dyn FnMut(&str) -> String,
) -> String {
    let request = match request {
        Request::Constructors => {
            let enabled = Constructor::ALL.into_iter().filter(|kind| kernel.constructor_enabled(*kind)).map(Constructor::word).collect::<Vec<_>>();
            return format!("OK native-act state=constructors plans={}\n", if enabled.is_empty() { "-".into() } else { enabled.join(",") });
        }
        Request::Info => return host_epoch().map_or_else(|| "ERR native entropy unavailable\n".into(), |epoch|
            format!("OK native-act state=info epoch={epoch} native-act=1 handoff=blocked clock=monotonic-us own-image={}\n", own_image_rows(&ctx.native_policy))),
        Request::RejectionDomain => return if caller.principal == crate::control::Principal::Harness {
            match kernel.rejection_domain() {
                Ok(Some(domain)) => format!("OK native-act state=rejection-domain domain={domain}\n"),
                Ok(None) => "ERR usage: native-act rejection-domain unsupported\n".into(),
                Err(()) => UNKNOWN.into(),
            }
        } else {
            DENIED.into()
        },
        Request::Prepare(request) => return prepare(ctx, master, *request, caller, kernel),
        Request::CancelSupport => return "OK native-act state=cancel-support cancel-prepare=1\n".into(),
        Request::CancelPrepare(request) => return cancel_prepare(ctx, master, *request, caller, kernel),
        request => request,
    };
    let binding = match &request {
        Request::Info
        | Request::Constructors
        | Request::RejectionDomain
        | Request::Prepare(_)
        | Request::CancelSupport
        | Request::CancelPrepare(_) => unreachable!(),
        Request::Precommit(b, _)
        | Request::Commit(b, _, _)
        | Request::Query(b)
        | Request::Abort(b)
        | Request::Recover(b)
        | Request::Signal(b)
        | Request::Resume(b, _, _, _)
        | Request::Attach(b, _)
        | Request::Resolve(b, _, _)
        | Request::Execute(b, _)
        | Request::ExecFailed(b, _, _)
        | Request::Reconcile(b) => b,
    };
    let Some(operation) = lookup(ctx, binding) else {
        return DENIED.into();
    };
    if matches!(request, Request::Reconcile(_)) {
        let op = operation.lock().unwrap_or_else(|p| p.into_inner());
        if !same_actor(&op, &caller, kernel) {
            return DENIED.into();
        }
        if op.phase == Phase::Queued {
            drop(op);
            return retry_preparation(ctx, master, operation, &caller, kernel, false);
        }
    }
    if matches!(
        request,
        Request::Attach(..) | Request::Resolve(..) | Request::Execute(..) | Request::ExecFailed(..)
    ) {
        if let Request::Execute(_, phase) = &request
            && !renewal::execute_recorded(&operation, *phase)
        {
            return DENIED.into();
        }
        let reply = serve_launcher(ctx, master, &request, &caller, &operation, kernel);
        if let Request::Attach(_, phase) = &request
            && reply.starts_with("OK native-act state=launch ")
            && !renewal::persist_attached(&operation, *phase)
        {
            return "ERR native renewal receipt unavailable\n".into();
        }
        return reply;
    }
    // Account-file and native-history observations never run under Meta or an
    // operation predicate. The immutable seal and ordinary kernel guards below
    // bind this preflight to the exact plan and process.
    if matches!(request, Request::Signal(_)) {
        let (seal, signal_recorded) = {
            let op = operation.lock().unwrap_or_else(|p| p.into_inner());
            (op.authority.renewal.clone(), op.signal_sent)
        };
        // An already-recorded effect returns its old outcome below. Resume and
        // Execute still authorize every subsequent mutation independently.
        if !signal_recorded && seal.as_ref().is_some_and(|seal| !seal.preflight()) {
            return DENIED.into();
        }
    }
    if let Request::Resume(_, phase, _, _) = &request {
        let seal = operation
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .authority
            .renewal
            .clone();
        if seal.as_ref().is_some_and(|seal| !seal.continuing(*phase)) {
            return DENIED.into();
        }
    }
    let asked_signal = if matches!(request, Request::Signal(_)) {
        let op = operation.lock().unwrap_or_else(|p| p.into_inner());
        if op.request.asked.is_some() && !op.signal_sent {
            let NativePlan::Restart {
                process, forward, ..
            } = &op.request.plan
            else {
                return DENIED.into();
            };
            Some((forward.clone(), process.pid))
        } else {
            None
        }
    } else {
        None
    };
    // A recorded signal is observed, never re-authorized as a second effect. Fresh asked
    // signals recheck canonical execution before taking Meta/operation predicates.
    let _asked_account_guard = match asked_signal
        .as_ref()
        .map(|(command, pid)| kernel.account_execution(command, *pid, None))
        .transpose()
    {
        Ok(guard) => guard,
        Err(()) => return DENIED.into(),
    };
    // Retry disposal is deferred beyond both predicates, including early refusal paths.
    let mut retired_children = Vec::new();
    let mut retired_witnesses = Vec::new();
    let mut retired_resolutions = Vec::new();
    let mutation_meta = matches!(
        request,
        Request::Signal(_) | Request::Resume(..) | Request::Commit(..)
    )
    .then(|| ctx.meta.lock().unwrap_or_else(|p| p.into_inner()));
    let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
    if !same_actor(&op, &caller, kernel) {
        let asked = op.request.asked.is_some();
        drop(op);
        drop(mutation_meta);
        drop(_asked_account_guard);
        if asked {
            aterm_log::debug!("asked native completion refused stage=caller");
        }
        return DENIED.into();
    }
    match request {
        Request::Recover(_) => {
            if matches!(op.phase, Phase::Completed(_)) {
                drop(op);
                return completed_reply(&operation);
            }
            let Some(connection) = caller.connection else {
                return DENIED.into();
            };
            if !op.channel_closed || matches!(op.phase, Phase::Queued | Phase::Completed(_)) {
                return DENIED.into();
            }
            op.channel = Some(connection);
            op.channel_closed = false;
            admitted(&op.binding)
        }
        Request::Abort(_) => {
            if matches!(op.phase, Phase::Completed(_)) {
                drop(op);
                return completed_reply(&operation);
            }
            let before_mutation = match &op.request.plan {
                NativePlan::Restart { .. } => {
                    (!op.signal_sent || op.signal_accepted == Some(false)) && !op.resume_sent
                }
                NativePlan::OwnImage { child, .. } => {
                    op.authority.actor.as_ref().is_some_and(|actor| {
                        kernel.process(actor.pid).is_some_and(|actual| {
                            own_actor_matches(&actual, actor) && actual.image == actor.image
                        }) && kernel.process(child.pid).as_ref() == Some(child)
                    })
                }
            };
            if op.phase != Phase::Started || !before_mutation {
                return UNKNOWN.into();
            }
            let Some(completion) = op.seal_without_effect(kernel) else {
                return UNKNOWN.into();
            };
            drop(op);
            kernel.completion_sealed();
            finish(ctx, &operation, Some(completion))
        }
        Request::Reconcile(_) => {
            if matches!(op.phase, Phase::Completed(_)) {
                drop(op);
                return completed_reply(&operation);
            }
            if op.phase == Phase::Queued {
                return UNKNOWN.into();
            }
            if uncertain_quiescent(&op, kernel) {
                let completion = op.seal_completion(Decision::Unknown);
                drop(op);
                return finish(ctx, &operation, completion);
            }
            let Some(reason) = retirement(&op, kernel) else {
                return UNKNOWN.into();
            };
            let completion = op.seal_retirement(reason, kernel);
            drop(op);
            finish(ctx, &operation, completion)
        }
        Request::Query(_) => match op.phase {
            Phase::Completed(_) => {
                drop(op);
                completed_reply(&operation)
            }
            Phase::Started
                if matches!(op.request.asked, Some(AskedIntent::Relaunch(_)))
                    && op.signal_sent
                    && op.signal_accepted == Some(true) =>
            {
                "OK native-act state=signalled\n".into()
            }
            Phase::Started
                if op
                    .resume_phase
                    .is_some_and(|phase| launch_unissued(&op, phase, kernel)) =>
            {
                format!(
                    "OK native-act state=unissued phase={}\n",
                    op.resume_phase.unwrap().word()
                )
            }
            Phase::Started if op.authority.builtin => {
                let outcome = op
                    .resume_phase
                    .ok_or("query-resume-phase")
                    .and_then(|phase| observe_image_outcome(&op, master, phase, kernel));
                match outcome {
                    Ok(_) => format!(
                        "OK native-act state=resumed phase={}\n",
                        op.resume_phase.unwrap().word()
                    ),
                    Err(reason) => {
                        let asked = op.request.asked.is_some();
                        drop(op);
                        drop(mutation_meta);
                        drop(_asked_account_guard);
                        if asked {
                            aterm_log::debug!("asked native query refused stage={reason}");
                        }
                        UNKNOWN.into()
                    }
                }
            }
            Phase::Queued | Phase::Started | Phase::Precommitted(_) | Phase::Committing(_) => {
                UNKNOWN.into()
            }
        },
        Request::Precommit(_, phase) => {
            let outcome = if matches!(op.phase, Phase::Started | Phase::Precommitted(_)) {
                observe_image_outcome(&op, master, phase, kernel)
            } else {
                Err("precommit-phase")
            };
            match outcome {
                Ok(_) => {
                    op.phase = Phase::Precommitted(phase);
                    "OK native-act state=precommitted\n".into()
                }
                Err(reason) => {
                    let asked = op.request.asked.is_some();
                    drop(op);
                    drop(mutation_meta);
                    drop(_asked_account_guard);
                    if asked {
                        aterm_log::debug!("asked native precommit refused stage={reason}");
                    }
                    UNKNOWN.into()
                }
            }
        }
        Request::Commit(_, phase, version) => {
            if matches!(op.phase, Phase::Completed(_)) {
                drop(op);
                return completed_reply(&operation);
            }
            if op.authority.builtin {
                if !matches!(op.phase, Phase::Precommitted(p) if p == phase) {
                    let asked = op.request.asked.is_some();
                    drop(op);
                    drop(mutation_meta);
                    drop(_asked_account_guard);
                    if asked {
                        aterm_log::debug!("asked native commit refused stage=commit-phase");
                    }
                    return UNKNOWN.into();
                }
                let outcome = match observe_image_outcome(&op, master, phase, kernel) {
                    Ok(outcome) => outcome,
                    Err(reason) => {
                        let asked = op.request.asked.is_some();
                        drop(op);
                        drop(mutation_meta);
                        drop(_asked_account_guard);
                        if asked {
                            aterm_log::debug!("asked native commit refused stage={reason}");
                        }
                        return UNKNOWN.into();
                    }
                };
                let Some(meta) = mutation_meta.as_ref() else {
                    return UNKNOWN.into();
                };
                let mut capture = None;
                let mut capture_stage = "commit-context";
                if let Some(context) =
                    observe_control::context(ctx, meta, &caller, kernel).filter(|context| {
                        capture_stage = "commit-conversation";
                        if !op
                            .authority
                            .conversation
                            .as_ref()
                            .is_some_and(|guard| guard.current(ctx))
                        {
                            return false;
                        }
                        match op.outcome_slot.as_ref() {
                            Some(slot) => {
                                capture_stage = "commit-slot-stale";
                                slot.is_current(context, op.clock.now_us().unwrap_or(u64::MAX))
                            }
                            None => {
                                capture_stage = "commit-slot-absent";
                                false
                            }
                        }
                    })
                {
                    let Some(slot) = op.outcome_slot.take() else {
                        return UNKNOWN.into();
                    };
                    let Some(child) = op.children[phase_index(phase)].as_ref() else {
                        op.outcome_slot = Some(slot);
                        return UNKNOWN.into();
                    };
                    let Some(vendor) = kernel.process(child.process.pid) else {
                        op.outcome_slot = Some(slot);
                        return UNKNOWN.into();
                    };
                    let NativePlan::Restart { shell, .. } = &op.request.plan else {
                        unreachable!();
                    };
                    if child.death.outcome.get().is_none() {
                        let _installed = child.death.outcome.set(slot.hold());
                    }
                    let death: Arc<dyn crate::native_observe::DeathWitness> = child.death.clone();
                    match slot.capture_with(
                        &vendor,
                        shell,
                        &context,
                        master,
                        op.clock.now_us().unwrap_or(u64::MAX),
                        death,
                        &ObservationKernel(kernel),
                    ) {
                        Ok(captured) => {
                            capture = Some(captured);
                            capture_stage = "commit-captured";
                        }
                        Err(refused) => {
                            op.outcome_slot = Some(refused.slot);
                            retired_witnesses.push(refused.death);
                            return UNKNOWN.into();
                        }
                    }
                }
                op.commit_version = Some(version);
                let mut completion = op.seal_completion(Decision::Committed);
                if completion.is_some()
                    && phase == ImagePhase::Forward
                    && (matches!(op.request.plan, NativePlan::Restart { cold: Some(_), .. })
                        || op.request.asked.is_some())
                {
                    op.committed_image = Some(outcome);
                }
                if let Some(completion) = &mut completion {
                    completion.outcome_capture = capture;
                }
                drop(op);
                drop(mutation_meta);
                aterm_log::debug!("native completion observation stage={capture_stage}");
                kernel.completion_sealed();
                return finish_restart(ctx, master, &operation, completion, &caller, kernel);
            }
            // Final commit is sent on the original authoritative stream. A fresh proof stream
            // may precommit/query the SAME operation, never substitute for that stream here.
            if (!op.authority.builtin && (caller.connection != op.channel || op.channel_closed))
                || !matches!(op.phase, Phase::Precommitted(p) | Phase::Committing(p) if p == phase)
                || op
                    .commit_version
                    .as_ref()
                    .is_some_and(|expected| expected != &version)
                || !image_outcome(&op, master, phase, kernel)
            {
                return UNKNOWN.into();
            }
            if let NativePlan::OwnImage {
                version: expected, ..
            } = &op.request.plan
                && phase == ImagePhase::Forward
                && &version != expected
            {
                return UNKNOWN.into();
            }
            // Rebind only if no transition replaced the captured authority. Invalidation is
            // never undone by an old operation's successful continuation.
            // Seal the commit choice BEFORE releasing this lock to acquire MetaLock. A
            // concurrent proof/query can never promise rollback during an in-flight commit.
            op.phase = Phase::Committing(phase);
            op.commit_version = Some(version.clone());
            drop(op);
            drop(mutation_meta);
            let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
            if op.phase != Phase::Committing(phase)
                || (!op.authority.builtin && (caller.connection != op.channel || op.channel_closed))
                || !image_outcome(&op, master, phase, kernel)
            {
                return UNKNOWN.into();
            }
            if !op.authority.builtin
                && meta.supervisor_authority(crate::metrics::now_us()) == op.authority.snapshot
                && let Some(scoped) = meta
                    .supervisor
                    .as_mut()
                    .and_then(|claim| claim.scoped.as_mut())
            {
                let Some(version) = aterm_agent::harness::contract::ClaimVersion::parse(&version)
                else {
                    return UNKNOWN.into();
                };
                scoped.claim.version = Some(version);
                meta.supervisor_revision = meta.supervisor_revision.wrapping_add(1);
                op.claim_outcome = ClaimOutcome::Bound(meta.supervisor_revision);
            }
            let completion = op.seal_completion(Decision::Committed);
            drop(meta);
            drop(op);
            kernel.completion_sealed();
            let reply = finish(ctx, &operation, completion);
            ctx.watchers.ring_watchers();
            reply
        }
        Request::Signal(_) => {
            if !op.authority.asked_matches(&op.request) {
                return DENIED.into();
            }
            if op.resume_only || !op.authority.builtin || op.phase != Phase::Started {
                return DENIED.into();
            }
            if op.signal_sent {
                return if op.signal_accepted == Some(true) {
                    "OK native-act state=signalled\n"
                } else {
                    UNKNOWN
                }
                .into();
            }
            let NativePlan::Restart {
                process,
                shell,
                observation: None,
                ..
            } = &op.request.plan
            else {
                return DENIED.into();
            };
            if op.source_death.is_none()
                || ctx.human_input.seq() != op.human_seq
                || op
                    .authority
                    .renewal
                    .as_ref()
                    .is_some_and(|seal| !seal.current())
                || op
                    .authority
                    .conversation
                    .as_ref()
                    .is_none_or(|guard| !guard.current(ctx))
                || op.clock.remaining(op.request.deadline_us).is_none()
                || (!op.effect_started.load(std::sync::atomic::Ordering::Acquire)
                    && mutation_meta.as_ref().is_none_or(|meta| {
                        meta.supervisor_authority(crate::metrics::now_us()) != op.authority.snapshot
                    }))
                || kernel.process(process.pid).as_ref() != Some(process)
                || kernel.process(shell.pid).as_ref() != Some(shell)
                || kernel.foreground(master) != Some(process.group)
            {
                return DENIED.into();
            }
            // Mark dispatch before the syscall: retrying an ambiguous ACK never signals a
            // newly reused pid or performs a second mutation. Only this retained plan is used.
            let process = process.clone();
            // Kernel observations can outlive the earlier scalar checks. Fence the
            // original human sequence and deadline again at actual first dispatch.
            if ctx.human_input.seq() != op.human_seq
                || op.clock.remaining(op.request.deadline_us).is_none()
            {
                return DENIED.into();
            }
            if matches!(op.request.asked, Some(AskedIntent::Relaunch(_)))
                && op
                    .authority
                    .asked
                    .as_mut()
                    .is_none_or(|qualification| !qualification.consume_first_signal())
            {
                return DENIED.into();
            }
            op.signal_sent = true;
            if let Some(seal) = &op.authority.renewal {
                seal.did_signal();
            }
            op.effect_started
                .store(true, std::sync::atomic::Ordering::Release);
            drop(mutation_meta);
            let reply = kernel.signal(master, &process, ctx);
            op.signal_accepted = Some(reply.starts_with("OK "));
            if reply.starts_with("OK ") {
                "OK native-act state=signalled\n".into()
            } else {
                UNKNOWN.into()
            }
        }
        Request::Resume(_, phase, generation, expected_human_seq) => {
            if matches!(
                op.grant_record,
                GrantRecord::Writing(_) | GrantRecord::Uncertain(_)
            ) {
                return UNKNOWN.into();
            }
            if !op.authority.builtin
                || !matches!(op.phase, Phase::Started | Phase::Precommitted(_))
                || (!op.signal_sent && op.observation.is_none() && !op.resume_only)
            {
                return DENIED.into();
            }
            if op.resume_phase == Some(ImagePhase::Rollback) && phase == ImagePhase::Forward {
                return DENIED.into();
            }
            if phase == ImagePhase::Rollback && op.resume_phase != Some(ImagePhase::Rollback) {
                if op.grant_record == GrantRecord::Recorded
                    && !op.children[0]
                        .as_ref()
                        .is_some_and(|child| child.execute_sent)
                {
                    return UNKNOWN.into();
                }
                if op.children[0].as_ref().is_some_and(|child| {
                    child.execute_sent
                        && (!child.death.exited()
                            || kernel.group_gone(child.process.group) != Some(true))
                }) || op
                    .resume_attempt
                    .as_ref()
                    .is_none_or(|attempt| !attempt.settled())
                    || command_for(&op, phase).is_none()
                {
                    return UNKNOWN.into();
                }
                // Closing this phase rejects every delayed forward Attach/Execute. When no
                // Go was granted it does not assert an unregistered helper or cohort died.
                op.resume_phase = Some(phase);
                op.phase = Phase::Started;
                op.resume_sent = false;
                op.resume_attempt = None;
            }
            if op.resume_sent {
                if op
                    .resume_attempt
                    .as_ref()
                    .is_some_and(|attempt| attempt.unwritten())
                {
                    op.resume_sent = false;
                    op.resume_attempt = None;
                } else if launch_unissued(&op, phase, kernel) {
                    retired_children.extend(op.children[phase_index(phase)].take());
                    retired_resolutions.extend(op.resolved_cold.take());
                    op.cold_facts = None;
                    op.cold_resolving = false;
                    op.resume_sent = false;
                    op.resume_attempt = None;
                } else {
                    return UNKNOWN.into();
                }
            }
            let NativePlan::Restart {
                process,
                shell,
                launcher,
                socket,
                cold,
                forward,
                ..
            } = &op.request.plan
            else {
                return DENIED.into();
            };
            // Missing/unread facts are not proof of exit. The exact old birth must be gone,
            // while the original shell and its foreground still agree before any bytes start.
            if kernel.birth_alive(process) != Some(false)
                || op.authority.account.as_ref().is_some_and(|account| {
                    !account.current(ctx, &op.request, &caller, kernel)
                        || !account.proof().claimed_by(&op.request)
                })
                || kernel.group_gone(process.group) != Some(true)
                || kernel.process(shell.pid).as_ref() != Some(shell)
                || kernel.foreground(master) != Some(shell.group)
                || ctx.human_input.seq() != expected_human_seq
                || op
                    .authority
                    .conversation
                    .as_ref()
                    .is_none_or(|guard| !guard.current(ctx))
                || op.clock.remaining(op.request.deadline_us).is_none()
                || (!op.effect_started.load(std::sync::atomic::Ordering::Acquire)
                    && mutation_meta.as_ref().is_none_or(|meta| {
                        meta.supervisor_authority(crate::metrics::now_us()) != op.authority.snapshot
                    }))
            {
                return DENIED.into();
            }
            let line = if cold.is_some() && phase == ImagePhase::Forward {
                forward.cold_launcher_line(
                    launcher,
                    socket,
                    ctx.self_id.as_str(),
                    &op.binding,
                    phase,
                )
            } else {
                command_for(&op, phase).and_then(|command| {
                    command.launcher_line(
                        launcher,
                        socket,
                        ctx.self_id.as_str(),
                        &op.binding,
                        phase,
                    )
                })
            };
            let Some(line) = line else {
                return DENIED.into();
            };
            let turn = format!("if-gen={generation} idle=1 timeout=1000 {line}");
            let Some(custody) = op.custody.as_ref().cloned() else {
                return DENIED.into();
            };
            op.resume_sent = true;
            op.resume_phase = Some(phase);
            op.human_seq = expected_human_seq;
            let human_seq = expected_human_seq;
            let attempt = Arc::new(NativeInput::new(
                ctx,
                custody,
                human_seq,
                op.authority.snapshot.clone(),
                op.request.deadline_us,
                Arc::clone(&op.effect_started),
                op.authority.conversation.clone(),
                op.clock.clone(),
            ));
            op.resume_attempt = Some(Arc::clone(&attempt));
            drop(op);
            drop(mutation_meta);
            let carried = Carried::enter(ctx, Arc::clone(&attempt));
            let reply = resume(&turn);
            drop(carried);
            if attempt.unwritten() {
                return "OK native-act state=unwritten\n".into();
            }
            if reply.starts_with("OK ") && !reply.split_ascii_whitespace().any(|w| w == "skipped") {
                format!("OK native-act state=resumed phase={}\n", phase.word())
            } else {
                UNKNOWN.into()
            }
        }
        Request::Info
        | Request::Constructors
        | Request::RejectionDomain
        | Request::Prepare(_)
        | Request::CancelSupport
        | Request::CancelPrepare(_)
        | Request::Attach(..)
        | Request::Resolve(..)
        | Request::Execute(..)
        | Request::ExecFailed(..) => unreachable!(),
    }
}

/// EOF publishes native channel loss without retiring any actual custody. Only an explicit
/// same-operation recovery with its opaque capability and fresh kernel actor proof can rebind.
pub(crate) fn connection_closed(ctx: &crate::SessionCtx, connection: u64) {
    connection_closed_by(ctx, connection, &System(&ctx.native_policy, false));
}

fn connection_closed_by(ctx: &crate::SessionCtx, connection: u64, kernel: &dyn Kernel) {
    let operations: Vec<_> = {
        let mut table = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        table.coordinated_connections.remove(&connection);
        table.by_id.values().cloned().collect()
    };
    for operation in operations {
        let mut operation = operation.lock().unwrap_or_else(|p| p.into_inner());
        if operation.channel == Some(connection) {
            operation.channel_closed = true;
        }
    }
    reconcile_extinct_by(ctx, kernel);
}

/// Native coordination is explicitly selected by actual prepare, rather than retroactively
/// rejecting existing v3 clients. Once selected, omitting native words cannot regain general
/// signal/restart authority on this connection or the same kernel actor after release/expiry.
pub(crate) fn untyped_refusal(ctx: &crate::SessionCtx) -> Option<String> {
    let peer = crate::control::serving_peer_pid();
    let birth = peer
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(aterm_uds::process::read_birth)
        .map(|birth| birth.0);
    untyped_required(
        ctx,
        crate::control::serving_principal(),
        crate::control::serving_connection(),
        peer,
        birth,
    )
    .then(|| DENIED.into())
}

fn untyped_required(
    ctx: &crate::SessionCtx,
    principal: crate::control::Principal,
    connection: Option<u64>,
    peer: Option<u32>,
    birth: Option<u64>,
) -> bool {
    let table = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    (principal == crate::control::Principal::Harness && table.builtin_coordinated)
        || connection.is_some_and(|c| table.coordinated_connections.contains(&c))
        || peer.is_some_and(|pid| {
            table.coordinated_actors.iter().any(|(known, old_birth)| {
                *known == pid && birth.is_none_or(|birth| birth == *old_birth)
            })
        })
}

#[cfg(unix)]
fn process_birth_alive(pid: u32, birth: u64) -> Option<bool> {
    if pid <= 1 || birth == 0 {
        return None;
    }
    let pid = i32::try_from(pid).ok()?;
    if let Some(actual) = aterm_uds::process::read_birth(pid) {
        return Some(actual.0 == birth);
    }
    // SAFETY: signal zero observes existence without mutating the positive original pid.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        Some(false)
    } else {
        None
    }
}

#[cfg(not(unix))]
fn process_birth_alive(_: u32, _: u64) -> Option<bool> {
    None
}

thread_local! {
    static CONTINUATION: std::cell::RefCell<Option<(aterm_session::SessionId, aterm_session::LaunchNonce,
        std::sync::Weak<NativeInput>)>> = const { std::cell::RefCell::new(None) };
}

/// One exact constructor-owned input batch. The original gesture fence is checked at its
/// first actual write; admitted suffixes then finish under the same native custody.
#[derive(Debug)]
pub(crate) struct NativeInput {
    human: Arc<crate::human_input::HumanInputStamp>,
    expected: u64,
    ctx: std::sync::Weak<crate::SessionCtx>,
    authority: SupervisorAuthority,
    deadline_us: u64,
    effect_started: Arc<std::sync::atomic::AtomicBool>,
    conversation: Option<Conversation>,
    clock: Clock,
    state: Mutex<NativeInputState>,
}

#[derive(Debug)]
struct NativeInputState {
    custody: Option<Arc<NativeCustody>>,
    started: Option<bool>,
    sealed: bool,
    pending: usize,
    accepted: bool,
    uncertain: bool,
}

impl NativeInput {
    #[allow(clippy::too_many_arguments)]
    fn new(
        ctx: &Arc<crate::SessionCtx>,
        custody: Arc<NativeCustody>,
        expected: u64,
        authority: SupervisorAuthority,
        deadline_us: u64,
        effect_started: Arc<std::sync::atomic::AtomicBool>,
        conversation: Option<Conversation>,
        clock: Clock,
    ) -> Self {
        Self {
            human: Arc::clone(&ctx.human_input),
            expected,
            ctx: Arc::downgrade(ctx),
            authority,
            deadline_us,
            effect_started,
            conversation,
            clock,
            state: Mutex::new(NativeInputState {
                custody: Some(custody),
                started: None,
                sealed: false,
                pending: 0,
                accepted: false,
                uncertain: false,
            }),
        }
    }

    pub(crate) fn reserve(self: &Arc<Self>) -> Option<Arc<NativeInputWork>> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.sealed {
            return None;
        }
        state.pending = state.pending.checked_add(1)?;
        Some(Arc::new(NativeInputWork {
            input: Arc::clone(self),
            attempted: std::sync::atomic::AtomicBool::new(false),
            finished: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    pub(crate) fn producer(self: &Arc<Self>) -> NativeInputProducer {
        NativeInputProducer(Arc::clone(self))
    }

    pub(crate) fn seal(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.sealed = true;
        let custody = (state.pending == 0).then(|| state.custody.take()).flatten();
        drop(state);
        drop(custody);
    }

    fn unwritten(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.sealed && state.pending == 0 && !state.accepted && !state.uncertain
    }

    fn settled(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.sealed && state.pending == 0 && !state.uncertain
    }

    pub(crate) fn start(&self, nonparking: bool) -> bool {
        {
            let state = match self.state.try_lock() {
                Ok(state) => state,
                Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) if nonparking => return false,
                Err(std::sync::TryLockError::WouldBlock) => {
                    self.state.lock().unwrap_or_else(|p| p.into_inner())
                }
            };
            if let Some(started) = state.started {
                return started;
            }
        }
        let Some(ctx) = self.ctx.upgrade() else {
            return false;
        };
        let meta = if nonparking {
            match ctx.meta.try_lock() {
                Ok(meta) => meta,
                Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => return false,
            }
        } else {
            ctx.meta.lock().unwrap_or_else(|p| p.into_inner())
        };
        let mut state = if nonparking {
            match self.state.try_lock() {
                Ok(state) => state,
                Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => return false,
            }
        } else {
            self.state.lock().unwrap_or_else(|p| p.into_inner())
        };
        if state.custody.is_none() || state.pending == 0 {
            return false;
        }
        let started = *state.started.get_or_insert_with(|| {
            self.human.seq() == self.expected
                && self
                    .conversation
                    .as_ref()
                    .is_some_and(|guard| guard.current(&ctx))
                && self.clock.remaining(self.deadline_us).is_some()
                && (self
                    .effect_started
                    .load(std::sync::atomic::Ordering::Acquire)
                    || meta.supervisor_authority(crate::metrics::now_us()) == self.authority)
        });
        if started {
            self.effect_started
                .store(true, std::sync::atomic::Ordering::Release);
        }
        started
    }
}

pub(crate) struct NativeInputProducer(Arc<NativeInput>);

impl Drop for NativeInputProducer {
    fn drop(&mut self) {
        self.0.seal();
    }
}

pub(crate) struct NativeInputWork {
    input: Arc<NativeInput>,
    attempted: std::sync::atomic::AtomicBool,
    finished: std::sync::atomic::AtomicBool,
}

impl NativeInputWork {
    pub(crate) fn attempted(&self) {
        self.attempted
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn finish(&self, accepted: bool) {
        let mut state = self.input.state.lock().unwrap_or_else(|p| p.into_inner());
        state.accepted |= accepted;
        self.finished
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

impl Drop for NativeInputWork {
    fn drop(&mut self) {
        let mut state = self.input.state.lock().unwrap_or_else(|p| p.into_inner());
        state.uncertain |= self.attempted.load(std::sync::atomic::Ordering::Acquire)
            && !self.finished.load(std::sync::atomic::Ordering::Acquire);
        state.pending = state.pending.saturating_sub(1);
        let custody = (state.sealed && state.pending == 0)
            .then(|| state.custody.take())
            .flatten();
        drop(state);
        drop(custody);
    }
}

struct Carried {
    previous: Option<(
        aterm_session::SessionId,
        aterm_session::LaunchNonce,
        std::sync::Weak<NativeInput>,
    )>,
    _custody: Arc<NativeInput>,
}

impl Carried {
    fn enter(ctx: &crate::SessionCtx, custody: Arc<NativeInput>) -> Self {
        let previous = CONTINUATION.with(|slot| {
            slot.replace(Some((
                ctx.self_id.clone(),
                ctx.nonce,
                Arc::downgrade(&custody),
            )))
        });
        Self {
            previous,
            _custody: custody,
        }
    }
}

impl Drop for Carried {
    fn drop(&mut self) {
        self._custody.seal();
        CONTINUATION.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}

pub(crate) fn carried_custody(ctx: &crate::SessionCtx) -> Option<Arc<NativeInput>> {
    CONTINUATION.with(|slot| {
        let binding = slot.borrow();
        let (sid, nonce, custody) = binding.as_ref()?;
        (sid == &ctx.self_id && nonce == &ctx.nonce)
            .then(|| custody.upgrade())
            .flatten()
    })
}
