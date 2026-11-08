// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native custody for one cooperative, same-PID account replacement. Selection
//! stays in the shared pool. A checkpoint and a kernel image are never grants
//! without this live operation and its original claim/owner fence.

use super::*;
use crate::session_timeline::{NativeCustody, SupervisorAuthority};
use aterm_agent::harness::codex_exec_handoff::transaction::{Stage, Transaction};
use aterm_agent::harness::native_act::Binding;
use std::collections::BTreeMap;

#[derive(Debug)]
pub(in crate::native_act) struct Operation {
    binding: Binding,
    source: ExecSource,
    successor: Option<ExecSource>,
    command: ExecCommand,
    checkpoint: String,
    receipt: codex_renewal::Receipt,
    proposal: codex_rotation::Proposal<ExecCommand>,
    preparation: Arc<Preparation>,
    authority: SupervisorAuthority,
    source_connection: u64,
    source_disconnected: bool,
    human_seq: u64,
    deadline: u64,
    tx: Transaction,
    tree: Option<aterm_agent::harness::codex_exec_handoff::tree::Admission>,
    custody: Option<Arc<NativeCustody>>,
    terminate: bool,
}

fn fields(line: &str) -> Option<(&str, BTreeMap<&str, &str>)> {
    let mut words = line.split_ascii_whitespace();
    let verb = words.next()?;
    let mut fields = BTreeMap::new();
    for word in words {
        let (key, value) = word.split_once('=')?;
        if fields.insert(key, value).is_some() {
            return None;
        }
    }
    Some((verb, fields))
}

fn checkpoint(encoded: &str, intent: &codex_renewal::Intent) -> Option<String> {
    if encoded.len() > 128 * 1024 {
        return None;
    }
    let text = String::from_utf8(aterm_codec::hex::decode_vec(encoded.as_bytes())?).ok()?;
    let value: aterm_json::Value = aterm_json::from_str(&text).ok()?;
    if value.get("thread")?.as_str()? != intent.thread
        || value.get("turn")?.as_str()? != intent.wall
    {
        return None;
    }
    // Retain exactly the checkpoint bytes the old runtime supplied. Core still
    // validates this metadata against durable history before native consumption.
    (aterm_codec::hex::encode_lower(text.as_bytes()) == encoded).then_some(text)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |n| u64::try_from(n.as_millis()).unwrap_or(u64::MAX))
}

fn environment() -> Option<Env> {
    Env::from_process().ok()
}

fn admitted(ctx: &crate::SessionCtx, op: &Operation, authority: &SupervisorAuthority) -> bool {
    !op.terminate
        && op.tx.stage() != Stage::Cancelled
        && remaining(op.deadline).is_some()
        && ctx.fabric.hold().is_none()
        && ctx.native_policy.quota_rotation_enabled()
        && ctx.human_input.seq() == op.human_seq
        && *authority == op.authority
}

fn grant(op: &Operation, state: &str) -> String {
    let target = op.receipt.destination.as_ref().expect("quota destination");
    format!(
        "OK codex-exec-handoff state={state} id={} cap={} deadline_us={} thread={} turn={} source_account={} source_user={} target_account={} target_user={} auth={} checkpoint={}\n",
        op.binding.id,
        op.binding.capability,
        op.deadline,
        op.receipt.intent.thread,
        op.receipt.intent.wall,
        op.receipt.intent.identity.account,
        op.receipt.intent.identity.user,
        target.account,
        target.user,
        aterm_codec::hex::encode_lower(op.command.auth_path.as_bytes()),
        aterm_codec::hex::encode_lower(op.checkpoint.as_bytes())
    )
}

