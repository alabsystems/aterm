// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual private observation lane. Watch creation and destruction occur outside every
//! metadata, native-operation and observation-store guard.

use super::*;
use aterm_agent::harness::native_observe::{Reply, Request};

fn unknown() -> String {
    Reply::Unknown.encode().expect("fixed observation reply")
}

pub(super) fn store(ctx: &crate::SessionCtx) -> Arc<Mutex<crate::native_observe::Store>> {
    Arc::clone(
        &ctx.fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .observations,
    )
}

/// The caller holds Meta only for capture/publication. This function opens no descriptor.
pub(super) fn context(
    ctx: &crate::SessionCtx,
    meta: &crate::session_timeline::SessionMeta,
    caller: &SupervisorInputCaller,
    kernel: &dyn Kernel,
) -> Option<crate::native_observe::Context> {
    if caller.principal != crate::control::Principal::Harness || caller.connection.is_none() {
        return None;
    }
    let actor = kernel.process(caller.peer?)?;
    Some(crate::native_observe::Context {
        session: ctx.self_id.clone(),
        nonce: ctx.nonce,
        host_epoch: host_epoch()?.to_string(),
        caller: caller.clone(),
        actor,
        authority: meta.supervisor_authority(crate::metrics::now_us()),
        human_seq: ctx.human_input.seq(),
        conversation: kernel.record_key(ctx)?,
    })
}

pub(crate) fn serve(ctx: &Arc<crate::SessionCtx>, master: i32, tail: &str) -> String {
    let Some(request) = Request::parse(tail) else {
        return "ERR usage: native-observe <register|refresh> v=1 <bounded fields>\n".into();
    };
    let caller = SupervisorInputCaller {
        principal: crate::control::serving_principal(),
        connection: crate::control::serving_connection(),
        peer: crate::control::serving_peer_pid(),
        proof: None,
    };
    let Some(now) = Clock::System.now_us() else {
        return unknown();
    };
    let initial = {
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        context(ctx, &meta, &caller, &System(&ctx.native_policy))
    };
    let Some(initial) = initial else {
        return unknown();
    };
    let observations = store(ctx);
    match request {
        Request::Refresh(binding) => {
            let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            let Some(fresh) = context(ctx, &meta, &caller, &System(&ctx.native_policy)) else {
                return unknown();
            };
            let reply = observations
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .refresh(&binding, &fresh, master, now);
            drop(meta);
            reply.encode().unwrap_or_else(unknown)
        }
        Request::Register(registration) => {
            let (mut reservation, retired) = observations
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .reserve(&registration, &initial, now)
                .into_parts();
            let reclaimed = !retired.is_empty();
            drop(retired);
            if reservation.is_none() && reclaimed {
                let (next, retired) = observations
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .reserve(&registration, &initial, now)
                    .into_parts();
                drop(retired);
                reservation = next;
            }
            // The optional preparation is declared before Meta so every early return also
            // disposes its watch after the predicate; arm never runs beneath any guard.
            let mut prepared = reservation.and_then(|reservation| reservation.arm(master));
            if prepared.is_none() {
                return unknown();
            }
            let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            let Some(fresh) = context(ctx, &meta, &caller, &System(&ctx.native_policy)) else {
                return unknown();
            };
            let Some(now) = Clock::System.now_us() else {
                return unknown();
            };
            let (reply, retired) = observations
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .publish(
                    prepared.take().expect("armed preparation"),
                    &fresh,
                    master,
                    now,
                )
                .into_parts();
            drop(meta);
            drop(retired);
            reply.encode().unwrap_or_else(unknown)
        }
    }
}
