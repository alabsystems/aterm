// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! SDK last-use cleanup uses a dedicated authenticated connection. It is not
//! generic foreground work and cannot turn a registration into source access.
use super::use_control::{cached, error};
use super::*;
pub(super) fn serve(
    ctx: &Arc<crate::SessionCtx>,
    request: Request,
    registry: &Arc<registry::Registry>,
    id: u64,
) -> String {
    serve_identified(request, registry, id, || identify(ctx))
}
pub(super) fn serve_identified(
    request: Request,
    registry: &Arc<registry::Registry>,
    id: u64,
    observe: impl Fn() -> Result<Caller, registry::Error>,
) -> String {
    let caller = match observe() {
        Ok(caller) => caller,
        Err(why) => return error(why),
    };
    let Some(epoch) = host_epoch() else {
        return UNAVAILABLE.into();
    };
    let cleanup = match registry.connection(id) {
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
        let Action::Retire { registration } = &request.action else {
            return error(registry::Error::Denied);
        };
        let result = registry
            .retire_nonpreloaded_member(
                &caller.caller,
                &cleanup,
                request.member.as_deref().unwrap_or(""),
                registration.as_deref(),
            )
            .and_then(|()| {
                frame(
                    "member-retire",
                    &envelope(&request, &caller, epoch, request.member.as_deref()),
                )
                .ok_or(registry::Error::Unavailable)
            });
        job.finish(result);
    }
    if observe().as_ref() != Ok(&caller) || !cleanup.is_live() {
        return error(registry::Error::Denied);
    }
    match cached(&job) {
        Some(Ok(line)) => line,
        Some(Err(why)) => error(why),
        None => frame(
            "member-waiting",
            &envelope(&request, &caller, epoch, request.member.as_deref()),
        )
        .unwrap_or_else(|| UNAVAILABLE.into()),
    }
}