pub(super) fn serve(ctx: &Arc<crate::SessionCtx>, master: i32, line: &str) -> String {
    if line.trim() == "tree-info" {
        return "OK codex-exec-handoff tree-contract=1\n".into();
    }
    if line.trim() == "info" {
        return "OK codex-exec-handoff contract=3\n".into();
    }
    let Some((verb, fields)) = fields(line) else {
        return DENIED.into();
    };
    if verb == "arm" {
        return arm(ctx, master, &fields);
    }
    let expected_fields = match verb {
        "reserve" | "execute" | "load" | "consume" | "cancel" | "complete" => 2,
        "bind-tree" => 3,
        "admit-member" => 5,
        _ => return DENIED.into(),
    };
    if fields.len() != expected_fields {
        return DENIED.into();
    }
    let (Some(id), Some(cap)) = (fields.get("id"), fields.get("cap")) else {
        return DENIED.into();
    };
    let operation = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .exec_operation
        .clone();
    let Some(operation) = operation else {
        return DENIED.into();
    };
    let Some(pid) = crate::control::serving_peer_pid() else {
        return DENIED.into();
    };
    let Some(current) = ExecSource::capture(pid) else {
        return DENIED.into();
    };
    let Some(generation) = current.exec_generation else {
        return DENIED.into();
    };
    let Some(env) = environment() else {
        return DENIED.into();
    };
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let authority = meta.supervisor_authority(crate::metrics::now_us());
    let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
    if *id != op.binding.id
        || *cap != op.binding.capability
        || pid != op.source.process.pid
        || crate::quit_safety::foreground_pgrp(master) != i32::try_from(pid).unwrap_or(-1)
    {
        return DENIED.into();
    }
    if verb == "bind-tree" {
        if current != op.source
            || op.tx.stage() != Stage::Reserved
            || !admitted(ctx, &op, &authority)
        {
            return DENIED.into();
        }
        let Some(encoded) = fields.get("manifest") else {
            return DENIED.into();
        };
        if encoded.len() > aterm_agent::harness::native_act::MAX_REQUEST {
            return DENIED.into();
        }
        let Some(bytes) = aterm_codec::hex::decode_vec(encoded.as_bytes()) else {
            return DENIED.into();
        };
        let Ok(json) = String::from_utf8(bytes) else {
            return DENIED.into();
        };
        let Some(manifest) = aterm_agent::harness::codex_exec_handoff::tree::Manifest::decode(
            &json,
            &op.receipt.intent.thread,
            &op.receipt.intent.wall,
        ) else {
            return DENIED.into();
        };
        if op
            .tree
            .as_ref()
            .is_some_and(|tree| !tree.matches(&manifest))
        {
            return DENIED.into();
        }
        let digest = manifest.digest().to_owned();
        op.tree.get_or_insert_with(|| {
            aterm_agent::harness::codex_exec_handoff::tree::Admission::new(manifest)
        });
        return format!("OK codex-exec-handoff state=tree-bound digest={digest}\n");
    }
    if verb == "admit-member" {
        if op.successor.as_ref() != Some(&current)
            || !matches!(op.tx.stage(), Stage::Loaded(bound) | Stage::Consumed(bound) if bound == generation)
            || !admitted(ctx, &op, &authority)
        {
            return DENIED.into();
        }
        // A consumed root may have retired its pool deduplication row. Both
        // paths revalidate the selected account under the shared store writer;
        // neither a root's prior admission nor a cached child ACK grants this call.
        let selection = if matches!(op.tx.stage(), Stage::Consumed(_)) {
            codex_rotation::with_consumed_member_writer(&env, &op.receipt, |_, _| Ok(()))
        } else {
            codex_rotation::with_selected_writer(&env, &op.receipt, |_, _| Ok(()))
        };
        if !matches!(selection, Ok(Some(()))) || !admitted(ctx, &op, &authority) {
            return DENIED.into();
        }
        let (Some(digest), Some(thread), Some(encoded_turn)) = (
            fields.get("digest"),
            fields.get("thread"),
            fields.get("turn"),
        ) else {
            return DENIED.into();
        };
        let turn = if *encoded_turn == "-" {
            None
        } else {
            let Some(bytes) = aterm_codec::hex::decode_vec(encoded_turn.as_bytes()) else {
                return DENIED.into();
            };
            let Ok(turn) = String::from_utf8(bytes) else {
                return DENIED.into();
            };
            Some(turn)
        };
        let Some(tree) = op.tree.as_mut() else {
            return DENIED.into();
        };
        let Some(fresh) = tree.admit(digest, thread, turn.as_deref()) else {
            return DENIED.into();
        };
        return format!(
            "OK codex-exec-handoff state=member-admitted digest={digest} thread={thread} turn={encoded_turn} fresh={}\n",
            u8::from(fresh)
        );
    }
    if verb == "cancel" && current == op.source {
        if op.tx.cancel() && !op.receipt.launch_attempted {
            match codex_renewal::cancel_unexecuted_rotation(
                &env,
                &op.receipt,
                &op.proposal.destination.label,
            ) {
                Ok(Some(retired)) => {
                    op.receipt = retired;
                    op.custody.take();
                }
                _ => return DENIED.into(),
            }
        }
        return "OK codex-exec-handoff state=cancelled\n".into();
    }
    // A lost consume ACK may be reconciled only by that same successor, while
    // its claim still stands. It cannot authorize a later exec generation.
    if op.tx.stage() == Stage::Consumed(generation)
        && op.successor.as_ref() == Some(&current)
        && authority.identity() == op.authority.identity()
        && !op.terminate
        && ctx.fabric.hold().is_none()
        && ctx.native_policy.quota_rotation_enabled()
        && ctx.human_input.seq() == op.human_seq
    {
        if verb == "complete" {
            return match codex_renewal::record_completed_rotation(&env, &op.receipt) {
                Ok(Some(())) => "OK codex-exec-handoff state=completed\n".into(),
                Ok(None) => "OK codex-exec-handoff state=waiting\n".into(),
                Err(_) => DENIED.into(),
            };
        }
        let _ = codex_renewal::retire_pool_record(&env, &op.receipt);
        return if matches!(verb, "load" | "consume") {
            grant(&op, "consumed")
        } else {
            DENIED.into()
        };
    }
    if !admitted(ctx, &op, &authority) {
        return DENIED.into();
    }
    match verb {
        "reserve" if current == op.source => {
            if !matches!(op.tx.stage(), Stage::Armed | Stage::Reserved) {
                return DENIED.into();
            }
            if op.proposal.commit(&op.receipt, env.now).is_err() {
                // A healthy destination may still be another session's canary.
                // An invalid preparation must instead be retired and selected
                // again while the original quota-waiting turn is still alive.
                if codex_rotation::with_prepared_writer(&env, &op.receipt, |_, _| Ok(())).is_err() {
                    op.preparation.invalidate_selection(&op.receipt);
                    return DENIED.into();
                }
                return format!(
                    "OK codex-exec-handoff state=waiting deadline_us={}\n",
                    op.deadline
                );
            }
            if !admitted(ctx, &op, &authority) || !op.tx.reserve(generation) {
                return DENIED.into();
            }
            format!(
                "OK codex-exec-handoff state=reserved deadline_us={}\n",
                op.deadline
            )
        }
        "execute" if current == op.source => {
            if !matches!(op.tx.stage(), Stage::Reserved | Stage::Executing) {
                return DENIED.into();
            }
            // The shared pool recognizes this receipt id/binding on retry, so
            // ACK loss never allocates another rotation or selects another row.
            if op.proposal.commit(&op.receipt, env.now).is_err() {
                return format!(
                    "OK codex-exec-handoff state=waiting deadline_us={}\n",
                    op.deadline
                );
            }
            op.receipt.phase = codex_renewal::Phase::Committing;
            op.receipt.launch_attempted = true;
            let written = codex_rotation::with_selected_writer(&env, &op.receipt, |_, _| {
                codex_renewal::write_receipt(&env, &op.receipt)
            });
            if !matches!(written, Ok(Some(()))) {
                return format!(
                    "OK codex-exec-handoff state=waiting deadline_us={}\n",
                    op.deadline
                );
            }
            if !admitted(ctx, &op, &authority) || !op.tx.execute(generation) {
                return DENIED.into();
            }
            format!(
                "OK codex-exec-handoff state=execute deadline_us={}\n",
                op.deadline
            )
        }
        "load" | "consume" => {
            if current.process != op.source.process
                || current.config_home != op.source.config_home
                || current.command_for(&current.auth_path).as_ref() != Some(&op.command)
                || !op.tx.load(generation)
            {
                return DENIED.into();
            }
            if op.successor.as_ref().is_some_and(|saved| *saved != current) {
                return DENIED.into();
            }
            op.successor = Some(current);
            if !matches!(
                codex_rotation::with_selected_writer(&env, &op.receipt, |_, _| Ok(())),
                Ok(Some(()))
            ) {
                return DENIED.into();
            }
            if verb == "load" {
                return grant(&op, "loaded");
            }
            let Some(connection) = crate::control::serving_connection() else {
                return DENIED.into();
            };
            let mut consumed = op.receipt.clone();
            consumed.phase = codex_renewal::Phase::Consumed;
            consumed.accepted_turn = Some(consumed.intent.wall.clone());
            consumed.successor_pid = Some(pid);
            consumed.successor_birth = Some(op.source.process.birth);
            if !matches!(
                codex_rotation::with_selected_writer(&env, &op.receipt, |_, _| {
                    codex_renewal::write_receipt(&env, &consumed)
                }),
                Ok(Some(()))
            ) {
                return DENIED.into();
            }
            if !admitted(ctx, &op, &authority) {
                return DENIED.into();
            }
            let Some(transferred) = meta.transfer_native_exec_claim(
                &op.authority,
                connection,
                crate::metrics::now_us(),
            ) else {
                return DENIED.into();
            };
            op.authority = transferred;
            op.receipt = consumed;
            if !op.tx.consume(generation) {
                return DENIED.into();
            }
            op.custody.take();
            let _ = codex_renewal::retire_pool_record(&env, &op.receipt);
            grant(&op, "consumed")
        }
        _ => DENIED.into(),
    }
}

