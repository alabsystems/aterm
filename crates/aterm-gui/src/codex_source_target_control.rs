// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Destination construction is counted before exposing transport Waiting.

use super::use_control::{cached, error};
use super::*;

/// Release-only: actual socket identity and retained native OPEN custody are
/// sufficient even when foreground or ordinary app work admission has ended.
pub(super) fn serve_cleanup(
    ctx: &Arc<crate::SessionCtx>,
    request: Request,
    registry: &Arc<registry::Registry>,
    id: u64,
) -> String {
    let caller = match identify(ctx) {
        Ok(caller) => caller,
        Err(why) => return error(why),
    };
    let Some(epoch) = host_epoch() else {
        return UNAVAILABLE.into();
    };
    let connection = match registry.connection(id) {
        Ok(value) => value,
        Err(why) => return error(why),
    };
    let (job, _) = match admit_job(id, &caller, &request) {
        Ok(value) => value,
        Err(AdmissionError::Changed) => return "ERR codex-auth request changed\n".into(),
        Err(AdmissionError::Stale) => return "ERR codex-auth stale\n".into(),
        Err(AdmissionError::Capacity) => return UNAVAILABLE.into(),
    };
    if cached(&job).is_none() {
        let mut prepared = job
            .prepared_target_cleanup
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let running = matches!(
            *job.progress.lock().unwrap_or_else(|e| e.into_inner()),
            Progress::Running { .. }
        );
        if prepared.is_none() && !running {
            let Action::CloseTargetOpen {
                open_operation,
                assignment,
                context,
            } = &request.action
            else {
                return error(registry::Error::Denied);
            };
            let command = match registry.prepare_target_open_retirement(
                &caller.caller,
                &connection,
                registry::TargetOpenRetirement {
                    operation: &request.operation,
                    open_operation,
                    assignment,
                    context,
                    member: request.member.as_deref(),
                },
            ) {
                Ok(command) => command,
                Err(why) => return error(why),
            };
            let released = jobs()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .fence_target_open(&caller, open_operation);
            drop(released);
            *prepared = Some(command);
            job.target_accounted.store(true, Ordering::Release);
        }
        drop(prepared);
        let (_, start) = jobs()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reserve_ready(Arc::clone(&job));
        if start {
            let command = job
                .prepared_target_cleanup
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            let Some(command) = command else {
                job.finish(Err(registry::Error::Unavailable));
                return UNAVAILABLE.into();
            };
            let worker = Arc::clone(&job);
            let registry = Arc::clone(registry);
            if std::thread::Builder::new()
                .name("codex-target-open-release".into())
                .spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        registry.retire_target_open(command)?;
                        let mut value = envelope(
                            &worker.request,
                            &worker.caller,
                            epoch,
                            worker.request.member.as_deref(),
                        );
                        value
                            .as_object_mut()
                            .ok_or(registry::Error::Unavailable)?
                            .insert("state".into(), aterm_json::json!("closed"));
                        let Action::CloseTargetOpen {
                            open_operation,
                            assignment,
                            context,
                        } = &worker.request.action
                        else {
                            return Err(registry::Error::Denied);
                        };
                        let object = value.as_object_mut().ok_or(registry::Error::Unavailable)?;
                        object.insert("open_operation".into(), aterm_json::json!(open_operation));
                        object.insert("assignment".into(), aterm_json::json!(assignment));
                        object.insert("context".into(), aterm_json::json!(context));
                        frame("member-target-open-close", &value)
                            .ok_or(registry::Error::Unavailable)
                    }))
                    .unwrap_or(Err(registry::Error::Unavailable));
                    worker.finish(result);
                })
                .is_err()
            {
                job.finish(Err(registry::Error::Unavailable));
            }
        }
    }
    if !connection.is_live() || identify(ctx).as_ref() != Ok(&caller) {
        return error(registry::Error::Denied);
    }
    match cached(&job) {
        Some(Ok(reply)) => reply,
        Some(Err(why)) => error(why),
        None => frame(
            "member-waiting",
            &envelope(&request, &caller, epoch, request.member.as_deref()),
        )
        .unwrap_or_else(|| UNAVAILABLE.into()),
    }
}

fn response(
    request: &Request,
    caller: &Caller,
    epoch: &str,
    allocation: registry::MemberAllocation,
) -> Result<String, registry::Error> {
    let mut value = envelope(request, caller, epoch, Some(&allocation.member));
    value
        .as_object_mut()
        .ok_or(registry::Error::Unavailable)?
        .insert("member".into(), aterm_json::json!(allocation.member));
    frame("member-open-target", &value).ok_or(registry::Error::Unavailable)
}

fn preload_response(
    request: &Request,
    caller: &Caller,
    epoch: &str,
    preload: registry::Preload,
) -> Result<String, registry::Error> {
    let mut value = completed_envelope(request, caller, epoch, request.member.as_deref());
    let source = super::super::auth_control::source_value(
        &request.operation,
        session(caller, epoch),
        preload.source(),
    )
    .ok_or(registry::Error::Unavailable)?;
    let object = value.as_object_mut().ok_or(registry::Error::Unavailable)?;
    object.insert(
        "registration".into(),
        aterm_json::json!(preload.registration),
    );
    object.insert("source".into(), source);
    frame("member-preload-target", &value).ok_or(registry::Error::Unavailable)
}

