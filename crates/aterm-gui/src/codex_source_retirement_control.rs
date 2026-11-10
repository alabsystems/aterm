// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Owned member cleanup uses exact native release custody. It never enters
//! ordinary work or upgrades a foreground identity to source authority.
use super::use_control::{cached, error};
use super::*;

fn reply(job: &Job, epoch: &str) -> Result<String, registry::Error> {
    frame(
        "member-retire-owned",
        &completed_envelope(
            &job.request,
            &job.caller,
            epoch,
            job.request.member.as_deref(),
        ),
    )
    .ok_or(registry::Error::Unavailable)
}

pub(super) fn serve(
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
        Ok(connection) => connection,
        Err(why) => return error(why),
    };
    let (job, _) = match admit_job(id, &caller, &request) {
        Ok(job) => job,
        Err(AdmissionError::Changed) => return "ERR codex-auth request changed\n".into(),
        Err(AdmissionError::Stale) => return "ERR codex-auth stale\n".into(),
        Err(AdmissionError::Capacity) => return UNAVAILABLE.into(),
    };
    if cached(&job).is_none() {
        let mut prepared = job
            .prepared_member_retirement
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let running = matches!(
            *job.progress.lock().unwrap_or_else(|e| e.into_inner()),
            Progress::Running { .. }
        );
        if prepared.is_none() && !running {
            let Action::RetireOwned {
                preload_operation,
                registration,
            } = &request.action
            else {
                return error(registry::Error::Denied);
            };
            let Some(member) = request.member.as_deref() else {
                return error(registry::Error::Denied);
            };
            let sequence = match request.owned_preload_sequence() {
                Ok(sequence) => sequence,
                Err(why) => return error(why),
            };
            match registry.prepare_member_retirement(
                &caller.caller,
                &connection,
                registry::MemberRetirementRequest {
                    operation: &request.operation,
                    member,
                    preload_operation,
                    preload_sequence: sequence,
                    registration: registration.as_deref(),
                },
            ) {
                Ok(registry::MemberRetirementPreparation::Closed) => job.finish(reply(&job, epoch)),
                Ok(registry::MemberRetirementPreparation::Pending(command)) => {
                    // Native fencing precedes taking any command. Running
                    // workers retain actual native custody and block Closed.
                    let released = jobs()
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .fence_member_preload(&caller, member, preload_operation);
                    drop(released);
                    *prepared = Some(command);
                    job.target_accounted.store(true, Ordering::Release);
                }
                Err(why) => return error(why),
            }
        }
        drop(prepared);
        if cached(&job).is_none() {
            let (_, start) = jobs()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .reserve_ready(Arc::clone(&job));
            if start {
                let command = job
                    .prepared_member_retirement
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
                    .name("codex-member-retire-owned".into())
                    .spawn(move || {
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            if !registry.retire_member_owned(command)? {
                                return Err(registry::Error::Busy);
                            }
                            reply(&worker, epoch)
                        }))
                        .unwrap_or(Err(registry::Error::Unavailable));
                        worker.finish(result);
                    })
                    .is_err()
                {
                    // The native exact fence and its release turn remain
                    // retained. A fresh poll resumes this same body.
                    job.finish(Err(registry::Error::Busy));
                }
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