fn arm(ctx: &Arc<crate::SessionCtx>, master: i32, fields: &BTreeMap<&str, &str>) -> String {
    if fields.len() != 7 {
        return DENIED.into();
    }
    let get = |name| fields.get(name).copied();
    let (
        Some(thread),
        Some(turn),
        Some(account),
        Some(user),
        Some(target_account),
        Some(target_user),
        Some(encoded),
    ) = (
        get("thread"),
        get("turn"),
        get("source_account"),
        get("source_user"),
        get("target_account"),
        get("target_user"),
        get("checkpoint"),
    )
    else {
        return DENIED.into();
    };
    let Some(codex_renewal::Request::Prepare(intent)) = codex_renewal::Request::parse_rotation(
        &format!("prepare thread={thread} turn={turn} source_account={account} source_user={user}"),
    ) else {
        return DENIED.into();
    };
    let Some(checkpoint) = checkpoint(encoded, &intent) else {
        return DENIED.into();
    };
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
    let Some(generation) = source.exec_generation else {
        return DENIED.into();
    };
    let Some(env) = environment() else {
        return DENIED.into();
    };
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let authority = meta.supervisor_authority(crate::metrics::now_us());
    let mut table = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if let Some(existing) = &table.exec_operation {
        let op = existing.lock().unwrap_or_else(|p| p.into_inner());
        if op.custody.is_some() {
            return if op.source == source
                && op.receipt.intent == intent
                && op.checkpoint == checkpoint
                && op.receipt.destination.as_ref().is_some_and(|target| {
                    target.account == target_account && target.user == target_user
                })
                && admitted(ctx, &op, &authority)
            {
                grant(&op, "armed")
            } else {
                DENIED.into()
            };
        }
    }
    if authority.identity() != Some(&proof)
        || authority.contract() != Some(2)
        || authority.connection() != crate::control::serving_connection()
        || ctx.fabric.hold().is_some()
        || !ctx.native_policy.quota_rotation_enabled()
    {
        return DENIED.into();
    }
    let Some(job) = table.exec_prepare.as_ref() else {
        return DENIED.into();
    };
    if !job.matches(
        &source,
        &intent,
        &proof,
        authority.revision(),
        ctx.human_input.seq(),
    ) {
        return DENIED.into();
    }
    let progress = job.progress.lock().unwrap_or_else(|p| p.into_inner());
    let Progress::Selected(proposal, observed) = &*progress else {
        return DENIED.into();
    };
    if observed.elapsed() >= Duration::from_secs(60)
        || proposal.destination_identity.account != target_account
        || proposal.destination_identity.user != target_user
    {
        return DENIED.into();
    }
    let proposal = (**proposal).clone();
    drop(progress);
    let Some(command) = proposal.plan.prepared_command().cloned() else {
        return DENIED.into();
    };
    let Ok(id) = aterm_agent::harness::native_act::operation_id() else {
        return DENIED.into();
    };
    let (Ok(a), Ok(b)) = (
        aterm_agent::harness::native_act::operation_id(),
        aterm_agent::harness::native_act::operation_id(),
    ) else {
        return DENIED.into();
    };
    let capability = format!("{a}{b}");
    let Some(deadline) = aterm_agent::harness::native_act::monotonic_us()
        .and_then(|now| now.checked_add(120_000_000))
    else {
        return DENIED.into();
    };
    let Some(epoch) = host_epoch() else {
        return DENIED.into();
    };
    let Some(encoded_command) = command.encode() else {
        return DENIED.into();
    };
    let Some(selected_generation) = proposal.plan.selected_generation() else {
        return DENIED.into();
    };
    let binding_data = aterm_json::json!({"kind":"codex-exec", "host":epoch,
        "command":aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(encoded_command.as_bytes())),
        "checkpoint":aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(checkpoint.as_bytes())),
        "generation":generation});
    let receipt = codex_renewal::Receipt {
        destination: Some(proposal.destination_identity.clone()),
        id: id.clone(),
        intent,
        sid: ctx.self_id.as_str().into(),
        old_pid: pid,
        old_birth: source.process.birth,
        claim_revision: authority.revision(),
        human_seq: ctx.human_input.seq(),
        old_auth: proposal.old_auth.clone(),
        new_auth: proposal.new_auth.clone(),
        generation: selected_generation,
        row_stamp: codex_renewal::row_stamp(&proposal.destination),
        native_prepare: binding_data.to_string(),
        expires_ms: now_ms().saturating_add(120_000),
        phase: codex_renewal::Phase::Ready,
        successor_pid: None,
        successor_birth: None,
        accepted_turn: None,
        launch_attempted: false,
        candidate: None,
        current_attempt: None,
        revoked: false,
    };
    let Some(boundary) = aterm_types::update_boundary::Boundary::process()
        .try_hold(aterm_types::update_boundary::GuardedItem::HarnessNativeMutation)
    else {
        return DENIED.into();
    };
    let Ok(custody) = ctx
        .fenced_writes
        .reserve_native(authority.revision(), &id, boundary)
    else {
        return DENIED.into();
    };
    if !matches!(
        codex_rotation::with_prepared_writer(&env, &receipt, |_, writer| writer.create(&receipt)),
        Ok(Some(()))
    ) {
        // Preparation can become stale before an operation exists. Do not
        // keep advertising that rejected selection until its cache expires.
        job.invalidate_selection(&receipt);
        return DENIED.into();
    }
    let Some(connection) = crate::control::serving_connection() else {
        return DENIED.into();
    };
    let op = Arc::new(Mutex::new(Operation {
        binding: Binding { id, capability },
        source,
        successor: None,
        command,
        checkpoint,
        receipt,
        proposal,
        preparation: Arc::clone(job),
        authority,
        source_connection: connection,
        source_disconnected: false,
        human_seq: ctx.human_input.seq(),
        deadline,
        tx: Transaction::new(generation),
        tree: None,
        custody: Some(Arc::new(custody)),
        terminate: false,
    }));
    table.exec_operation = Some(Arc::clone(&op));
    drop(table);
    drop(meta);
    // Never leave a native marker without a monitor able to retire it after the
    // captured process dies. A failed monitor launch cancels before any Go.
    if !monitor(ctx, &op) {
        let mut op = op.lock().unwrap_or_else(|p| p.into_inner());
        op.tx.cancel();
        op.custody.take();
        return DENIED.into();
    }
    let wait = op.lock().unwrap_or_else(|p| p.into_inner()).custody.clone();
    if !wait
        .as_ref()
        .is_some_and(|custody| custody.await_predecessors(deadline))
    {
        let mut op = op.lock().unwrap_or_else(|p| p.into_inner());
        op.tx.cancel();
        op.custody.take();
        return DENIED.into();
    }
    drop(wait);
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let op = op.lock().unwrap_or_else(|p| p.into_inner());
    if !admitted(
        ctx,
        &op,
        &meta.supervisor_authority(crate::metrics::now_us()),
    ) || !op.source.current()
    {
        return DENIED.into();
    }
    grant(&op, "armed")
}

