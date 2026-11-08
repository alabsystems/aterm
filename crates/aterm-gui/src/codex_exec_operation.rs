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

#[path = "codex_exec_monitor.rs"]
mod monitor;
#[path = "codex_exec_settlement.rs"]
mod settlement;

#[derive(Debug)]
pub(in crate::native_act) struct Operation {
    binding: Binding,
    source: ExecSource,
    successor: Option<ExecSource>,
    command: ExecCommand,
    checkpoint: String,
    proposal: codex_rotation::Proposal<ExecCommand>,
    preparation: Arc<Preparation>,
    tree: Option<aterm_agent::harness::codex_exec_handoff::tree::Admission>,
    gate: settlement::Execution,
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

fn admitted(
    ctx: &crate::SessionCtx,
    op: &settlement::State,
    authority: &SupervisorAuthority,
) -> bool {
    op.active
        && !op.terminate
        && op.tx.stage() != Stage::Cancelled
        && remaining(op.deadline).is_some()
        && ctx.fabric.hold().is_none()
        && ctx.native_policy.quota_rotation_enabled()
        && ctx.human_input.seq() == op.human_seq
        && *authority == op.authority
}

fn grant(launch: &Operation, op: &settlement::State, state: &str) -> String {
    grant_fields(
        &launch.binding,
        &launch.command,
        &launch.checkpoint,
        op,
        state,
    )
}

fn grant_fields(
    binding: &Binding,
    command: &ExecCommand,
    checkpoint: &str,
    op: &settlement::State,
    state: &str,
) -> String {
    let target = op.receipt.destination.as_ref().expect("quota destination");
    format!(
        "OK codex-exec-handoff state={state} id={} cap={} deadline_us={} thread={} turn={} source_account={} source_user={} target_account={} target_user={} auth={} checkpoint={}\n",
        binding.id,
        binding.capability,
        op.deadline,
        op.receipt.intent.thread,
        op.receipt.intent.wall,
        op.receipt.intent.identity.account,
        op.receipt.intent.identity.user,
        target.account,
        target.user,
        aterm_codec::hex::encode_lower(command.auth_path.as_bytes()),
        aterm_codec::hex::encode_lower(checkpoint.as_bytes())
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
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let authority = meta.supervisor_authority(crate::metrics::now_us());
    let mut launch = operation.lock().unwrap_or_else(|p| p.into_inner());
    let Operation {
        binding,
        source,
        successor,
        command,
        checkpoint,
        proposal,
        preparation,
        tree,
        gate,
    } = &mut *launch;
    let mut op = gate.lock();
    let env = op.environment(i64::try_from(now_ms() / 1000).unwrap_or(i64::MAX));
    if *id != binding.id
        || *cap != binding.capability
        || pid != source.process.pid
        || crate::quit_safety::foreground_pgrp(master) != i32::try_from(pid).unwrap_or(-1)
    {
        return DENIED.into();
    }
    if verb == "bind-tree" {
        if current != *source || op.tx.stage() != Stage::Reserved || !admitted(ctx, &op, &authority)
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
        if tree.as_ref().is_some_and(|tree| !tree.matches(&manifest)) {
            return DENIED.into();
        }
        let digest = manifest.digest().to_owned();
        tree.get_or_insert_with(|| {
            aterm_agent::harness::codex_exec_handoff::tree::Admission::new(manifest)
        });
        return format!("OK codex-exec-handoff state=tree-bound digest={digest}\n");
    }
    if verb == "admit-member" {
        if successor.as_ref() != Some(&current)
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
        let Some(tree) = tree.as_mut() else {
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
    if verb == "cancel" && current == *source {
        op.close(
            codex_renewal::CancellationKind::OwnerStop,
            ctx.human_input.seq(),
            false,
            std::time::Instant::now(),
        );
        return "OK codex-exec-handoff state=cancelled\n".into();
    }
    // A lost consume ACK may be reconciled only by that same successor, while
    // its claim still stands. It cannot authorize a later exec generation.
    if op.tx.stage() == Stage::Consumed(generation)
        && successor.as_ref() == Some(&current)
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
            grant_fields(binding, command, checkpoint, &op, "consumed")
        } else {
            DENIED.into()
        };
    }
    if !admitted(ctx, &op, &authority) {
        return DENIED.into();
    }
    match verb {
        "reserve" if current == *source => {
            if !matches!(op.tx.stage(), Stage::Armed | Stage::Reserved) {
                return DENIED.into();
            }
            if proposal.commit(&op.receipt, env.now).is_err() {
                // A healthy destination may still be another session's canary.
                // An invalid preparation must instead be retired and selected
                // again while the original quota-waiting turn is still alive.
                if codex_rotation::with_prepared_writer(&env, &op.receipt, |_, _| Ok(())).is_err() {
                    preparation.invalidate_selection(&op.receipt);
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
        "execute" if current == *source => {
            if !matches!(op.tx.stage(), Stage::Reserved | Stage::Executing) {
                return DENIED.into();
            }
            // The shared pool recognizes this receipt id/binding on retry, so
            // ACK loss never allocates another rotation or selects another row.
            if proposal.commit(&op.receipt, env.now).is_err() {
                return format!(
                    "OK codex-exec-handoff state=waiting deadline_us={}\n",
                    op.deadline
                );
            }
            let written = codex_rotation::publish_selected_committing(&env, &mut op.receipt);
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
            if current.process != source.process
                || current.config_home != source.config_home
                || current.command_for(&current.auth_path).as_ref() != Some(command)
            {
                return DENIED.into();
            }
            if successor.as_ref().is_some_and(|saved| *saved != current) {
                return DENIED.into();
            }
            if !matches!(
                codex_rotation::with_selected_writer(&env, &op.receipt, |_, _| Ok(())),
                Ok(Some(()))
            ) || !admitted(ctx, &op, &authority)
                || !op.tx.load(generation)
            {
                return DENIED.into();
            }
            *successor = Some(current);
            if verb == "load" {
                return grant_fields(binding, command, checkpoint, &op, "loaded");
            }
            let Some(connection) = crate::control::serving_connection() else {
                return DENIED.into();
            };
            let mut consumed = op.receipt.clone();
            consumed.phase = codex_renewal::Phase::Consumed;
            consumed.accepted_turn = Some(consumed.intent.wall.clone());
            consumed.successor_pid = Some(pid);
            consumed.successor_birth = Some(source.process.birth);
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
            grant_fields(binding, command, checkpoint, &op, "consumed")
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
        let launch = existing.lock().unwrap_or_else(|p| p.into_inner());
        let op = launch.gate.lock();
        if op.custody.is_some() {
            return if launch.source == source
                && op.receipt.intent == intent
                && launch.checkpoint == checkpoint
                && op.receipt.destination.as_ref().is_some_and(|target| {
                    target.account == target_account && target.user == target_user
                })
                && admitted(ctx, &op, &authority)
            {
                grant(&launch, &op, "armed")
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
    // Reserve capacity and start the receiver before a durable root exists.
    // Saturation or thread-start failure cannot strand a published operation.
    let Some(monitor) = monitor::Monitor::reserve() else {
        return DENIED.into();
    };
    let Some(connection) = crate::control::serving_connection() else {
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
    let (gate, cleanup) = settlement::Execution::new(
        env,
        proposal.destination.label.clone(),
        settlement::Initial {
            receipt,
            authority,
            source_connection: connection,
            human_seq: ctx.human_input.seq(),
            deadline,
            tx: Transaction::new(generation),
            custody: Arc::new(custody),
        },
    );
    let op = Arc::new(Mutex::new(Operation {
        binding: Binding { id, capability },
        source,
        successor: None,
        command,
        checkpoint,
        gate,
        proposal,
        preparation: Arc::clone(job),
        tree: None,
    }));
    {
        let launch = op.lock().unwrap_or_else(|p| p.into_inner());
        monitor.start(ctx, &launch.source, cleanup);
    }
    table.exec_operation = Some(Arc::clone(&op));
    drop(table);
    drop(meta);
    let wait = op
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .gate
        .lock()
        .custody
        .clone();
    if !wait
        .as_ref()
        .is_some_and(|custody| custody.await_predecessors(deadline))
    {
        let launch = op.lock().unwrap_or_else(|p| p.into_inner());
        launch.gate.lock().close(
            codex_renewal::CancellationKind::OwnerStop,
            ctx.human_input.seq(),
            false,
            std::time::Instant::now(),
        );
        return DENIED.into();
    }
    drop(wait);
    let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let launch = op.lock().unwrap_or_else(|p| p.into_inner());
    let mut op = launch.gate.lock();
    if !op.activate()
        || !admitted(
            ctx,
            &op,
            &meta.supervisor_authority(crate::metrics::now_us()),
        )
        || !launch.source.current()
    {
        op.close(
            codex_renewal::CancellationKind::ChangedRecord,
            ctx.human_input.seq(),
            false,
            std::time::Instant::now(),
        );
        return DENIED.into();
    }
    grant(&launch, &op, "armed")
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
    let launch = active.lock().unwrap_or_else(|p| p.into_inner());
    let mut op = launch.gate.lock();
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
                .gate
                .lock()
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
    let launch = active.lock().unwrap_or_else(|p| p.into_inner());
    let mut op = launch.gate.lock();
    if op.custody.is_none() {
        return false;
    }
    op.close(
        codex_renewal::CancellationKind::OwnerStop,
        ctx.human_input.seq(),
        true,
        std::time::Instant::now(),
    );
    true
}