enum Prepared {
    Pending(Box<registry::TargetConstruction>),
    Settled(String),
}

fn prepare(
    job: &Job,
    registry: &registry::Registry,
    peer: &registry::Peer,
    connection: &registry::Connection,
    epoch: &str,
) -> Result<(), registry::Error> {
    let mut prepared = job
        .prepared_target
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    {
        let progress = job.progress.lock().unwrap_or_else(|e| e.into_inner());
        match &*progress {
            Progress::Complete(_) => return Ok(()),
            Progress::Running { .. } => {
                return if job.target_accounted.load(Ordering::Acquire) {
                    Ok(())
                } else {
                    Err(registry::Error::Unavailable)
                };
            }
            Progress::Waiting => {}
        }
    }
    if prepared.is_some() {
        return Ok(());
    }
    let admission = match &job.request.action {
        Action::OpenTarget {
            assignment,
            context,
        } => registry
            .prepare_target_open(
                peer,
                connection,
                assignment,
                context,
                &job.request.operation,
            )
            .and_then(|result| match result {
                registry::TargetOpenPreparation::Pending(command) => Ok(Prepared::Pending(command)),
                registry::TargetOpenPreparation::Settled(allocation) => {
                    response(&job.request, &job.caller, epoch, allocation).map(Prepared::Settled)
                }
            }),
        Action::PreloadTarget {
            assignment,
            context,
        } => registry
            .prepare_target_preload_context(
                peer,
                connection,
                assignment,
                context,
                job.request
                    .member
                    .as_deref()
                    .ok_or(registry::Error::Denied)?,
                &job.request.operation,
                job.request.owned_preload_sequence()?,
            )
            .and_then(|result| match result {
                registry::TargetPreloadPreparation::Pending(command) => {
                    Ok(Prepared::Pending(Box::new(command)))
                }
                registry::TargetPreloadPreparation::Settled(preload) => {
                    preload_response(&job.request, &job.caller, epoch, preload)
                        .map(Prepared::Settled)
                }
            }),
        _ => return Err(registry::Error::Denied),
    };
    match admission {
        Ok(Prepared::Pending(command)) => {
            *prepared = Some(*command);
            job.target_accounted.store(true, Ordering::Release);
            Ok(())
        }
        Ok(Prepared::Settled(reply)) => {
            job.finish(Ok(reply));
            Ok(())
        }
        Err(why) => {
            if why != registry::Error::Busy {
                job.finish(Err(why));
            }
            Err(why)
        }
    }
}

pub(super) fn serve(
    ctx: &Arc<crate::SessionCtx>,
    master: i32,
    request: Request,
    registry: &Arc<registry::Registry>,
    id: u64,
) -> String {
    let (caller, peer) = match claim(ctx, master, registry) {
        Ok(value) => value,
        Err(why) => return error(why),
    };
    let Some(epoch) = host_epoch() else {
        return UNAVAILABLE.into();
    };
    let connection = match registry.connection(id) {
        Ok(value) => value,
        Err(why) => return error(why),
    };
    let (job, _) = match admit_job(id, &caller, &request) {
        Ok(value) => value,
        Err(AdmissionError::Changed) => return "ERR codex-auth request changed\n".into(),
        Err(AdmissionError::Stale) => return "ERR codex-auth stale\n".into(),
        Err(AdmissionError::Capacity) => return UNAVAILABLE.into(),
    };
    if let Err(why) = prepare(&job, registry, &peer, &connection, epoch) {
        // No Waiting until an actual native constructor has been retained.
        return error(why);
    }
    if cached(&job).is_none() {
        let (_, start) = jobs()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reserve_ready(Arc::clone(&job));
        if start {
            let command = job
                .prepared_target
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            let Some(command) = command else {
                job.finish(Err(registry::Error::Unavailable));
                return UNAVAILABLE.into();
            };
            let worker = Arc::clone(&job);
            let registry = Arc::clone(registry);
            if std::thread::Builder::new()
                .name("codex-target-open".into())
                .spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        match &worker.request.action {
                            Action::OpenTarget { .. } => {
                                registry.run_target_open(command).and_then(|allocation| {
                                    response(&worker.request, &worker.caller, epoch, allocation)
                                })
                            }
                            Action::PreloadTarget { .. } => {
                                let env = super::super::codex_account_env()
                                    .map_err(|_| registry::Error::Unavailable)?;
                                registry.preload_target(&env, command).and_then(|preload| {
                                    preload_response(
                                        &worker.request,
                                        &worker.caller,
                                        epoch,
                                        preload,
                                    )
                                })
                            }
                            _ => Err(registry::Error::Denied),
                        }
                    }))
                    .unwrap_or(Err(registry::Error::Unavailable));
                    worker.finish(result);
                })
                .is_err()
            {
                // Native still owns the accepted OPEN and its original Scope.
                job.finish(Err(registry::Error::Unavailable));
            }
        }
    }
    if !connection.is_live()
        || !claim(ctx, master, registry).is_ok_and(|(fresh, _)| fresh == caller)
    {
        return "ERR codex-auth denied\n".into();
    }
    match cached(&job) {
        Some(Ok(reply)) => reply,
        Some(Err(why)) => error(why),
        None => frame(
            "member-waiting",
            &envelope(&request, &caller, epoch, request.member.as_deref()),
        )
        .unwrap_or_else(|| UNAVAILABLE.into()),
    }
}