pub(in crate::native_act) fn retain_claim(ctx: &crate::SessionCtx, connection: u64) -> bool {
    let active = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .exec_operation
        .clone();
    let Some(active) = active else { return false };
    let mut op = active.lock().unwrap_or_else(|p| p.into_inner());
    if op.source_connection != connection || op.custody.is_none() {
        return false;
    }
    op.source_disconnected = true;
    true
}

pub(in crate::native_act) fn pending(ctx: &crate::SessionCtx) -> bool {
    ctx.fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .exec_operation
        .as_ref()
        .is_some_and(|op| {
            op.lock()
                .unwrap_or_else(|p| p.into_inner())
                .custody
                .is_some()
        })
}

pub(in crate::native_act) fn cancel_owner(ctx: &crate::SessionCtx) -> bool {
    let _meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let active = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .exec_operation
        .clone();
    let Some(active) = active else { return false };
    let mut op = active.lock().unwrap_or_else(|p| p.into_inner());
    if op.custody.is_none() {
        return false;
    }
    op.terminate = true;
    op.tx.cancel();
    true
}

fn monitor(ctx: &Arc<crate::SessionCtx>, op: &Arc<Mutex<Operation>>) -> bool {
    let weak = Arc::downgrade(ctx);
    let operation = Arc::clone(op);
    std::thread::Builder::new()
        .name("codex-exec-custody".into())
        .spawn(move || {
            loop {
                let Some(ctx) = weak.upgrade() else { return };
                let authority = ctx
                    .meta
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .supervisor_authority(crate::metrics::now_us());
                let (source, terminate, disconnected, connection, retired) = {
                    let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
                    if op.custody.is_none() {
                        let release = op.source_disconnected;
                        let connection = op.source_connection;
                        drop(op);
                        if release {
                            crate::session_timeline::publish_release_supervisor(
                                &ctx,
                                Some(connection),
                                crate::metrics::now_us(),
                                aterm_slog::Actor::Aterm,
                            )
                            .published();
                            crate::session_timeline::close_claim_connection(
                                &ctx,
                                connection,
                                crate::metrics::now_us(),
                            );
                        }
                        return;
                    }
                    let revoked = remaining(op.deadline).is_none()
                        || ctx.fabric.hold().is_some()
                        || ctx.human_input.seq() != op.human_seq
                        || authority != op.authority
                        || !ctx.native_policy.quota_rotation_enabled();
                    // A prior client cancellation retains any issued Go. An
                    // ungranted source may still be writing its checkpoint;
                    // revocation alone cannot interrupt that durable write.
                    if revoked && op.tx.revoke() {
                        op.terminate = true;
                    }
                    let retired =
                        (op.tx.stage() == Stage::Cancelled && !op.receipt.revoked).then(|| {
                            if op.receipt.launch_attempted {
                                op.receipt.revoked = true;
                                op.receipt.phase = codex_renewal::Phase::Retired;
                            }
                            op.receipt.clone()
                        });
                    (
                        op.source.clone(),
                        op.terminate,
                        op.source_disconnected,
                        op.source_connection,
                        retired,
                    )
                };
                if let Some(receipt) = retired
                    && let Some(env) = environment()
                {
                    if !receipt.launch_attempted {
                        let target = operation
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .proposal
                            .destination
                            .label
                            .clone();
                        if let Ok(Some(retired)) =
                            codex_renewal::cancel_unexecuted_rotation(&env, &receipt, &target)
                        {
                            let mut op = operation.lock().unwrap_or_else(|p| p.into_inner());
                            op.receipt = retired;
                            op.custody.take();
                        }
                    } else {
                        let _ = codex_renewal::with_receipt_writer(&env, |_| {
                            codex_renewal::write_receipt(&env, &receipt)
                        });
                        let _ = codex_renewal::retire_pool_record(&env, &receipt);
                    }
                }
                let pid = i32::try_from(source.process.pid).unwrap_or(-1);
                let gone = aterm_uds::process::read_birth(pid)
                    .is_some_and(|birth| birth.0 != source.process.birth)
                    || matches!(
                        aterm_uds::process::process_presence(pid),
                        aterm_uds::process::ProcessPresence::Absent
                            | aterm_uds::process::ProcessPresence::Exited
                    );
                if gone {
                    operation
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .custody
                        .take();
                    if disconnected {
                        crate::session_timeline::publish_release_supervisor(
                            &ctx,
                            Some(connection),
                            crate::metrics::now_us(),
                            aterm_slog::Actor::Aterm,
                        )
                        .published();
                        crate::session_timeline::close_claim_connection(
                            &ctx,
                            connection,
                            crate::metrics::now_us(),
                        );
                    }
                    return;
                }
                if terminate
                    && let Some(current) = ExecSource::capture(source.process.pid)
                    && current.process == source.process
                    && let Some(identity) = aterm_uds::process::read_identity(pid)
                {
                    let _ = aterm_uds::process::end_proven(
                        identity,
                        Duration::from_secs(1),
                        &mut aterm_uds::process::EndControlLive,
                        |_| {
                            ExecSource::capture(source.process.pid)
                                .is_some_and(|now| now.process == source.process)
                        },
                    );
                }
                std::thread::park_timeout(Duration::from_millis(100));
            }
        })
        .is_ok()
}
